//! What one request may take from the resident compiler: a CPU budget, and
//! no more than [`PATIENCE`] of any other client's time.
//!
//! A request runs on the serving thread, since it takes the process's
//! standard descriptors, directory, environment and umask, so two requests
//! never run at once and a running one cannot be interrupted part way (an
//! engine such as LLVM has no cancellation point). For each request the
//! server starts a watcher thread that never waits on anything the request
//! holds:
//!
//! - It accepts the connections that arrive meanwhile and reads their
//!   requests. One that has waited [`PATIENCE`] is refused, so its client
//!   compiles in its own process: a long request never queues a short one
//!   for longer than that. A `serve --stop` is answered at once and the
//!   server stops after the request. The rest go back to the serving thread,
//!   in arrival order, when the request ends.
//! - It reads the process's CPU time (every thread's, user and system). Once
//!   the request has used its budget (the client's [`CPU_VARIABLE`], whole
//!   seconds, by default [`DEFAULT_CPU_SECONDS`]) and has not yet been
//!   answered, the watcher answers its client that the request passed the
//!   budget (the client exits with the memory limit's status,
//!   [`hirsel_guard::EXIT_STATUS`]), refuses the connections it holds, and
//!   recycles the server: it executes the server's own image again in the
//!   same process, which keeps its listening socket, its lock and its
//!   launched clients' connections and scratch directories
//!   ([`INHERIT_VARIABLE`]) and drops everything the request held. Clients
//!   still waiting to be accepted are served by the new image; the server's
//!   remembered derivations are dropped, as after a panic.
//!
//! One handoff decides who answers a request ([`send`]): the serving thread
//! answers under a lock that the watcher takes before it answers, and the
//! watcher recycles only a request not yet answered. The budget counts CPU
//! time, not wall time, so a request that waits (on its client, a test
//! launch or a linker) is not refused for it; [`PATIENCE`] keeps the other
//! clients going meanwhile. The budget is the resident compiler's alone: a
//! one-shot compile runs in the client's own process, under its terminal.
use super::{Request, Toolchain, read_message, refuse, write_message};
use crate::store::Scratch;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

/// The client's variable that sets its request's CPU budget, in seconds.
pub(crate) const CPU_VARIABLE: &str = "HIRSEL_RESIDENT_CPU_SECONDS";
/// A request's CPU budget when its client sets none.
pub(crate) const DEFAULT_CPU_SECONDS: u64 = 300;
/// How long a client waits behind a running request before it is refused
/// and compiles in its own process.
pub(crate) const PATIENCE: Duration = Duration::from_secs(2);
/// How the server hands its socket, lock and launched clients to its next
/// image.
const INHERIT_VARIABLE: &str = "HIRSEL_RESIDENT_INHERIT";
/// How often the watcher looks at the request's CPU time.
const TICK: Duration = Duration::from_millis(50);
/// How long the watcher waits for a waiting client's request or a client to
/// take an answer.
const IO_PATIENCE: Duration = Duration::from_secs(1);

/// The CPU budget `value` (the client's [`CPU_VARIABLE`]) sets.
pub(crate) fn cpu_seconds(value: Option<&[u8]>) -> Result<u64, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_CPU_SECONDS);
    };
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&seconds| seconds > 0)
        .ok_or_else(|| format!("{CPU_VARIABLE} expects a positive whole number of seconds"))
}

/// The budget of a request whose client's environment is `env`.
pub(super) fn request_cpu_seconds(env: &[(Vec<u8>, Vec<u8>)]) -> u64 {
    let value = env
        .iter()
        .find(|(name, _)| name == CPU_VARIABLE.as_bytes())
        .map(|(_, value)| value.as_slice());
    // The client checked the value before it sent the request.
    cpu_seconds(value).unwrap_or(DEFAULT_CPU_SECONDS)
}

/// What the client of a request that passed its budget is told.
pub(crate) fn exceeded(seconds: u64) -> String {
    format!(
        "the request passed its budget of {seconds} CPU seconds and was stopped; \
         set {CPU_VARIABLE} to raise it, or compile without the resident compiler"
    )
}

/// Who has answered the request being served.
struct Turn {
    /// Its last answer is sent: the request only releases its state now.
    answered: bool,
}
static TURN: Mutex<Turn> = Mutex::new(Turn { answered: false });

fn turn() -> MutexGuard<'static, Turn> {
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Send an answer of the request being served; `last` when no other follows.
pub(super) fn send(stream: &mut UnixStream, bytes: &[u8], last: bool) -> std::io::Result<()> {
    let mut turn = turn();
    let sent = write_message(stream, bytes, &[]);
    turn.answered |= last;
    sent
}

/// A connection that arrived while a request ran, with its request.
pub(super) struct Held {
    pub since: Instant,
    pub stream: UnixStream,
    pub bytes: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

/// What a watcher hands back when its request ends.
pub(super) struct Watched {
    /// The connections still waiting, in arrival order.
    pub held: VecDeque<Held>,
    /// A client asked the server to stop.
    pub stopped: bool,
}

/// The watcher of the request being served.
pub(super) struct Watch {
    end: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<Watched>,
}
impl Watch {
    /// Watch the request being served, whose client is `client`, with the
    /// connections already waiting in `held`.
    pub(super) fn start(
        listener: RawFd,
        held: VecDeque<Held>,
        client: Option<UnixStream>,
        seconds: u64,
        recycle: Recycle,
    ) -> std::io::Result<Self> {
        turn().answered = false;
        let (end, ended) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("resident-watch".into())
            .spawn(move || watch(listener, held, client, seconds, recycle, ended))?;
        Ok(Self { end, thread })
    }

    /// The request ended: what the watcher holds.
    pub(super) fn end(self) -> Watched {
        let _ = self.end.send(());
        self.thread.join().expect("the watcher never panics")
    }
}

fn watch(
    listener: RawFd,
    mut held: VecDeque<Held>,
    mut client: Option<UnixStream>,
    seconds: u64,
    recycle: Recycle,
    ended: mpsc::Receiver<()>,
) -> Watched {
    let budget = Duration::from_secs(seconds);
    let start = cpu_time();
    let mut stopped = false;
    let mut counting = true;
    loop {
        if !matches!(ended.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Watched { held, stopped };
        }
        if counting && cpu_time().saturating_sub(start) >= budget {
            let turn = turn();
            if turn.answered {
                counting = false;
            } else {
                recycle.exceed(client.as_mut(), held, turn);
            }
        }
        while held.front().is_some_and(|h| h.since.elapsed() >= PATIENCE) {
            let mut late = held.pop_front().expect("a front");
            let _ = refuse(&mut late.stream, "the resident compiler is busy");
        }
        let mut poll = libc::pollfd {
            fd: listener,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll reads and writes the one entry it is given.
        if unsafe { libc::poll(&mut poll, 1, TICK.as_millis() as i32) } <= 0 {
            continue;
        }
        // SAFETY: the listener outlives the watcher, which only borrows it.
        let listening = std::mem::ManuallyDrop::new(unsafe { UnixListener::from_raw_fd(listener) });
        let Ok((mut stream, _)) = listening.accept() else {
            continue;
        };
        let since = Instant::now();
        let _ = stream.set_read_timeout(Some(IO_PATIENCE));
        let Ok((bytes, fds)) = read_message(&mut stream) else {
            continue;
        };
        let _ = stream.set_read_timeout(None);
        if serde_json::from_slice::<Request>(&bytes).is_ok_and(|request| request.stop) {
            let _ = refuse(&mut stream, "stopped");
            stopped = true;
            continue;
        }
        held.push_back(Held {
            since,
            stream,
            bytes,
            fds,
        });
    }
}

/// The process's CPU time so far, every thread's, user and system.
fn cpu_time() -> Duration {
    // SAFETY: getrusage fills the struct it is given.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let time = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// What a server hands its next image.
#[derive(Serialize, Deserialize)]
struct Inherit {
    toolchain: Toolchain,
    listener: RawFd,
    lock: RawFd,
    /// Each launched client's connection, with its scratch lock and path.
    launched: Vec<(RawFd, Option<(RawFd, Vec<u8>)>)>,
}

/// The server's next image, made ready before its request runs so that
/// recycling only answers and executes.
pub(super) struct Recycle {
    executable: CString,
    argv: Vec<CString>,
    env: Vec<CString>,
    /// The descriptors the next image keeps.
    keep: Vec<RawFd>,
    /// The server's own standard descriptors.
    own: [RawFd; 3],
    directory: CString,
    exceeded: Vec<u8>,
}

fn c_bytes(bytes: Vec<u8>) -> std::io::Result<CString> {
    CString::new(bytes).map_err(std::io::Error::other)
}

impl Recycle {
    pub(super) fn prepare(
        own: &super::Own,
        toolchain: Toolchain,
        listener: RawFd,
        lock: RawFd,
        launched: &[(UnixStream, Option<Scratch>)],
        seconds: u64,
    ) -> std::io::Result<Self> {
        let inherit = Inherit {
            toolchain,
            listener,
            lock,
            launched: launched
                .iter()
                .map(|(stream, scratch)| {
                    let scratch = scratch.as_ref().map(|scratch| {
                        let path = scratch.path().as_os_str().as_bytes().to_vec();
                        (scratch.lock_fd(), path)
                    });
                    (stream.as_raw_fd(), scratch)
                })
                .collect(),
        };
        let mut keep = vec![listener, lock];
        for (stream, scratch) in &inherit.launched {
            keep.push(*stream);
            keep.extend(scratch.as_ref().map(|(lock, _)| *lock));
        }
        let mut env = Vec::with_capacity(own.env.len() + 1);
        for (name, value) in &own.env {
            let mut pair = name.clone();
            pair.push(b'=');
            pair.extend_from_slice(value);
            env.push(c_bytes(pair)?);
        }
        env.push(c_bytes(
            format!("{INHERIT_VARIABLE}={}", serde_json::to_string(&inherit)?).into_bytes(),
        )?);
        let answer = super::Answer::Exceeded(exceeded(seconds));
        Ok(Self {
            executable: c_bytes(std::env::current_exe()?.into_os_string().into_vec())?,
            argv: std::env::args_os()
                .map(|arg| c_bytes(arg.into_vec()))
                .collect::<std::io::Result<_>>()?,
            env,
            keep,
            own: own.fds.each_ref().map(|fd| fd.as_raw_fd()),
            directory: c_bytes(own.directory.as_os_str().as_bytes().to_vec())?,
            exceeded: serde_json::to_vec(&answer)?,
        })
    }

    /// The request passed its budget unanswered: answer its client, refuse
    /// the waiting connections, and execute the next image. Holds the turn,
    /// so the request cannot answer after this.
    fn exceed(
        &self,
        client: Option<&mut UnixStream>,
        held: VecDeque<Held>,
        turn: MutexGuard<'_, Turn>,
    ) -> ! {
        if let Some(client) = client {
            let _ = client.set_write_timeout(Some(IO_PATIENCE));
            let _ = write_message(client, &self.exceeded, &[]);
        }
        for mut waiting in held {
            let _ = waiting.stream.set_write_timeout(Some(IO_PATIENCE));
            let _ = refuse(&mut waiting.stream, "the resident compiler is recycling");
        }
        let argv: Vec<*const libc::c_char> = self
            .argv
            .iter()
            .map(|a| a.as_ptr())
            .chain([std::ptr::null()])
            .collect();
        let env: Vec<*const libc::c_char> = self
            .env
            .iter()
            .map(|e| e.as_ptr())
            .chain([std::ptr::null()])
            .collect();
        // SAFETY: each call takes descriptors this process holds and
        // NUL-terminated strings and arrays that outlive it; execve replaces
        // every thread, the request's included.
        unsafe {
            for &fd in &self.keep {
                libc::fcntl(fd, libc::F_SETFD, 0);
            }
            for (fd, &own) in self.own.iter().enumerate() {
                libc::dup2(own, fd as RawFd);
            }
            libc::chdir(self.directory.as_ptr());
            libc::execve(self.executable.as_ptr(), argv.as_ptr(), env.as_ptr());
            let failed =
                b"hirsel serve: a request passed its budget and the server could not restart\n";
            libc::write(2, failed.as_ptr().cast(), failed.len());
        }
        drop(turn);
        // SAFETY: _exit ends the process without running its exit handlers.
        unsafe { libc::_exit(hirsel_guard::EXIT_STATUS) }
    }
}

/// What a previous image of this server handed over.
pub(super) struct Inherited {
    pub toolchain: Toolchain,
    pub listener: UnixListener,
    pub lock: std::fs::File,
    pub launched: Vec<(UnixStream, Option<Scratch>)>,
}

/// Take what a previous image of this server left, if this image is one's
/// successor, and remove its variable from the environment.
pub(super) fn inherited() -> Option<Inherited> {
    let value = std::env::var_os(INHERIT_VARIABLE)?;
    // SAFETY: the server reads and writes the environment from its serving
    // thread only, and serves nothing yet.
    unsafe { std::env::remove_var(INHERIT_VARIABLE) };
    let inherit: Inherit = serde_json::from_slice(value.as_bytes()).ok()?;
    // SAFETY: the previous image left each descriptor open for this one,
    // which takes each once; they close on exec again.
    let own = |fd: RawFd| unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        OwnedFd::from_raw_fd(fd)
    };
    Some(Inherited {
        toolchain: inherit.toolchain,
        listener: UnixListener::from(own(inherit.listener)),
        lock: std::fs::File::from(own(inherit.lock)),
        launched: inherit
            .launched
            .into_iter()
            .map(|(stream, scratch)| {
                let scratch = scratch.map(|(lock, path)| {
                    let path = PathBuf::from(std::ffi::OsString::from_vec(path));
                    Scratch::inherited(path, own(lock))
                });
                (UnixStream::from(own(stream)), scratch)
            })
            .collect(),
    })
}

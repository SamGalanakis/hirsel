//! The resident compiler (`hirsel serve`): one long-lived process per user
//! and toolchain that serves `build`, `run` and `test` requests from every
//! directory, so the state a request derives from unchanged inputs stays in
//! memory between edits and is shared by working copies of one source.
//!
//! A request runs in the server exactly as it runs one-shot: for the request
//! the server takes the client's standard descriptors, working directory,
//! environment and umask, calls the same [`crate::request`] path with the
//! client's arguments, and answers with the request's [`Outcome`]; then it
//! restores its own. Only the variables a process reads once, when it starts
//! ([`STARTUP_VARIABLES`]), cannot be taken per request: a client whose
//! digest of them differs from the server's, or that runs another toolchain,
//! is refused and runs the request itself. A refusal never stops the server.
//!
//! The server listens on `<toolchain>.sock` in the user's runtime directory
//! (`$XDG_RUNTIME_DIR/hirsel`, else `/tmp/hirsel-<uid>`), named by the content
//! identity of its compiler executable, and holds `<toolchain>.lock` there
//! while it runs. It re-reads its executable's identity on each request and
//! exits once that changed. A request that panics fails alone: its client is
//! answered 101, and the server forgets every working set, since what the
//! request was deriving may be half made. The server returns its free pages
//! to the system after a second without requests and exits after
//! [`IDLE_EXIT`] without any.
//!
//! A `run` request's program runs in the client, under the client's
//! terminal and signals; the server keeps the program's scratch directory
//! until that client hangs up. A `test` request is a session on its
//! connection: the server answers with the session's plan, the client steps
//! each unit and runs every launch the server hands back (under the client's
//! directory and environment, which write the pass cache), and the server
//! folds coverage and the law lock when the client finishes. Hanging up drops
//! the session.
use crate::request::Outcome;
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::sync::atomic::{AtomicBool, Ordering};

mod memo;
pub(crate) use memo::{
    CONSTANTS, DEBUG_POSITIONS, MATERIALISED, Memo, OBJECT_RECORDS, RECIPE_GROUPS, RECIPE_REACH,
    REFERENCED, RESOLVED, RUNTIME_SOURCES, WorkingMemo, parse_file, source_hash, symbol_suffix,
};

const PROTOCOL: u32 = 2;
/// Cold Place builds peaked at 1,168 MiB. Add 512 MiB for retained sets
/// and round up to a 256 MiB boundary.
pub(crate) const DEFAULT_MAX_MEMORY: u64 = 1792 << 20;
/// How long a server without requests or launched clients stays: sccache's.
const IDLE_EXIT: Duration = Duration::from_secs(600);
/// How long a server waits without requests before returning free pages.
const COLLECT_AFTER: Duration = Duration::from_secs(1);
/// The variables a compiler reads once, when it starts, so that a server
/// keeps its own: the memory limit and the guard's oracle switch (read at the
/// first allocation), and the LLVM engine's facts switch, partition bound and
/// expected build id (read at their first use). Every other variable a
/// request reads is its client's.
const STARTUP_VARIABLES: [&str; 5] = [
    "HIRSEL_MEMORY_LIMIT",
    "HIRSEL_GUARD_ORACLE",
    hirsel_llvm_lazy::core::NO_FACTS_VARIABLE,
    hirsel_llvm_lazy::core::PARTITION_VARIABLE,
    "HIRSEL_LLVM_EXPERIMENT_BUILD_ID",
];

static SERVING: AtomicBool = AtomicBool::new(false);
thread_local! {
    /// The connection of the request being served, until it is answered.
    static CLIENT: std::cell::RefCell<Option<UnixStream>> = const { std::cell::RefCell::new(None) };
}

/// Whether this process is a resident compiler serving requests.
pub(crate) fn serving() -> bool {
    SERVING.load(Ordering::Relaxed)
}

/// The toolchain a process runs, by content: its compiler executable's link
/// identity ([`crate::reuse::executable_identity`]), read once at load. A
/// server answers only clients of the same file, so a rebuilt compiler never
/// talks to its predecessor's server even when both share a compiler identity.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
struct Toolchain(ContentHash);
impl Toolchain {
    fn loaded() -> io::Result<Self> {
        static LOADED: std::sync::LazyLock<io::Result<ContentHash>> =
            std::sync::LazyLock::new(|| {
                hirsel_core_front::cost_scope!("non-check", "compiler executable identity");
                // The linker has already identified this executable. Read that
                // immutable note, rather than hashing the whole compiler.
                crate::reuse::executable_identity(&mut std::fs::File::open(
                    crate::host::current_image()?,
                )?)
            });
        LOADED
            .as_ref()
            .copied()
            .map(Self)
            .map_err(|e| io::Error::other(e.to_string()))
    }
    /// The toolchain the executable's file holds now: a rebuilt or replaced
    /// one has another, and a deleted one none.
    fn on_disk() -> io::Result<Self> {
        crate::reuse::executable_identity(&mut std::fs::File::open(std::env::current_exe()?)?)
            .map(Self)
    }
    /// The name of its server's `suffix` file: 128 bits of the identity.
    fn file(self, suffix: &str) -> String {
        format!("{}.{suffix}", &crate::request::hex(self.0)[..32])
    }
}

/// The user's directory of resident compiler sockets and locks, made private
/// to the user; one that is not is refused.
fn runtime_directory() -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let directory = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("hirsel"),
        None => PathBuf::from(format!("/tmp/hirsel-{uid}")),
    };
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
    {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(&directory)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "{} is not a directory private to this user",
            directory.display()
        )));
    }
    Ok(directory)
}

/// The path a law's stand-in server binds for this toolchain, under the
/// runtime directory `runtime` (what `XDG_RUNTIME_DIR` names).
#[cfg(test)]
pub(crate) fn socket_under(runtime: &Path) -> PathBuf {
    runtime
        .join("hirsel")
        .join(Toolchain::loaded().unwrap().file("sock"))
}

/// The process's whole environment, as a request carries it.
fn environment() -> Vec<(Vec<u8>, Vec<u8>)> {
    std::env::vars_os()
        .map(|(name, value)| (name.into_vec(), value.into_vec()))
        .collect()
}

/// The digest of `env`'s [`STARTUP_VARIABLES`], set or unset.
fn startup_digest(env: &[(Vec<u8>, Vec<u8>)]) -> ContentHash {
    let mut parts: Vec<&[u8]> = Vec::new();
    for name in STARTUP_VARIABLES {
        parts.push(name.as_bytes());
        match env.iter().find(|(n, _)| n == name.as_bytes()) {
            Some((_, value)) => parts.extend([&[1u8][..], value]),
            None => parts.push(&[0]),
        }
    }
    ContentHash::of_parts("hirsel.resident-startup.v1", parts)
}

/// This process's umask.
fn umask() -> u32 {
    // SAFETY: umask only swaps the process's mask; the old one is restored.
    unsafe {
        let mask = libc::umask(0o022);
        libc::umask(mask);
        mask as u32
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    protocol: u32,
    toolchain: Toolchain,
    env: Vec<(Vec<u8>, Vec<u8>)>,
    cwd: Vec<u8>,
    umask: u32,
    args: Vec<String>,
    /// Stop the server instead (`hirsel serve --stop`).
    stop: bool,
}
#[derive(Serialize, Deserialize)]
enum Answer {
    Exit(i32),
    Launch {
        argv: Vec<Vec<u8>>,
        env: Vec<(Vec<u8>, Vec<u8>)>,
    },
    Refused(String),
    /// A `test` request's session: how its units' launches may overlap.
    Session(crate::test_run::Plan),
    /// A unit's next launch, or None once it ended.
    Test(Option<crate::test_run::LaunchSpec>),
}

/// A session's client messages after its request.
#[derive(Serialize, Deserialize)]
enum Step {
    /// Step `unit` with the run of its last launch (None to start it).
    Unit {
        unit: usize,
        ran: Option<Result<crate::test_run::Ran, String>>,
    },
    /// Every unit ended: fold and answer the exit status.
    Finish,
}

fn write_message(stream: &mut UnixStream, bytes: &[u8], fds: &[RawFd]) -> io::Result<()> {
    let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(bytes);
    if fds.is_empty() {
        return stream.write_all(&framed);
    }
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut iov = libc::iovec {
        iov_base: framed.as_mut_ptr().cast(),
        iov_len: framed.len(),
    };
    // SAFETY: the header points at `iov` and `control`, which outlive the
    // call; the control buffer has CMSG_SPACE for the descriptors written.
    let sent = unsafe {
        let mut header: libc::msghdr = std::mem::zeroed();
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
        libc::sendmsg(stream.as_raw_fd(), &header, libc::MSG_NOSIGNAL)
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    stream.write_all(&framed[sent as usize..])
}

/// One message, with the descriptors that came with its first bytes.
fn read_message(stream: &mut UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut length = [0u8; 4];
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(4 * 8) } as usize];
    let mut iov = libc::iovec {
        iov_base: length.as_mut_ptr().cast(),
        iov_len: length.len(),
    };
    let mut fds = Vec::new();
    // SAFETY: as in write_message; each descriptor received is owned once.
    let got = unsafe {
        let mut header: libc::msghdr = std::mem::zeroed();
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = control.len() as _;
        let got = crate::host::receive(stream.as_raw_fd(), &mut header);
        if got > 0 {
            let mut cmsg = libc::CMSG_FIRSTHDR(&header);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                    let n = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    for i in 0..n {
                        fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&header, cmsg);
            }
        }
        got
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    if got == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    stream.read_exact(&mut length[got as usize..])?;
    let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
    stream.read_exact(&mut bytes)?;
    Ok((bytes, fds))
}

/// A path to `name` in `directory` that a socket address can hold, however
/// long the directory's own path is: through the descriptor `held` keeps open.
fn short_path(directory: &Path, name: &str) -> io::Result<(PathBuf, std::fs::File)> {
    let held = std::fs::File::open(directory)?;
    Ok((crate::host::fd_path(&held)?.join(name), held))
}

struct Route {
    resident: bool,
    retryable: bool,
    input: memo::Input,
}

/// Route a request to the resident compiler of its toolchain.
fn route(args: &[String]) -> Option<Route> {
    use crate::cli::OptionId as Opt;
    let crate::cli::Action::Invoke(invocation) = crate::cli::parse(args.iter().cloned()).ok()?
    else {
        return None;
    };
    if !matches!(invocation.command.name, "build" | "run" | "test") {
        return None;
    }
    let mut resident = false;
    // A test session's launches may already have had effects.
    let mut retryable = invocation.command.name != "test";
    let mut manifest = None;
    let mut recipe = None;
    let mut entry = None;
    for (option, value) in &invocation.options {
        match option {
            Opt::Resident => resident = true,
            Opt::Inputs => manifest = value.as_deref().map(PathBuf::from),
            Opt::Recipe => recipe = value.clone(),
            Opt::Entry => entry = value.clone(),
            // Ordinary compiler stores publish by content. A durable code
            // store also refreshes publication times. A custom linker can
            // have its own effects. Never repeat either after a lost reply.
            Opt::CodeStore | Opt::Cc => retryable = false,
            Opt::Grant
                if invocation.command.name == "run"
                    && value.as_deref().is_some_and(|v| v.starts_with("Foreign=")) =>
            {
                retryable = false;
            }
            _ => {}
        }
    }
    let input = if let Some(recipe) = recipe {
        memo::Input::Recipe(recipe)
    } else if let Some(manifest) = manifest {
        memo::Input::Manifest(manifest, entry)
    } else {
        let package = PathBuf::from(invocation.positional.as_deref().unwrap_or("."));
        memo::Input::Package(package, entry)
    };
    Some(Route {
        resident,
        retryable,
        input,
    })
}

/// Submit a `build`, `run` or `test` request to the resident compiler of its
/// toolchain, starting one first under `--resident`. The request's exit
/// status when a server answered it; None when the caller runs it itself.
pub fn submit(args: &[String]) -> Option<i32> {
    let route = route(args)?;
    match try_submit(&route, args) {
        Ok(answer) => answer,
        Err(error) => {
            eprintln!("hirsel: resident compiler: {error}");
            Some(2)
        }
    }
}

fn connect(toolchain: Toolchain) -> io::Result<UnixStream> {
    let (path, _held) = short_path(&runtime_directory()?, &toolchain.file("sock"))?;
    UnixStream::connect(path)
}

fn try_submit(route: &Route, args: &[String]) -> io::Result<Option<i32>> {
    let (resident, retryable) = (route.resident, route.retryable);
    let toolchain = Toolchain::loaded()?;
    let mut stream = match connect(toolchain) {
        Ok(stream) => stream,
        Err(_) if resident => start(toolchain)?,
        Err(_) => return Ok(None),
    };
    let request = Request {
        protocol: PROTOCOL,
        toolchain,
        env: environment(),
        cwd: std::env::current_dir()?.into_os_string().into_vec(),
        umask: umask(),
        args: args.to_vec(),
        stop: false,
    };
    let request = serde_json::to_vec(&request)?;
    let mut retried = false;
    // A killed attempt can have printed a complete JSON result before it
    // sent its reply. Only forward stdout from the attempt that answered.
    let mut output = (resident && retryable)
        .then(tempfile::tempfile)
        .transpose()?;
    loop {
        let stdout = if let Some(output) = &mut output {
            output.set_len(0)?;
            output.rewind()?;
            output.as_raw_fd()
        } else {
            1
        };
        let (answer, _) = match write_message(&mut stream, &request, &[0, stdout, 2])
            .and_then(|()| read_message(&mut stream))
        {
            Ok(answer) => answer,
            Err(error) if connection_lost(&error) && retryable && resident && !retried => {
                drop(stream);
                stream = start(toolchain)?;
                retried = true;
                continue;
            }
            Err(error) if connection_lost(&error) && retryable && !resident => return Ok(None),
            Err(error) => {
                return Err(io::Error::other(format!(
                    "the server ended without an answer: {error}"
                )));
            }
        };
        let answer = serde_json::from_slice::<Answer>(&answer)?;
        if !matches!(answer, Answer::Refused(_))
            && let Some(output) = &mut output
        {
            output.rewind()?;
            let mut stdout = io::stdout().lock();
            io::copy(output, &mut stdout)?;
            stdout.flush()?;
        }
        return match answer {
            Answer::Exit(code) => Ok(Some(code)),
            Answer::Launch { argv, env } => {
                let argv: Vec<OsString> = argv.into_iter().map(OsString::from_vec).collect();
                let env: Vec<(OsString, OsString)> = env
                    .into_iter()
                    .map(|(n, v)| (OsString::from_vec(n), OsString::from_vec(v)))
                    .collect();
                let code = crate::request::Launch::execute(&argv, &env)
                    .map_err(|error| io::Error::other(error.to_string()));
                drop(stream);
                code.map(Some)
            }
            // The server serves other clients; this one compiles itself.
            Answer::Refused(_) => Ok(None),
            // A test request is not retryable, so its output is not held.
            Answer::Session(plan) => drive_session(&mut stream, &plan).map(Some),
            Answer::Test(_) => Err(io::Error::other("the server answered out of turn")),
        };
    }
}

/// A test session's client: run each launch the server hands back here,
/// then ask the server to finish. The request's exit status.
fn drive_session(stream: &mut UnixStream, plan: &crate::test_run::Plan) -> io::Result<i32> {
    struct Remote<'a> {
        stream: &'a mut UnixStream,
        /// The status the server ended the session with early.
        ended: Option<i32>,
    }
    impl Remote<'_> {
        fn send(&mut self, step: &Step) -> io::Result<Answer> {
            write_message(self.stream, &serde_json::to_vec(step)?, &[])?;
            let (answer, _) = read_message(self.stream)?;
            Ok(serde_json::from_slice(&answer)?)
        }
    }
    impl crate::test_run::Steps for Remote<'_> {
        fn step(
            &mut self,
            unit: usize,
            ran: Option<Result<crate::test_run::Ran, String>>,
        ) -> io::Result<Option<crate::test_run::LaunchSpec>> {
            match self.send(&Step::Unit { unit, ran })? {
                Answer::Test(launch) => Ok(launch),
                Answer::Exit(code) => {
                    self.ended = Some(code);
                    Err(io::Error::other("the server ended the session"))
                }
                _ => Err(io::Error::other("the server answered out of turn")),
            }
        }
    }
    let mut remote = Remote {
        stream,
        ended: None,
    };
    let lost =
        |error: io::Error| io::Error::other(format!("the server ended without an answer: {error}"));
    if let Err(error) = crate::test_run::drive(plan, &mut remote) {
        return remote.ended.ok_or_else(|| lost(error));
    }
    match remote.send(&Step::Finish).map_err(lost)? {
        Answer::Exit(code) => Ok(code),
        _ => Err(io::Error::other("the server answered out of turn")),
    }
}

fn connection_lost(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
    )
}

/// Wait until no server of `toolchain` holds its lock.
fn wait_gone(toolchain: Toolchain) {
    for _ in 0..5000 {
        match lock(toolchain) {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(_) => return,
        }
    }
}

/// Start a server of `toolchain` and return the connection the request will
/// use. The server runs in `/`, so that it holds no client's directory.
fn start(toolchain: Toolchain) -> io::Result<UnixStream> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("serve")
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let mut child = command.spawn()?;
    for _ in 0..10_000 {
        if let Ok(stream) = connect(toolchain) {
            return Ok(stream);
        }
        if let Some(status) = child.try_wait()? {
            // Another client's server may have taken the lock first.
            if let Ok(stream) = connect(toolchain) {
                return Ok(stream);
            }
            if status.success() {
                // An exiting server can relinquish the lock before its
                // successor binds, or while our child is exiting.
                if lock(toolchain)?.is_some() {
                    child = command.spawn()?;
                }
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            return Err(io::Error::other(format!(
                "the resident compiler exited at start: {status}"
            )));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(io::Error::other("the resident compiler did not start"))
}

/// The server lock of `toolchain` when no other process holds it.
fn lock(toolchain: Toolchain) -> io::Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtime_directory()?.join(toolchain.file("lock")))?;
    // SAFETY: flock takes the descriptor `file` owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(Some(file))
}

/// Answer the request being served, once: flush its output to the client's
/// descriptors, then send the outcome. Called by the request itself before
/// its frames unwind (`request::conclude`), so the client goes on while the
/// server releases the request's state.
pub(crate) fn answer(outcome: &Outcome) {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    crate::meter::trace::finish();
    let Some(mut stream) = CLIENT.with_borrow_mut(Option::take) else {
        return;
    };
    let answer = match outcome {
        Outcome::Exit(code) => Answer::Exit(*code),
        Outcome::Test(_) => unreachable!("a session answers as its client steps it"),
        Outcome::Launch(launch) => Answer::Launch {
            argv: launch.argv.iter().map(|a| a.as_bytes().to_vec()).collect(),
            env: launch
                .env
                .iter()
                .map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
        },
    };
    let bytes = serde_json::to_vec(&answer).expect("an answer encodes");
    let _ = write_message(&mut stream, &bytes, &[]);
}

/// Options of `hirsel serve`.
pub(crate) struct Serve {
    max_memory: u64,
    stop: bool,
}
impl Serve {
    pub(crate) fn read(
        invocation: crate::cli::Invocation,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use crate::cli::OptionId as Opt;
        if invocation.positional.is_some() {
            return Err("serve takes no package: one server serves every directory".into());
        }
        let mut max_memory = DEFAULT_MAX_MEMORY;
        let mut stop = false;
        for (option, value) in invocation.options {
            match option {
                Opt::MaxMemory => {
                    max_memory = value
                        .unwrap_or_default()
                        .parse()
                        .map_err(|_| "--max-memory expects a byte count")?
                }
                Opt::Stop => stop = true,
                _ => unreachable!("serve takes --max-memory and --stop"),
            }
        }
        Ok(Self { max_memory, stop })
    }
}

/// `hirsel serve`: serve this toolchain's requests until stopped, idle, over
/// its memory with its caches dropped, or its executable replaced. The exit
/// status of the command.
pub(crate) fn run(serve: Serve) -> Result<i32, Box<dyn std::error::Error>> {
    let toolchain = Toolchain::loaded()?;
    if serve.stop {
        let Ok(mut stream) = connect(toolchain) else {
            return Ok(0);
        };
        let request = Request {
            protocol: PROTOCOL,
            toolchain,
            env: vec![],
            cwd: vec![],
            umask: 0,
            args: vec![],
            stop: true,
        };
        write_message(&mut stream, &serde_json::to_vec(&request)?, &[])?;
        let _ = read_message(&mut stream);
        wait_gone(toolchain);
        return Ok(0);
    }
    let Some(_lock) = lock(toolchain)? else {
        // Another server serves the toolchain.
        return Ok(0);
    };
    let (socket, _held) = short_path(&runtime_directory()?, &toolchain.file("sock"))?;
    match std::fs::remove_file(&socket) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = UnixListener::bind(&socket)?;
    SERVING.store(true, Ordering::Relaxed);
    let served = serve_requests(&listener, toolchain, serve.max_memory);
    SERVING.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_file(&socket);
    served
}

/// What a request takes from its client for its duration: the standard
/// descriptors, working directory, environment and umask. Holds the server's
/// own, which [`Own::restore`] puts back.
struct Own {
    fds: [OwnedFd; 3],
    directory: PathBuf,
    env: Vec<(Vec<u8>, Vec<u8>)>,
    umask: u32,
}
impl Own {
    fn save() -> io::Result<Self> {
        Ok(Self {
            fds: [0, 1, 2].map(|fd| {
                // SAFETY: dup of a standard descriptor this process holds.
                unsafe { OwnedFd::from_raw_fd(libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10)) }
            }),
            directory: std::env::current_dir()?,
            env: environment(),
            umask: umask(),
        })
    }

    /// Take the client's: the directory is the caller's, since it can be gone.
    fn take(fds: &[OwnedFd], env: &[(Vec<u8>, Vec<u8>)], mask: u32) {
        for (fd, from) in fds.iter().enumerate() {
            // SAFETY: dup2 onto this process's standard descriptors.
            unsafe { libc::dup2(from.as_raw_fd(), fd as RawFd) };
        }
        set_environment(env);
        // SAFETY: umask only sets the process's mask.
        unsafe { libc::umask(mask as libc::mode_t) };
    }

    fn restore(&self) {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        for (fd, from) in self.fds.iter().enumerate() {
            // SAFETY: as above, restoring the server's own descriptors.
            unsafe { libc::dup2(from.as_raw_fd(), fd as RawFd) };
        }
        let _ = std::env::set_current_dir(&self.directory);
        set_environment(&self.env);
        // SAFETY: as above.
        unsafe { libc::umask(self.umask as libc::mode_t) };
    }
}

/// Make `env` the process's whole environment.
fn set_environment(env: &[(Vec<u8>, Vec<u8>)]) {
    // SAFETY: only the serving thread reads or writes the environment between
    // requests and during one: a request's own threads end with it, and the
    // collector's sweep thread reads no variable.
    unsafe {
        for (name, _) in std::env::vars_os() {
            std::env::remove_var(name);
        }
        for (name, value) in env {
            if !name.is_empty()
                && !name.contains(&b'=')
                && !name.contains(&0)
                && !value.contains(&0)
            {
                std::env::set_var(
                    OsString::from_vec(name.clone()),
                    OsString::from_vec(value.clone()),
                );
            }
        }
    }
}

fn serve_requests(
    listener: &UnixListener,
    toolchain: Toolchain,
    max_memory: u64,
) -> Result<i32, Box<dyn std::error::Error>> {
    let startup = startup_digest(&environment());
    // A client that hangs up mid-answer is an error on the write, not a signal.
    // SAFETY: SIG_IGN installs no handler.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let own = Own::save()?;
    // Launched programs' scratch directories, until their clients hang up.
    let mut launched: Vec<(UnixStream, Option<crate::store::Scratch>)> = Vec::new();
    let mut sets = memo::WorkingSets::default();
    let mut retire_when_idle = false;
    while let Some(mut stream) = accept(listener, &mut launched, retire_when_idle)? {
        let Ok((bytes, fds)) = read_message(&mut stream) else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
            let _ = refuse(&mut stream, "the request does not decode");
            continue;
        };
        let replaced = Toolchain::on_disk().ok() != Some(toolchain);
        let route = route(&request.args);
        let refusal = if request.stop {
            Some("stopped")
        } else if request.protocol != PROTOCOL || request.toolchain != toolchain || replaced {
            Some("the client runs another toolchain")
        } else if startup_digest(&request.env) != startup {
            Some("the client's startup variables differ from the server's")
        } else if fds.len() != 3 {
            Some("the request is not one a resident compiler serves")
        } else {
            None
        };
        let (Some(route), None) = (route, refusal) else {
            let reason = refusal.unwrap_or("the request is not one a resident compiler serves");
            let _ = refuse(&mut stream, reason);
            if request.stop || replaced {
                return Ok(0);
            }
            continue;
        };
        Own::take(&fds, &request.env, request.umask);
        drop(fds);
        let cwd = PathBuf::from(OsString::from_vec(request.cwd));
        CLIENT.with_borrow_mut(|client| *client = stream.try_clone().ok());
        if std::env::var_os("HIRSEL_TRACE_REQUEST_PHASES").is_some_and(|v| v == "1") {
            crate::meter::trace::begin();
        }
        let outcome = if std::env::set_current_dir(&cwd).is_ok() {
            sets.begin(route.input.absolute(&cwd));
            crate::meter::trace::mark("working set ready");
            let args = request.args;
            std::panic::catch_unwind(move || {
                #[cfg(test)]
                if std::env::var_os("HIRSEL_RESIDENT_LAW_PANIC").is_some() {
                    panic!("the law's request panics");
                }
                crate::request::serve(args)
            })
        } else {
            eprintln!("hirsel: the working directory {} is gone", cwd.display());
            Ok(Outcome::Exit(2))
        };
        let (outcome, panicked) = match outcome {
            Ok(Outcome::Test(session)) => (None, !serve_session(*session)),
            Ok(outcome) => (Some(outcome), false),
            Err(_) => (Some(Outcome::Exit(101)), true),
        };
        if let Some(outcome) = &outcome {
            answer(outcome);
        }
        own.restore();
        if let Some(Outcome::Launch(launch)) = outcome {
            launched.push((stream, launch.keep));
        }
        if panicked {
            // The request fails alone; what it was deriving is dropped.
            sets.forget();
        } else {
            sets.settle();
        }
        // A launched program's scratch must survive until its client is done.
        retire_when_idle = !sets.fit(max_memory);
    }
    Ok(0)
}

/// Serve a test session on the connection of its request, under the
/// client's descriptors and directory, until the client finishes it or hangs
/// up (which drops it). False when a step panicked: its client is answered
/// 101, and the caller forgets what the session derived.
fn serve_session(mut session: crate::TestSession) -> bool {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let Some(mut stream) = CLIENT.with_borrow_mut(Option::take) else {
        return true;
    };
    let send = |stream: &mut UnixStream, answer: &Answer| {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        let bytes = serde_json::to_vec(answer).expect("an answer encodes");
        write_message(stream, &bytes, &[])
    };
    if send(&mut stream, &Answer::Session(session.plan().clone())).is_err() {
        return true;
    }
    loop {
        let Ok((bytes, _)) = read_message(&mut stream) else {
            return true;
        };
        let Ok(step) = serde_json::from_slice::<Step>(&bytes) else {
            return true;
        };
        let answer = match step {
            Step::Unit { unit, ran } => {
                catch_unwind(AssertUnwindSafe(|| session.step(unit, ran))).map(Answer::Test)
            }
            Step::Finish => {
                let code = catch_unwind(AssertUnwindSafe(move || session.finish()));
                crate::meter::trace::finish();
                let finished = code.is_ok();
                let _ = send(&mut stream, &Answer::Exit(code.unwrap_or(101)));
                return finished;
            }
        };
        let Ok(answer) = answer else {
            let _ = send(&mut stream, &Answer::Exit(101));
            return false;
        };
        if send(&mut stream, &answer).is_err() {
            return true;
        }
    }
}

fn refuse(stream: &mut UnixStream, reason: &str) -> io::Result<()> {
    write_message(
        stream,
        &serde_json::to_vec(&Answer::Refused(reason.to_owned()))?,
        &[],
    )
}

/// The next connection, releasing each launched program's scratch once its
/// client has hung up. None once the server retires: over its memory, or
/// [`IDLE_EXIT`] without a request, with no launched client left. A second
/// without a request returns the allocator's free pages.
fn accept(
    listener: &UnixListener,
    launched: &mut Vec<(UnixStream, Option<crate::store::Scratch>)>,
    retire_when_idle: bool,
) -> io::Result<Option<UnixStream>> {
    let quiet = Instant::now();
    let mut collected = false;
    loop {
        let idle = quiet.elapsed() >= IDLE_EXIT;
        if (retire_when_idle || idle) && launched.is_empty() {
            return Ok(None);
        }
        let timeout = if !collected {
            COLLECT_AFTER
        } else if idle {
            // Launched clients keep the server until they hang up.
            Duration::MAX
        } else {
            IDLE_EXIT - quiet.elapsed()
        };
        let timeout = i32::try_from(timeout.as_millis()).unwrap_or(-1);
        let mut polls: Vec<libc::pollfd> = std::iter::once(listener.as_raw_fd())
            .chain(launched.iter().map(|(stream, _)| stream.as_raw_fd()))
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: poll reads and writes the array it is given.
        let ready = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            if !collected {
                memo::release_unused_pages();
                collected = true;
            }
            continue;
        }
        let mut index = 0;
        launched.retain(|_| {
            index += 1;
            polls[index].revents == 0
        });
        if polls[0].revents != 0 && (!retire_when_idle || !launched.is_empty()) {
            return listener.accept().map(|(stream, _)| Some(stream));
        }
    }
}

/// This process's resident set, in bytes.
fn resident_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        // SAFETY: sysconf reads a constant.
        .map_or(0, |pages| {
            pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64
        })
}

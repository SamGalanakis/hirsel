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
//! (see [`crate::host::runtime_directory`]), named by the content
//! identity of its compiler executable, and holds `<toolchain>.lock` there
//! while it runs. It re-reads its executable's identity on each request and
//! exits once that changed. A request that panics fails alone: its client is
//! answered 101, and the server forgets every working set, since what the
//! request was deriving may be half made. The server keeps a share of its
//! memory limit ([`memo::Budget`]): before and after each request it evicts
//! least recently used working sets until it fits the retained share
//! (`--max-memory`), and during a request whenever it passes the pressure
//! share, never the request's own set, so an edit loop on a working set
//! larger than the share still finds its previous request's derivations and
//! link. The server returns its free pages to the system after a second
//! without requests and exits after [`IDLE_EXIT`] without any.
//!
//! A `run` request's program runs in the client, under the client's
//! terminal and signals; the server keeps the program's scratch directory
//! until that client hangs up. A `test` request is a session on its
//! connection: the server answers with the session's plan, the client steps
//! each unit and runs every launch the server hands back (under the client's
//! directory and environment, which write the pass cache), and the server
//! folds coverage and the law lock when the client finishes. Hanging up drops
//! the session.
//!
//! No request holds the server: [`budget`] refuses a client that waited
//! behind a running request for [`budget::PATIENCE`] (it compiles itself),
//! and stops a request that passes its CPU budget, answering its client and
//! restarting the server in place on the same socket.
use crate::request::Outcome;
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use std::sync::atomic::{AtomicBool, Ordering};

mod budget;
mod memo;
mod wire;
pub(crate) use memo::{
    CONSTANTS, DEBUG_POSITIONS, MATERIALISED, Memo, OBJECT_RECORDS, RECIPE_GROUPS, RECIPE_REACH,
    REFERENCED, RESOLVED, RUNTIME_SOURCES, WorkingMemo, parse_file, source_hash, symbol_suffix,
};
use wire::{read_message, write_message};

const PROTOCOL: u32 = 3;
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
        format!("{}.{suffix}", &self.0.to_string()[..32])
    }
}

/// The path a law's stand-in server binds for this toolchain, under the
/// runtime directory `runtime` (what `XDG_RUNTIME_DIR` names).
#[cfg(test)]
pub(crate) fn socket_under(runtime: &std::path::Path) -> PathBuf {
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
        #[serde(default)]
        mode: crate::tape_cli::outputs::Mode,
        argv: Vec<Vec<u8>>,
        env: Vec<(Vec<u8>, Vec<u8>)>,
    },
    Refused(String),
    /// The request passed its CPU budget: why, for the client to report.
    Exceeded(String),
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

/// Linux reaches the directory through its descriptor to keep the address
/// short. Check the address before using the path on either host.
fn socket_path(toolchain: Toolchain) -> io::Result<(PathBuf, std::fs::File)> {
    let held = std::fs::File::open(crate::host::runtime_directory("hirsel")?)?;
    let path = crate::host::fd_path(&held)?.join(toolchain.file("sock"));
    SocketAddr::from_pathname(&path)?;
    Ok((path, held))
}

struct Route {
    resident: bool,
    retryable: bool,
    input: memo::Input,
}

/// Route a request to the resident compiler of its toolchain.
fn route(args: &[String]) -> Option<Route> {
    use crate::cli::CompilerOption as Opt;
    let crate::cli::Action::Invoke(crate::cli::Invocation::Compiler(invocation)) =
        crate::cli::parse(args.iter().cloned()).ok()?
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
    for option in &invocation.options {
        match option {
            Opt::Resident => resident = true,
            Opt::Inputs(value) => manifest = Some(PathBuf::from(value)),
            Opt::Recipe(value) => recipe = Some(value.clone()),
            Opt::Entry(value) => entry = Some(value.clone()),
            // Ordinary compiler stores publish by content. A durable code
            // store also refreshes publication times. A custom linker can
            // have its own effects. Never repeat either after a lost reply.
            Opt::CodeStore(_) | Opt::Cc(_) => retryable = false,
            Opt::Grant(value)
                if invocation.command.name == "run" && value.starts_with("Foreign=") =>
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
    let budget = std::env::var_os(budget::CPU_VARIABLE);
    if let Err(error) = budget::cpu_seconds(budget.as_ref().map(|v| v.as_bytes())) {
        eprintln!("hirsel: {error}");
        return Some(2);
    }
    match try_submit(&route, args) {
        Ok(answer) => answer,
        Err(error) => {
            eprintln!("hirsel: resident compiler: {error}");
            Some(2)
        }
    }
}

fn connect(toolchain: Toolchain) -> io::Result<UnixStream> {
    let (path, _held) = socket_path(toolchain)?;
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
        umask: crate::host::umask()?,
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
        if !matches!(answer, Answer::Refused(_) | Answer::Exceeded(_))
            && let Some(output) = &mut output
        {
            output.rewind()?;
            let mut stdout = io::stdout().lock();
            io::copy(output, &mut stdout)?;
            stdout.flush()?;
        }
        return match answer {
            Answer::Exit(code) => Ok(Some(code)),
            Answer::Launch { argv, env, mode } => {
                let argv: Vec<OsString> = argv.into_iter().map(OsString::from_vec).collect();
                let env: Vec<(OsString, OsString)> = env
                    .into_iter()
                    .map(|(n, v)| (OsString::from_vec(n), OsString::from_vec(v)))
                    .collect();
                let code = crate::request::Launch::execute_mode(&argv, &env, &mode)
                    .map_err(|error| io::Error::other(error.to_string()));
                drop(stream);
                code.map(Some)
            }
            // The server serves other clients; this one compiles itself.
            Answer::Refused(_) => Ok(None),
            Answer::Exceeded(why) => Ok(Some(exceeded(&why))),
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
                Answer::Exceeded(why) => {
                    self.ended = Some(exceeded(&why));
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
        Answer::Exceeded(why) => Ok(exceeded(&why)),
        _ => Err(io::Error::other("the server answered out of turn")),
    }
}

/// Report a request that passed its CPU budget: its exit status.
fn exceeded(why: &str) -> i32 {
    eprintln!("hirsel: resident compiler: {why}");
    hirsel_guard::EXIT_STATUS
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
        .open(crate::host::runtime_directory("hirsel")?.join(toolchain.file("lock")))?;
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
            mode: launch.mode.clone(),
            argv: launch.argv.iter().map(|a| a.as_bytes().to_vec()).collect(),
            env: launch
                .env
                .iter()
                .map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
        },
    };
    let bytes = serde_json::to_vec(&answer).expect("an answer encodes");
    let _ = budget::send(&mut stream, &bytes, true);
}

/// Options of `hirsel serve`.
pub(crate) struct Serve {
    max_memory: Option<u64>,
    stop: bool,
}
impl Serve {
    pub(crate) fn read(
        invocation: crate::cli::Request<crate::cli::ServeOption>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use crate::cli::ServeOption as Opt;
        if invocation.positional.is_some() {
            return Err("serve takes no package: one server serves every directory".into());
        }
        let mut max_memory = None;
        let mut stop = false;
        for option in invocation.options {
            match option {
                Opt::MaxMemory(value) => {
                    max_memory = Some(
                        value
                            .parse()
                            .map_err(|_| "--max-memory expects a byte count")?,
                    )
                }
                Opt::Stop => stop = true,
            }
        }
        Ok(Self { max_memory, stop })
    }
}

/// `hirsel serve`: serve this toolchain's requests until stopped, idle, or
/// its executable replaced. The exit status of the command.
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
    let limit = hirsel_guard::limit().unwrap_or(hirsel_guard::DEFAULT_LIMIT);
    let budget = memo::Budget::of(limit, serve.max_memory)?;
    let (socket, _held) = socket_path(toolchain)?;
    let (lock, listener, launched) = if let Some(inherited) = budget::inherited() {
        // This image succeeds one that stopped a request over its budget.
        if inherited.toolchain != toolchain {
            let _ = std::fs::remove_file(&socket);
            return Ok(0);
        }
        (inherited.lock, inherited.listener, inherited.launched)
    } else {
        let Some(lock) = lock(toolchain)? else {
            // Another server serves the toolchain.
            return Ok(0);
        };
        match std::fs::remove_file(&socket) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        (lock, UnixListener::bind(&socket)?, Vec::new())
    };
    SERVING.store(true, Ordering::Relaxed);
    let served = serve_requests(&listener, &lock, launched, toolchain, budget);
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
            umask: crate::host::umask()?,
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
    lock: &std::fs::File,
    mut launched: Vec<(UnixStream, Option<crate::store::Scratch>)>,
    toolchain: Toolchain,
    budget: memo::Budget,
) -> Result<i32, Box<dyn std::error::Error>> {
    let startup = startup_digest(&environment());
    // A client that hangs up mid-answer is an error on the write, not a signal.
    // SAFETY: SIG_IGN installs no handler.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let own = Own::save()?;
    // `launched`: programs' scratch directories, until their clients hang up.
    // `held`: connections that arrived during the previous request.
    let mut held = std::collections::VecDeque::new();
    memo::serve_under(budget.pressure);
    loop {
        let (mut stream, bytes, fds) = match held.pop_front() {
            Some(budget::Held {
                stream, bytes, fds, ..
            }) => (stream, bytes, fds),
            None => {
                let Some(mut stream) = accept(listener, &mut launched)? else {
                    break;
                };
                let Ok((bytes, fds)) = read_message(&mut stream) else {
                    continue;
                };
                (stream, bytes, fds)
            }
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
                for mut waiting in held {
                    let _ = refuse(&mut waiting.stream, reason);
                }
                return Ok(0);
            }
            continue;
        };
        let seconds = budget::request_cpu_seconds(&request.env);
        let recycle = budget::Recycle::prepare(
            &own,
            toolchain,
            listener.as_raw_fd(),
            lock.as_raw_fd(),
            &launched,
            seconds,
        )?;
        let watch = budget::Watch::start(
            listener.as_raw_fd(),
            std::mem::take(&mut held),
            stream.try_clone().ok(),
            seconds,
            recycle,
        )?;
        Own::take(&fds, &request.env, request.umask);
        drop(fds);
        let cwd = PathBuf::from(OsString::from_vec(request.cwd));
        CLIENT.with_borrow_mut(|client| *client = stream.try_clone().ok());
        if std::env::var_os("HIRSEL_TRACE_REQUEST_PHASES").is_some_and(|v| v == "1") {
            crate::meter::trace::begin();
        }
        let outcome = if std::env::set_current_dir(&cwd).is_ok() {
            {
                let mut sets = memo::sets();
                sets.begin(route.input.absolute(&cwd));
                // The set served before this request is spared no longer.
                sets.fit(budget.retain);
            }
            crate::meter::trace::mark("working set ready");
            let args = request.args;
            std::panic::catch_unwind(move || {
                #[cfg(test)]
                if std::env::var_os("HIRSEL_RESIDENT_LAW_PANIC").is_some() {
                    panic!("the law's request panics");
                }
                #[cfg(test)]
                if let Some(seconds) = std::env::var_os("HIRSEL_RESIDENT_LAW_SPIN") {
                    // A request that computes for this long before it builds.
                    let seconds = seconds.to_str().unwrap().parse().unwrap();
                    let until = Instant::now() + Duration::from_secs(seconds);
                    while Instant::now() < until {
                        std::hint::black_box(0u64);
                    }
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
        let mut sets = memo::sets();
        if panicked {
            // The request fails alone; what it was deriving is dropped.
            sets.forget();
        } else {
            sets.settle();
        }
        sets.fit(budget.retain);
        drop(sets);
        let watched = watch.end();
        held = watched.held;
        if watched.stopped {
            for mut waiting in held {
                let _ = refuse(&mut waiting.stream, "stopped");
            }
            return Ok(0);
        }
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
        budget::send(stream, &bytes, matches!(answer, Answer::Exit(_)))
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
/// client has hung up. None once the server retires: [`IDLE_EXIT`] without a
/// request, with no launched client left. A second without a request returns
/// the allocator's free pages.
fn accept(
    listener: &UnixListener,
    launched: &mut Vec<(UnixStream, Option<crate::store::Scratch>)>,
) -> io::Result<Option<UnixStream>> {
    let quiet = Instant::now();
    let mut collected = false;
    loop {
        let idle = quiet.elapsed() >= IDLE_EXIT;
        if idle && launched.is_empty() {
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
        if polls[0].revents != 0 {
            return listener.accept().map(|(stream, _)| Some(stream));
        }
    }
}

//! The resident compiler (`hirsel serve`): one long-lived process per store
//! that serves the store's `build` and `run` requests, so the state a request
//! derives from unchanged inputs stays in memory between edits.
//!
//! A request runs in the server exactly as it runs one-shot: the server calls
//! the same [`crate::request`] path with the client's arguments, working
//! directory and standard descriptors, and answers with the request's
//! [`Outcome`]. A one-shot process is a session with one request; nothing
//! here computes an answer of its own. A client whose toolchain or
//! environment differs from the server's is refused, and then runs the
//! request itself (with `--resident`, it replaces the server first).
//!
//! The server listens on `resident.sock` in the store and holds
//! `resident.lock` there while it runs. A `run` request's program runs in the
//! client, under the client's terminal and signals; the server keeps the
//! program's scratch directory until that client hangs up.
use crate::request::Outcome;
use hirsel_core_ids::ContentHash;
use hirsel_core_resolve::{ModuleId, Resolution, SourceId};
use hirsel_core_syntax::{Derive, ParseMode, ParseOutput, Symbol, Symbols};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const SOCKET: &str = "resident.sock";
const LOCK: &str = "resident.lock";
const PROTOCOL: u32 = 1;
/// The memory above which a server exits after a request.
pub(crate) const DEFAULT_MAX_MEMORY: u64 = 1 << 30;

static SERVING: AtomicBool = AtomicBool::new(false);
thread_local! {
    /// The connection of the request being served, until it is answered.
    static CLIENT: std::cell::RefCell<Option<UnixStream>> = const { std::cell::RefCell::new(None) };
}

/// The requests this server has begun, which dates each memo entry's use.
static REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A resident compiler's memo of one pure derivation, keyed by every input
/// the derivation reads, so an entry is never stale; outside a server it
/// remembers nothing. After each request the server drops the entries that
/// request did not use ([`sweep`]), so a memo holds one request's working set.
pub(crate) struct Memo<K, V> {
    entries: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<K, (V, u64)>>>,
}
impl<K: std::hash::Hash + Eq, V: Clone> Memo<K, V> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: std::sync::LazyLock::new(Default::default),
        }
    }
    /// The value under `key`, made by `make` unless remembered; a None key
    /// is a derivation this memo does not key.
    pub(crate) fn get_or<E>(
        &self,
        key: Option<K>,
        make: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        let Some(key) = key.filter(|_| serving()) else {
            return make();
        };
        let request = REQUEST.load(Ordering::Relaxed);
        if let Some((value, used)) = self.entries.lock().unwrap().get_mut(&key) {
            *used = request;
            return Ok(value.clone());
        }
        let value = make()?;
        self.entries
            .lock()
            .unwrap()
            .insert(key, (value.clone(), request));
        Ok(value)
    }
    fn sweep(&self, request: u64) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, (_, used)| *used == request);
    }
}

/// Materialised instances, by identity and definition record.
pub(crate) static MATERIALISED: Memo<
    (
        hirsel_core_ir::Identity,
        Option<hirsel_core_ids::ContentHash>,
    ),
    crate::instances::Materialised,
> = Memo::new();

/// The site items each link input references, by the input's content key.
pub(crate) static REFERENCED: Memo<
    (hirsel_core_ids::ContentHash, u64),
    crate::native_entry::Referenced,
> = Memo::new();

/// Each stored object record's key-determined parts, by store and key.
pub(crate) static OBJECT_RECORDS: Memo<(PathBuf, ContentHash), crate::native_object::Shared> =
    Memo::new();

#[derive(Clone)]
struct SourceHash {
    bytes: Arc<[u8]>,
    hash: ContentHash,
}

static SOURCE_HASHES: Memo<PathBuf, SourceHash> = Memo::new();

/// Hash a captured file once per content change. The path selects the
/// previous capture; an exact byte comparison confirms the hit.
pub(crate) fn source_hash(path: &Path, bytes: &Arc<[u8]>) -> ContentHash {
    if !serving() {
        return ContentHash::of_bytes(bytes);
    }
    let request = REQUEST.load(Ordering::Relaxed);
    let mut entries = SOURCE_HASHES.entries.lock().unwrap();
    if let Some((prior, used)) = entries.get_mut(path)
        && prior.bytes == *bytes
    {
        *used = request;
        return prior.hash;
    }
    let hash = ContentHash::of_bytes(bytes);
    entries.insert(
        path.to_owned(),
        (
            SourceHash {
                bytes: Arc::clone(bytes),
                hash,
            },
            request,
        ),
    );
    hash
}

#[derive(Clone)]
struct Parsed {
    output: Arc<ParseOutput>,
    added: Arc<[String]>,
    after: ContentHash,
}

static PARSED: Memo<ContentHash, Parsed> = Memo::new();

/// The parse stores numeric symbols. The key names all earlier assignments,
/// and replaying the parse's additions gives later files the same numbers.
pub(crate) fn parse_file(
    prefix: &mut ContentHash,
    symbols: &mut Symbols,
    hash: ContentHash,
    bytes: &[u8],
    mode: ParseMode,
    derive: Option<&Derive>,
) -> io::Result<(ContentHash, ParseOutput, bool)> {
    let style = derive.map(|d| (&d.codec, &d.path));
    let key = crate::native::key("hirsel.resident-parse.v1", &(*prefix, hash, mode, style))?;
    let start = symbols.len();
    let mut miss = false;
    let parsed = PARSED.get_or(Some(key), || {
        miss = true;
        let output = hirsel_core_resolve::parse_module(bytes, mode, symbols, derive);
        let added: Arc<[String]> = symbol_suffix(symbols, start).into();
        let after = if added.is_empty() {
            *prefix
        } else {
            crate::native::key("hirsel.resident-symbols.v1", &(*prefix, &added))?
        };
        Ok::<_, io::Error>(Parsed {
            output: Arc::new(output),
            added,
            after,
        })
    })?;
    if !miss {
        for name in parsed.added.iter() {
            symbols.intern(name);
        }
    }
    *prefix = parsed.after;
    Ok((key, (*parsed.output).clone(), miss))
}

pub(crate) fn symbol_suffix(symbols: &Symbols, start: usize) -> Vec<String> {
    (start..symbols.len())
        .map(|i| symbols.text(Symbol(i as u32)).to_owned())
        .collect()
}

/// File syntax plus its occurrence, the whole request's index shape and
/// final symbol assignments. Diagnostics with cross-file sites are rebuilt
/// against the current index.
pub(crate) static RESOLVED: Memo<
    (ContentHash, ContentHash, ContentHash, ModuleId, SourceId),
    Vec<Arc<Resolution>>,
> = Memo::new();

/// Drop every memo entry the request numbered `request` did not use.
fn sweep(request: u64) {
    MATERIALISED.sweep(request);
    REFERENCED.sweep(request);
    OBJECT_RECORDS.sweep(request);
    SOURCE_HASHES.sweep(request);
    PARSED.sweep(request);
    RESOLVED.sweep(request);
}

/// Whether this process is a resident compiler serving requests.
pub(crate) fn serving() -> bool {
    SERVING.load(Ordering::Relaxed)
}

/// The toolchain a process runs: its executable file's identity on disk. A
/// replaced or rebuilt toolchain has another.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
struct Toolchain {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}
impl Toolchain {
    fn current() -> io::Result<Self> {
        let meta = std::fs::metadata(std::env::current_exe()?)?;
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
        })
    }
}

/// The environment a request sees, without the variables a shell sets per
/// command or directory. A server serves only its own.
fn environment() -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(name, _)| !matches!(name.as_bytes(), b"_" | b"PWD" | b"OLDPWD" | b"SHLVL"))
        .map(|(name, value)| (name.into_vec(), value.into_vec()))
        .collect();
    env.sort();
    env
}

#[derive(Serialize, Deserialize)]
struct Request {
    protocol: u32,
    toolchain: Toolchain,
    env: Vec<(Vec<u8>, Vec<u8>)>,
    cwd: Vec<u8>,
    args: Vec<String>,
    /// The client replaces a server that refuses it.
    replace: bool,
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
        let got = libc::recvmsg(stream.as_raw_fd(), &mut header, libc::MSG_CMSG_CLOEXEC);
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
    Ok((
        PathBuf::from(format!("/proc/self/fd/{}/{name}", held.as_raw_fd())),
        held,
    ))
}

/// Where the resident compiler of a `build` or `run` request listens: the
/// store `--store` names, else `.hirsel-core` in the package directory, and
/// whether the request asks for one (`--resident`). None for any
/// other request.
fn request_store(args: &[String]) -> Option<(PathBuf, bool)> {
    use crate::cli::OptionId as Opt;
    let crate::cli::Action::Invoke(invocation) = crate::cli::parse(args.iter().cloned()).ok()?
    else {
        return None;
    };
    if !matches!(invocation.command.name, "build" | "run") {
        return None;
    }
    let mut store = None;
    let mut resident = false;
    for (option, value) in &invocation.options {
        match option {
            Opt::Store => store = value.clone().map(PathBuf::from),
            Opt::Resident => resident = true,
            _ => {}
        }
    }
    let store = store.unwrap_or_else(|| {
        Path::new(invocation.positional.as_deref().unwrap_or(".")).join(".hirsel-core")
    });
    Some((store, resident))
}

/// Submit a `build` or `run` request to the resident compiler serving its
/// store, starting one first under `--resident`. The request's exit status
/// when a server answered it; None when the caller runs it itself.
pub fn submit(args: &[String]) -> Option<i32> {
    let (store, resident) = request_store(args)?;
    match try_submit(&store, args, resident) {
        Ok(answer) => answer,
        Err(error) if resident => {
            eprintln!("hirsel: resident compiler: {error}");
            Some(2)
        }
        Err(_) => None,
    }
}

fn connect(store: &Path) -> io::Result<UnixStream> {
    let (path, _held) = short_path(store, SOCKET)?;
    UnixStream::connect(path)
}

fn try_submit(store: &Path, args: &[String], resident: bool) -> io::Result<Option<i32>> {
    let request = Request {
        protocol: PROTOCOL,
        toolchain: Toolchain::current()?,
        env: environment(),
        cwd: std::env::current_dir()?.into_os_string().into_vec(),
        args: args.to_vec(),
        replace: resident,
        stop: false,
    };
    let request = serde_json::to_vec(&request)?;
    let mut started = false;
    loop {
        let stream = match connect(store) {
            Ok(stream) => Some(stream),
            Err(_) if resident && !started => {
                start(store)?;
                started = true;
                continue;
            }
            Err(error) if resident => return Err(error),
            Err(_) => None,
        };
        let Some(mut stream) = stream else {
            return Ok(None);
        };
        write_message(&mut stream, &request, &[0, 1, 2])?;
        let (answer, _) = read_message(&mut stream).map_err(|error| {
            io::Error::other(format!("the server ended without an answer: {error}"))
        })?;
        match serde_json::from_slice::<Answer>(&answer)? {
            Answer::Exit(code) => return Ok(Some(code)),
            Answer::Launch { argv, env } => {
                let argv: Vec<OsString> = argv.into_iter().map(OsString::from_vec).collect();
                let env: Vec<(OsString, OsString)> = env
                    .into_iter()
                    .map(|(n, v)| (OsString::from_vec(n), OsString::from_vec(v)))
                    .collect();
                let code = crate::request::Launch::execute(&argv, &env)
                    .map_err(|error| io::Error::other(error.to_string()));
                drop(stream);
                return code.map(Some);
            }
            Answer::Refused(_) if resident && !started => {
                // The refusing server exits; start this toolchain's own.
                wait_gone(store);
                start(store)?;
                started = true;
            }
            Answer::Refused(reason) if resident => return Err(io::Error::other(reason)),
            Answer::Refused(_) => return Ok(None),
        }
    }
}

/// Wait until no server holds the store's lock.
fn wait_gone(store: &Path) {
    for _ in 0..5000 {
        match lock(store) {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
            Err(_) => return,
        }
    }
}

/// Start a server for `store` in its own session and wait until it accepts.
fn start(store: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    std::fs::create_dir_all(store)?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("serve")
        .arg("--store")
        .arg(store)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let mut child = command.spawn()?;
    for _ in 0..10_000 {
        if connect(store).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            // Another client's server may have taken the lock first.
            if connect(store).is_ok() {
                return Ok(());
            }
            return Err(io::Error::other(format!(
                "the resident compiler exited at start: {status}"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Err(io::Error::other("the resident compiler did not start"))
}

/// The store's server lock when no other process holds it.
fn lock(store: &Path) -> io::Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.join(LOCK))?;
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
    let Some(mut stream) = CLIENT.with_borrow_mut(Option::take) else {
        return;
    };
    let answer = match outcome {
        Outcome::Exit(code) => Answer::Exit(*code),
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
    store: PathBuf,
    max_memory: u64,
    stop: bool,
}
impl Serve {
    pub(crate) fn read(
        invocation: crate::cli::Invocation,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use crate::cli::OptionId as Opt;
        let mut store = None;
        let mut max_memory = DEFAULT_MAX_MEMORY;
        let mut stop = false;
        for (option, value) in invocation.options {
            match option {
                Opt::Store => store = value.map(PathBuf::from),
                Opt::MaxMemory => {
                    max_memory = value
                        .unwrap_or_default()
                        .parse()
                        .map_err(|_| "--max-memory expects a byte count")?
                }
                Opt::Stop => stop = true,
                _ => unreachable!("serve takes --store, --max-memory and --stop"),
            }
        }
        let store = store.unwrap_or_else(|| {
            Path::new(invocation.positional.as_deref().unwrap_or(".")).join(".hirsel-core")
        });
        Ok(Self {
            store,
            max_memory,
            stop,
        })
    }
}

/// `hirsel serve`: serve the store's requests until stopped, evicted by a
/// client of another toolchain, or over its memory with its caches dropped.
/// The exit status of the command.
pub(crate) fn run(serve: Serve) -> Result<i32, Box<dyn std::error::Error>> {
    if serve.stop {
        let Ok(mut stream) = connect(&serve.store) else {
            return Ok(0);
        };
        let request = Request {
            protocol: PROTOCOL,
            toolchain: Toolchain::current()?,
            env: vec![],
            cwd: vec![],
            args: vec![],
            replace: false,
            stop: true,
        };
        write_message(&mut stream, &serde_json::to_vec(&request)?, &[])?;
        let _ = read_message(&mut stream);
        wait_gone(&serve.store);
        return Ok(0);
    }
    std::fs::create_dir_all(&serve.store)?;
    let Some(_lock) = lock(&serve.store)? else {
        // Another server holds the store.
        return Ok(0);
    };
    let (socket, _held) = short_path(&serve.store, SOCKET)?;
    match std::fs::remove_file(&socket) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = UnixListener::bind(&socket)?;
    SERVING.store(true, Ordering::Relaxed);
    let served = serve_requests(&listener, serve.max_memory);
    SERVING.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_file(&socket);
    served
}

fn serve_requests(
    listener: &UnixListener,
    max_memory: u64,
) -> Result<i32, Box<dyn std::error::Error>> {
    let toolchain = Toolchain::current()?;
    let env = environment();
    // A client that hangs up mid-answer is an error on the write, not a signal.
    // SAFETY: SIG_IGN installs no handler.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let saved = [0, 1, 2].map(|fd| {
        // SAFETY: dup of a standard descriptor this process holds.
        unsafe { OwnedFd::from_raw_fd(libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10)) }
    });
    let home = std::env::current_dir()?;
    // Launched programs' scratch directories, until their clients hang up.
    let mut launched: Vec<(UnixStream, Option<crate::store::Scratch>)> = Vec::new();
    loop {
        let (mut stream, _) = accept(listener, &mut launched)?;
        let Ok((bytes, fds)) = read_message(&mut stream) else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
            let _ = refuse(&mut stream, "the request does not decode");
            continue;
        };
        let current = Toolchain::current().ok();
        let refusal = if request.stop {
            Some("stopped")
        } else if request.protocol != PROTOCOL
            || request.toolchain != toolchain
            || current != Some(toolchain)
        {
            Some("the client runs another toolchain")
        } else if request.env != env {
            Some("the client's environment differs from the server's")
        } else if fds.len() != 3 || request_store(&request.args).is_none() {
            Some("the request is not one a resident compiler serves")
        } else {
            None
        };
        if let Some(reason) = refusal {
            let _ = refuse(&mut stream, reason);
            if request.stop || request.replace || current != Some(toolchain) {
                return Ok(0);
            }
            continue;
        }
        for (fd, from) in fds.iter().enumerate() {
            // SAFETY: dup2 onto this process's standard descriptors.
            unsafe { libc::dup2(from.as_raw_fd(), fd as RawFd) };
        }
        drop(fds);
        let served = REQUEST.fetch_add(1, Ordering::Relaxed) + 1;
        let cwd = PathBuf::from(OsString::from_vec(request.cwd));
        let outcome = if std::env::set_current_dir(&cwd).is_ok() {
            CLIENT.with_borrow_mut(|client| *client = stream.try_clone().ok());
            let args = request.args;
            std::panic::catch_unwind(move || crate::request::serve(args))
        } else {
            eprintln!("hirsel: the working directory {} is gone", cwd.display());
            Ok(Outcome::Exit(2))
        };
        let panicked = outcome.is_err();
        let outcome = outcome.unwrap_or(Outcome::Exit(101));
        answer(&outcome);
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        for (fd, from) in saved.iter().enumerate() {
            // SAFETY: as above, restoring the server's own descriptors.
            unsafe { libc::dup2(from.as_raw_fd(), fd as RawFd) };
        }
        let _ = std::env::set_current_dir(&home);
        if let Outcome::Launch(launch) = outcome {
            launched.push((stream, launch.keep));
        }
        sweep(served);
        if panicked {
            // The request's state may be torn; the next client starts afresh.
            return Ok(101);
        }
        if resident_bytes() > max_memory {
            return Ok(0);
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
/// client has hung up.
fn accept(
    listener: &UnixListener,
    launched: &mut Vec<(UnixStream, Option<crate::store::Scratch>)>,
) -> io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
    loop {
        let mut polls: Vec<libc::pollfd> = std::iter::once(listener.as_raw_fd())
            .chain(launched.iter().map(|(stream, _)| stream.as_raw_fd()))
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: poll reads and writes the array it is given.
        if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let mut index = 0;
        launched.retain(|_| {
            index += 1;
            polls[index].revents == 0
        });
        if polls[0].revents != 0 {
            return listener.accept();
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

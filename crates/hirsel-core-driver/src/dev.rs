use crate::{
    cli::{Invocation, OptionId},
    package_workspace::Workspace,
    sdk::{Describe, Schema},
};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[path = "dev_session.rs"]
mod session;
#[path = "dev_transport.rs"]
mod transport;

type Error = Box<dyn std::error::Error>;
const POLL: Duration = Duration::from_millis(100);
const STOP_GRACE: Duration = Duration::from_secs(5);
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct ChildProcess(Child);

impl ChildProcess {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let parent = std::process::id();
        // SAFETY: these syscalls only set this child's parent-death signal and
        // inspect its parent, and run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() as u32 != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
        Ok(Self(command.spawn()?))
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            if STOP.load(Ordering::Relaxed) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            thread::sleep(POLL);
        }
    }

    fn stop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: this is a child owned by the supervisor that has not been reaped.
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if !matches!(self.0.try_wait(), Ok(None)) {
                return;
            }
            thread::sleep(POLL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Options {
    server: PathBuf,
    listen: SocketAddr,
    build: Vec<OsString>,
    arguments: Vec<String>,
}

impl Options {
    fn read(invocation: Invocation) -> Result<Self, Error> {
        let mut options = Self {
            server: fs::canonicalize(invocation.positional.ok_or("dev requires SERVER")?)?,
            listen: ([127, 0, 0, 1], 7700).into(),
            build: Vec::new(),
            arguments: invocation.arguments,
        };
        let mut limit = false;
        let mut net = false;
        for (id, value) in invocation.options {
            let flag = match id {
                OptionId::Listen => {
                    options.listen = value.ok_or("missing listen address")?.parse()?;
                    continue;
                }
                OptionId::Limit => {
                    limit = true;
                    "--limit"
                }
                OptionId::Grant => {
                    net |= value
                        .as_deref()
                        .is_some_and(|grant| grant.starts_with("Net"));
                    "--grant"
                }
                OptionId::Dir => "--dir",
                OptionId::Backend => "--backend",
                OptionId::Libraries => "--libraries",
                OptionId::Runtime => "--runtime",
                OptionId::SourceCore => "--source-core",
                OptionId::SourceStd => "--source-std",
                _ => return Err("unsupported dev option".into()),
            };
            options.build.push(flag.into());
            options.build.extend(value.map(OsString::from));
        }
        if !options.listen.ip().is_loopback() {
            return Err("dev listens on a loopback address".into());
        }
        if !limit {
            options
                .build
                .extend(["--limit".into(), "1610612736".into()]);
        }
        for grant in ["Clock", "Random", "Tasks", "Process", "Stderr"] {
            options.build.extend(["--grant".into(), grant.into()]);
        }
        if !net {
            options
                .build
                .extend(["--grant".into(), "Net=Listen 127.0.0.1/32 0-65535".into()]);
        }
        Ok(options)
    }

    fn build(&self, store: &Path, output: &Path) -> Result<bool, Error> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("build")
            .arg(&self.server)
            .arg("--dir")
            .arg(store)
            .args(&self.build)
            .arg("--output")
            .arg(output);
        Ok(ChildProcess::spawn(&mut command)?.wait()?.success())
    }

    fn install(&self, executable: &Path) -> Result<bool, Error> {
        let mut command = Command::new(executable);
        command
            .arg("--hirsel-install")
            .args(&self.arguments)
            .stdin(Stdio::null());
        Ok(ChildProcess::spawn(&mut command)?.wait()?.success())
    }

    fn start(&self, executable: &Path, listener: &TcpListener) -> Result<ChildProcess, Error> {
        // The shell sets socket-activation metadata to its own PID and hands
        // that same PID and descriptor to the executable.
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "LISTEN_PID=$$ LISTEN_FDS=1 exec \"$@\"", "hirsel-dev"])
            .arg(executable)
            .args(&self.arguments)
            .stdin(Stdio::null());
        let fd = listener.as_raw_fd();
        // SAFETY: the listener owner stays live until spawn returns. The child
        // owns descriptor 3 after dup2, and retains it across exec.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut process = ChildProcess::spawn(&mut command)?;
        let address = listener.local_addr()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !STOP.load(Ordering::Relaxed) {
            if let Some(status) = process.0.try_wait()? {
                return Err(format!("place exited during startup: {status}").into());
            }
            if ready(address) {
                return Ok(process);
            }
            thread::sleep(POLL);
        }
        Err("the development place did not become ready".into())
    }
}

fn ready(address: SocketAddr) -> bool {
    (|| -> io::Result<bool> {
        let mut socket = TcpStream::connect_timeout(&address, POLL)?;
        socket.set_read_timeout(Some(POLL))?;
        socket.set_write_timeout(Some(POLL))?;
        write!(
            socket,
            "GET /hirsel/status HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )?;
        let mut status = [0; 12];
        socket.read_exact(&mut status)?;
        Ok(&status == b"HTTP/1.1 200")
    })()
    .unwrap_or(false)
}

fn sources(server: &Path) -> Result<Vec<PathBuf>, Error> {
    let workspace = Workspace::discover(server)?.ok_or("dev requires a Hirsel package")?;
    let mut files = vec![workspace.directory.join(crate::package_dir::MANIFEST)];
    for member in workspace.members.values() {
        files.push(member.manifest_path.clone());
        let directory = workspace.directory.join(&member.directory);
        files.extend(member.content.files.keys().map(|path| directory.join(path)));
    }
    Ok(files)
}

fn source_state(paths: &[PathBuf]) -> Vec<Option<[i64; 4]>> {
    use std::os::unix::fs::MetadataExt;
    paths
        .iter()
        .map(|path| {
            fs::metadata(path).ok().map(|info| {
                [
                    info.ino() as i64,
                    info.size() as i64,
                    info.mtime(),
                    info.mtime_nsec(),
                ]
            })
        })
        .collect()
}

fn describe(path: &Path) -> Result<Describe, Error> {
    let response = crate::sdk_command::describe(path, 16 * 1024 * 1024, Duration::from_secs(10))?;
    Ok(Describe::read(&response)?)
}

struct Connections(Vec<thread::JoinHandle<()>>);

impl Connections {
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.0.len() {
            if self.0[i].is_finished() {
                let _ = self.0.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
    }
}

impl Drop for Connections {
    fn drop(&mut self) {
        STOP.store(true, Ordering::Relaxed);
        for join in self.0.drain(..) {
            let _ = join.join();
        }
    }
}

pub(in crate::request) fn run(invocation: Invocation) -> Result<i32, Error> {
    let options = Options::read(invocation)?;
    if crate::resident::serving() {
        return Err("dev must run in the terminal process".into());
    }
    // SAFETY: the handler only changes an atomic flag and leaves the child
    // owner in charge of draining and reaping.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let temporary = tempfile::tempdir()?;
    let store = temporary.path().join("store");
    fs::create_dir(&store)?;
    let public = TcpListener::bind(options.listen)?;
    public.set_nonblocking(true)?;
    let backend = TcpListener::bind(("127.0.0.1", 0))?;
    let address = backend.local_addr()?;
    let codec = Arc::new(Schema::read(crate::sdk_command::FRAME_SCHEMA)?);
    let mut files = sources(&options.server)?;
    let mut seen = source_state(&files);
    let mut generation = 0u64;
    let mut active = temporary.path().join("place-0");
    if !options.build(&store, &active)? {
        return Ok(1);
    }
    let description = describe(&active)?;
    let mut process = options.start(&active, &backend)?;
    let current: session::Current = Arc::new(Mutex::new(Some(Arc::new(session::Release {
        generation,
        describe: description,
    }))));
    let mut connections = Connections(Vec::new());
    println!("hirsel dev listening on {}", public.local_addr()?);

    while !STOP.load(Ordering::Relaxed) {
        if let Some(status) = process.0.try_wait()? {
            *current.lock().unwrap() = None;
            return Err(format!("the development place exited: {status}").into());
        }
        if let Ok((stream, _)) = public.accept() {
            let selected = Arc::clone(&current);
            let codec = Arc::clone(&codec);
            connections.0.push(thread::Builder::new().spawn(move || {
                if let Err(problem) = session::serve(stream, address, selected, codec) {
                    eprintln!("hirsel dev session: {problem}");
                }
            })?);
        }
        connections.reap();
        let captured = source_state(&files);
        if captured != seen {
            seen = captured;
            generation += 1;
            let candidate = temporary.path().join(format!("place-{generation}"));
            if options.build(&store, &candidate)? {
                if let Ok(new_files) = sources(&options.server) {
                    // Keep the pre-build stamps for watched inputs so edits
                    // during compilation still schedule the next build.
                    let old: std::collections::BTreeMap<_, _> =
                        files.into_iter().zip(seen).collect();
                    seen = new_files
                        .iter()
                        .map(|file| old.get(file).copied().flatten())
                        .collect();
                    files = new_files;
                }
                let mut description = match describe(&candidate) {
                    Ok(description) => description,
                    Err(error) => {
                        eprintln!("hirsel dev: {error}");
                        continue;
                    }
                };
                *current.lock().unwrap() = None;
                process.stop();
                if options.install(&candidate)? {
                    process = options.start(&candidate, &backend)?;
                    fs::remove_file(&active)?;
                    active = candidate;
                } else {
                    eprintln!("hirsel dev: migration refused");
                    match options.start(&active, &backend) {
                        Ok(reopened) => {
                            process = reopened;
                            description = describe(&active)?;
                        }
                        Err(previous) => {
                            process = options.start(&candidate, &backend).map_err(|next| {
                                format!("could not reopen either activation: {previous}; {next}")
                            })?;
                            fs::remove_file(&active)?;
                            active = candidate;
                        }
                    }
                }
                *current.lock().unwrap() = Some(Arc::new(session::Release {
                    generation,
                    describe: description,
                }));
            }
        }
        thread::sleep(POLL);
    }
    process.stop();
    Ok(0)
}

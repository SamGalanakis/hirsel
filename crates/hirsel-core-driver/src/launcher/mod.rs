//! The thin launcher: the one local process that holds the groups' listeners and starts
//! their children for the platform host (docs/spec/launch.md "The launcher", RUNNER-SPEC
//! §14.3-§14.5).
//!
//! Everything runs in one state loop ([`Launcher::turn`]). The host talks to it over one
//! control session at a time; each request is answered, and each child end, arrival and
//! endpoint notice is reported, as one ordered event stream. The launcher keeps no record
//! that a decision reads again: what it knows of its children is in memory and reaches a
//! new host through the inventory that answers `Hello`.
mod admit;
mod bootstrap;
mod cgroup;
#[cfg(test)]
mod laws;
pub(crate) mod objects;
mod ready;
pub(crate) mod session;
mod spawn;
mod stores;

use hirsel_core_ids::launch::LAUNCH_IMAGE_ENV;
use ready::{CHANGE, Epoll, READ, WRITE, Watch};

use session::{
    Approvals, Attempt, Bindings, Child, ChildState, End, Event, Failure, Group, HostCommand,
    HostReply, Input, InstallKind, Label, Launch, Limits, Mode, Phase, Refusal, Request, Role,
    Status, Wire,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub(crate) use admit::{Admission, deploy_foreign_lines, limits, read_recipe};
use stores::Stores;

/// Create `path` as a directory only its owner can enter, or check that it is one.
pub(crate) fn private_dir(path: PathBuf) -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "host directory must be private",
        ));
    }
    path.canonicalize()
}

/// Bind a control socket at `path` that only its owner can connect to. A stale socket
/// nobody answers on is replaced; a live one is in use.
pub(crate) fn bind_control(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() || UnixStream::connect(path).is_ok() {
            return Err(io::ErrorKind::AddrInUse.into());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Concurrent admissions; a further `Admit` fails until one ends.
const ADMISSIONS: usize = 4;

/// The staging bound of the launcher's code store.
const STAGING_BYTES: u64 = 512 << 20;

/// Lock `state`, recover any containers, then serve the control session on `SOCKET`.
pub(crate) fn run(
    invocation: crate::cli::Invocation,
) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    use crate::cli::OptionId as Opt;
    let (mut control, mut state, mut cgroup_root) = (None, None, None);
    let (mut operator, mut platform_host, mut max_places, mut idle_ms) = (None, None, None, None);
    for (option, value) in invocation.options {
        let text = value.unwrap_or_default();
        let value = PathBuf::from(&text);
        match option {
            Opt::Control => control = Some(value),
            Opt::HostState => state = Some(value),
            Opt::CgroupRoot => cgroup_root = Some(value),
            Opt::OperatorListen => operator = Some(value),
            Opt::PlatformHost => platform_host = Some(value),
            Opt::MaxPlaces => max_places = Some(positive(&text, "--max-places")?),
            Opt::IdleMs => idle_ms = Some(positive(&text, "--idle-ms")?),
            _ => return Err("unexpected launcher option".into()),
        }
    }
    let state = private_dir(state.ok_or("--state DIR is required")?)?;
    let control = control.ok_or("--control SOCKET is required")?;
    let mut admission = None;
    let mut launcher = Launcher::new(&state, cgroup_root.as_deref(), || {
        let stores = Stores::open(&state, limits().recipe_bytes)?;
        admission = Some(Admission::open(
            &state,
            stores.records(),
            limits(),
            STAGING_BYTES,
        )?);
        Ok(stores)
    })?;
    launcher.admit_with(admission.expect("opened with the stores"))?;
    launcher.listen(bind_control(&control)?)?;
    match (operator, platform_host) {
        (None, None) => (),
        (Some(operator), Some(program)) => {
            // The host's argv names the control socket by its name in STATE, which is the
            // host's working directory (launch.md "Starting the host").
            let name = control
                .file_name()
                .filter(|_| {
                    control.parent().and_then(|p| p.canonicalize().ok()) == Some(state.clone())
                })
                .ok_or("--control must name a socket directly in --state")?;
            let args = vec![
                OsString::from("ctl"),
                name.to_owned(),
                OsString::from(max_places.unwrap_or(1).to_string()),
                OsString::from(idle_ms.unwrap_or(30000).to_string()),
            ];
            let program = std::path::absolute(program)?;
            if !program.is_file() {
                eprintln!(
                    "hirsel launcher: the installation has no platform host at {}",
                    program.display()
                );
                std::process::exit(74);
            }
            // The host's one Dir is STATE/catalog; it creates the catalog store inside it.
            std::fs::create_dir_all(state.join("catalog"))?;
            launcher.bootstrap = Some(bootstrap::Bootstrap::new(
                program,
                args,
                state.clone(),
                bind_control(&std::path::absolute(operator)?)?,
            ));
        }
        _ => return Err("--operator-listen and --platform-host go together".into()),
    }
    loop {
        launcher.wait_turn(None)?;
        if let Some(code) = launcher.bootstrap.as_ref().and_then(|b| b.stopped()) {
            eprintln!("hirsel launcher: the platform host ended with status {code}");
            drop(launcher);
            std::process::exit(code);
        }
    }
}

fn positive(text: &str, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    match text.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("{name} takes a positive decimal").into()),
    }
}

/// What the launcher runs for an assembly record: its image and the recipe's launch words.
pub(crate) struct Image {
    pub(crate) path: PathBuf,
    executable: ImageFile,
    pub(crate) digest: [u8; 32],
    pub(crate) n_place: u64,
    /// The recipe's runtime prefix words (`--hirsel-exec=` approvals).
    pub(crate) prefix: Vec<String>,
}

enum ImageFile {
    Verified(File),
    #[cfg(test)]
    Script,
}

/// The launcher's content-addressed objects: assembly records, their images, and the code
/// objects their recipes list.
pub(crate) trait Images {
    fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String>;
    /// Remove every object that no kept assembly record reaches and that `reserved` does
    /// not hold for an admission in flight; answer how many.
    fn collect(
        &self,
        keep: &BTreeSet<[u8; 32]>,
        reserved: &dyn Fn(crate::code_admission::ObjectKey) -> bool,
    ) -> io::Result<u64>;
}

/// The bound on a wake report (one value-codec `Option[u64]`).
const REPORT_BYTES: u64 = 12;

/// A nonblocking framed stream: the control session, or a serving child's endpoint.
struct Conn {
    watch: Watch,
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    closed: bool,
    /// Reading is held back while an admission's input waits for its worker.
    paused: bool,
}

impl Conn {
    fn new(epoll: &Epoll, stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            watch: epoll.watch(stream.as_raw_fd()),
            stream,
            input: Vec::new(),
            output: Vec::new(),
            closed: false,
            paused: false,
        })
    }

    fn watch(&mut self) -> io::Result<()> {
        let read = if self.paused { 0 } else { READ };
        let interest = if self.closed {
            0
        } else if self.output.is_empty() {
            read
        } else {
            read | WRITE
        };
        self.watch.set(interest)
    }

    /// Read what is ready. A peer that closed or failed ends the stream.
    fn fill(&mut self) {
        let mut buffer = [0; 1 << 16];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => return self.closed = true,
                Ok(n) => self.input.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return self.closed = true,
            }
        }
    }

    fn frame(&mut self) -> Option<Vec<u8>> {
        let size = u32::from_le_bytes(self.input.get(..4)?.try_into().unwrap()) as usize;
        if size > session::MAX_FRAME {
            self.closed = true;
            return None;
        }
        let body = self.input.get(4..4 + size)?.to_vec();
        self.input.drain(..4 + size);
        Some(body)
    }

    fn queue(&mut self, value: &impl Wire) {
        let _ = session::write_frame(&mut self.output, value);
    }

    /// Write what the peer takes now; a stalled peer never blocks the loop.
    fn flush(&mut self) {
        while !self.output.is_empty() && !self.closed {
            match self.stream.write(&self.output) {
                Ok(n) => drop(self.output.drain(..n)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => self.closed = true,
            }
        }
    }
}

struct Listening {
    watch: Watch,
    group: Group,
    socket: TcpListener,
    label: Label,
    bindings: Bindings,
    limits: Limits,
    /// The host has been told this listener is readable, by `Arrived`, an inventory or an
    /// end's sample; the watch stays off until a serving child has run again.
    reported: bool,
    retiring: bool,
}

struct Process {
    watch: Option<Watch>,
    child: Option<Running>,
    /// A serving child's private endpoint (descriptor 4).
    endpoint: Option<Conn>,
    /// A serving open awaiting the host's durable acceptance. Replay it after Hello.
    opened: Option<HostReply>,
    /// A serving child's wake report (descriptor 5), the launcher's read end.
    report: Option<File>,
    /// Standard output of a child with a bounded answer.
    answer: Option<(File, u64)>,
    install_target: Option<u64>,
}

struct Ordinary {
    pid: libc::pid_t,
    pidfd: OwnedFd,
    group: Option<cgroup::Group>,
    status: Option<Status>,
}

enum Running {
    Ordinary(Ordinary),
    Contained(hirsel_contain::Contained),
}

impl Running {
    fn pidfd(&self) -> RawFd {
        match self {
            Self::Ordinary(child) => child.pidfd.as_raw_fd(),
            Self::Contained(child) => child.pidfd().as_raw_fd(),
        }
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        match self {
            Self::Contained(child) if signal == libc::SIGKILL => child.kill(),
            Self::Contained(child) => child.signal(signal),
            Self::Ordinary(child) => match (&child.group, signal, &child.status) {
                (Some(group), libc::SIGKILL, _) => group.kill(),
                (_, _, Some(_)) => Ok(()),
                _ => send(child.pidfd.as_raw_fd(), signal),
            },
        }
    }

    fn wait(self) -> io::Result<Status> {
        match self {
            Self::Ordinary(mut child) => {
                child.reap_leader()?;
                if let Some(group) = &child.group {
                    group.wait_empty()?;
                }
                child.finish()
            }
            Self::Contained(child) => match child.wait()? {
                hirsel_contain::End::Exited(status) => Ok(status_of(status)),
                hirsel_contain::End::Setup { step, errno } => {
                    eprintln!(
                        "contained launch failed at {step}: {}",
                        io::Error::from_raw_os_error(errno)
                    );
                    Ok(Status::Code(127))
                }
                hirsel_contain::End::Killed => Ok(Status::Signal(libc::SIGKILL)),
            },
        }
    }
}

impl Ordinary {
    fn reap_leader(&mut self) -> io::Result<()> {
        if self.status.is_none() {
            self.status = Some(status_of(wait_pid(self.pid)?));
            if let Some(group) = &self.group {
                group.kill()?;
            }
        }
        Ok(())
    }

    fn finish(self) -> io::Result<Status> {
        if let Some(group) = self.group {
            group.reap_orphans()?;
            group.remove()?;
        }
        Ok(self.status.expect("leader reaped"))
    }
}

impl Process {
    fn complete(&mut self, epoll: &Epoll) -> io::Result<Option<Status>> {
        self.watch.take();
        if let Some(Running::Ordinary(child)) = &mut self.child {
            child.reap_leader()?;
            if let Some(group) = &child.group
                && !group.empty()?
            {
                let mut watch = epoll.watch(group.event_fd());
                watch.set(CHANGE)?;
                self.watch = Some(watch);
                return Ok(None);
            }
        }
        match self.child.take().expect("a live child") {
            Running::Ordinary(child) => child.finish().map(Some),
            child => child.wait().map(Some),
        }
    }
}

fn status_of(status: ExitStatus) -> Status {
    match status.code() {
        Some(code) => Status::Code(code),
        None => Status::Signal(status.signal().unwrap_or(0)),
    }
}

pub(super) fn wait_pid(pid: libc::pid_t) -> io::Result<ExitStatus> {
    let mut status = 0;
    loop {
        // SAFETY: the caller owns this child and waitpid writes one status.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            return Ok(ExitStatus::from_raw(status));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn send(pidfd: RawFd, signal: i32) -> io::Result<()> {
    // SAFETY: a pidfd signal reaches only the process it pins.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd,
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if rc != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

struct Kept {
    state: ChildState,
    process: Option<Process>,
    install_reply: Option<HostReply>,
}

/// A cutover forwarded to OLD, kept from first forward until the host's `CutoverAck`.
struct Forwarded {
    command: HostCommand,
    reply: Option<HostReply>,
}

/// Admission state: the stores, the running workers, and the notes they send.
struct Admitting {
    admission: Arc<Admission>,
    workers: BTreeMap<Attempt, admit::Worker>,
    next: u64,
    notes: mpsc::Receiver<admit::Note>,
    sender: mpsc::Sender<admit::Note>,
    wake: File,
    waker: Arc<File>,
    _watch: Watch,
}

pub(crate) struct Launcher<I> {
    images: I,
    containment: Option<hirsel_contain::Host>,
    cgroup_root: Option<PathBuf>,
    epoll: Epoll,
    scratch: PathBuf,
    listeners: Vec<Listening>,
    children: Vec<Kept>,
    out: Vec<Event>,
    fenced: bool,
    reservations: BTreeMap<Attempt, BTreeSet<[u8; 32]>>,
    deferred: Vec<Attempt>,
    admitting: Option<Admitting>,
    cutovers: BTreeMap<(Child, Attempt), Forwarded>,
    control_watch: Option<Watch>,
    control: Option<UnixListener>,
    session: Option<Conn>,
    /// The platform host, when this launcher starts it; then only its process may hold the
    /// control session.
    bootstrap: Option<bootstrap::Bootstrap>,
    _lock: File,
}

/// Take the launcher lock in `state`: the OS lock that fences a lost launcher. The lock is
/// released when its holder dies.
pub(crate) fn lock(state: &Path) -> io::Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("launcher.lock"))?;
    file.try_lock().map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "another launcher holds this state directory",
        )
    })?;
    Ok(file)
}

fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open pins a kernel process identity, with no signal.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pidfd_open returns a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Whether `fd` is readable now, without waiting.
fn readable(fd: RawFd) -> bool {
    let mut record = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll borrows one initialized record and does not wait.
    unsafe { libc::poll(&raw mut record, 1, 0) > 0 && record.revents & libc::POLLIN != 0 }
}

fn above_launch_slots(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor above every launch slot.
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, spawn::IMAGE_FD + 1) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is fresh and owned.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn pipe() -> io::Result<(File, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills two fresh descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are fresh and owned here.
    let (read, write) = unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // The writer is the child's; only the reader stays nonblocking.
    // SAFETY: fcntl on an owned descriptor.
    unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, 0) };
    Ok((read, write))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A wake report: an empty read is no report, and anything but one `Option[u64]` is none.
fn read_report(file: &mut File) -> Option<Option<u64>> {
    let mut bytes = Vec::new();
    let _ = file.take(REPORT_BYTES + 1).read_to_end(&mut bytes);
    match bytes.as_slice() {
        [0, 0, 0, 0] => Some(None),
        [1, 0, 0, 0, due @ ..] if due.len() == 8 => {
            Some(Some(u64::from_le_bytes(due.try_into().unwrap())))
        }
        _ => None,
    }
}

/// Native images and the serving activation wrapper use the system loader and
/// support libraries. Bind individual files so sibling host files stay hidden.
fn runtime_files(image: &Path) -> Vec<PathBuf> {
    let (multiarch, loader) = if cfg!(target_arch = "aarch64") {
        ("aarch64-linux-gnu", "/lib/ld-linux-aarch64.so.1")
    } else {
        ("x86_64-linux-gnu", "/lib64/ld-linux-x86-64.so.2")
    };
    let mut files = vec![image.to_path_buf(), PathBuf::from(loader)];
    for dir in ["/lib", "/lib64", "/usr/lib", "/usr/lib64"] {
        for dir in [PathBuf::from(dir), Path::new(dir).join(multiarch)] {
            files.extend(
                [
                    "libc.so.6",
                    "libgcc_s.so.1",
                    "libm.so.6",
                    "libpthread.so.0",
                    "libdl.so.2",
                    "librt.so.1",
                    "libutil.so.1",
                ]
                .into_iter()
                .map(|name| dir.join(name))
                .filter(|path| path.is_file()),
            );
        }
    }
    files
}

fn container_id(launch: &Launch) -> hirsel_contain::ChildId {
    use hirsel_contain::{ChildId, Mode};
    let (role, key) = match launch.mode.role() {
        Role::Install(generation) => (Mode::Install, u128::from(generation)),
        Role::Serving(generation) => (Mode::Serving, u128::from(generation)),
        Role::Describe(a) => (Mode::Describe, u128::from_le_bytes(a.bytes())),
        Role::Preview(a) => (Mode::Preview, u128::from_le_bytes(a.bytes())),
        Role::Prepare(a) => (Mode::Prepare, u128::from_le_bytes(a.bytes())),
    };
    ChildId {
        group: launch.group.name.clone(),
        incarnation: launch.group.incarnation,
        key,
        role,
    }
}

impl<I: Images> Launcher<I> {
    /// A launcher over `state`, which it locks before it opens its objects with `open`;
    /// `state/scratch` holds each group's private scratch and export directories.
    pub(crate) fn new(
        state: &Path,
        cgroup_root: Option<&Path>,
        open: impl FnOnce() -> io::Result<I>,
    ) -> io::Result<Self> {
        let lock = lock(state)?;
        let containment = cgroup_root
            .map(|root| {
                let host = hirsel_contain::Host::open(root, state).map_err(|why| {
                    io::Error::other(format!("cannot contain in {}: {why:?}", root.display()))
                })?;
                host.recover()?;
                Ok::<_, io::Error>(host)
            })
            .transpose()?;
        let cgroup_root = match cgroup_root {
            Some(path) => Some(path.to_owned()),
            None => {
                let root = cgroup::delegated(state)?;
                if let Some(root) = &root {
                    cgroup::recover(root)?;
                }
                root
            }
        };
        if cgroup_root.is_some() {
            cgroup::own_orphans()?;
        }
        let epoll = Epoll::new()?;
        let images = open()?;
        let scratch = state.join("scratch");
        // A lost launcher's staging is not state: remove it.
        match std::fs::remove_dir_all(&scratch) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::create_dir_all(&scratch)?;
        Ok(Self {
            images,
            containment,
            cgroup_root,
            epoll,
            scratch,
            listeners: Vec::new(),
            children: Vec::new(),
            out: Vec::new(),
            fenced: false,
            reservations: BTreeMap::new(),
            deferred: Vec::new(),
            admitting: None,
            cutovers: BTreeMap::new(),
            control_watch: None,
            control: None,
            session: None,
            bootstrap: None,
            _lock: lock,
        })
    }

    /// Serve the control session on `listener`; the socket's permission is the host's.
    pub(crate) fn listen(&mut self, listener: UnixListener) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        let mut watch = self.epoll.watch(listener.as_raw_fd());
        watch.set(READ)?;
        self.control_watch = Some(watch);
        self.control = Some(listener);
        Ok(())
    }

    /// Admit releases with `admission`'s stores; without it every `Admit` fails.
    pub(crate) fn admit_with(&mut self, admission: Admission) -> io::Result<()> {
        let (wake, write) = pipe()?;
        let mut watch = self.epoll.watch(wake.as_raw_fd());
        watch.set(READ)?;
        let (sender, notes) = mpsc::channel();
        self.admitting = Some(Admitting {
            admission: Arc::new(admission),
            workers: BTreeMap::new(),
            next: 0,
            notes,
            sender,
            wake,
            waker: admit::waker(write)?,
            _watch: watch,
        });
        Ok(())
    }

    /// Keep the objects that `attempt` synced until the host releases it. Admission calls
    /// this once steps 1-3 of "Durable publication" are synced.
    pub(crate) fn reserve(&mut self, attempt: Attempt, objects: BTreeSet<[u8; 32]>) {
        self.reservations
            .entry(attempt)
            .or_default()
            .extend(objects);
    }

    /// The events not yet sent, for a caller that drives the launcher without a session.
    #[cfg(test)]
    pub(crate) fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.out)
    }

    /// The group's scratch directory, which a serving child gets as `HIRSEL_SCRATCH` and in
    /// which a prepare child's export directory lies.
    pub(crate) fn scratch_of(&self, group: &Group) -> PathBuf {
        self.scratch.join(format!(
            "{}-{}",
            hex(group.name.as_bytes()),
            group.incarnation
        ))
    }

    fn emit(&mut self, event: Event) {
        self.out.push(event);
    }

    fn listener(&self, group: &Group) -> Option<usize> {
        self.listeners.iter().position(|l| l.group == *group)
    }

    fn child(&self, child: &Child) -> Option<usize> {
        self.children.iter().position(|k| k.state.child == *child)
    }

    fn live(&self, group: &Group) -> impl Iterator<Item = &Child> {
        self.children
            .iter()
            .filter(move |k| k.process.is_some() && k.state.child.group == *group)
            .map(|k| &k.state.child)
    }

    fn serving_live(&self, group: &Group) -> bool {
        self.live(group).any(|c| matches!(c.role, Role::Serving(_)))
    }

    /// RUNNER-SPEC §14.3: a store-holding launch is refused while another lives, unless it
    /// is the one candidate that names the live serving child's gen.
    fn one_launch(&self, launch: &Launch) -> bool {
        if !launch.mode.role().holds_store() {
            return true;
        }
        let candidate = match &launch.mode {
            Mode::Serving(_, serve) => serve.candidate,
            _ => None,
        };
        let holders: Vec<_> = self
            .live(&launch.group)
            .filter(|c| c.role.holds_store())
            .collect();
        match holders.as_slice() {
            [] => true,
            [only] => candidate.is_some_and(|old| only.role == Role::Serving(old)),
            _ => false,
        }
    }

    /// Answer one request. Its events follow every event already queued.
    pub(crate) fn handle(&mut self, request: Request) {
        match request {
            Request::Hello => self.hello(),
            Request::Bind(group, address, label, bindings, limits) => {
                self.bind(group, &address, label, bindings, limits)
            }
            Request::Close(group) => {
                if let Some(i) = self.listener(&group) {
                    self.listeners.swap_remove(i);
                }
                self.emit(Event::Closed(group));
            }
            Request::Start(launch) => self.start(launch),
            Request::Stop(child) => {
                let sig = if matches!(child.role, Role::Install(_)) {
                    libc::SIGKILL
                } else {
                    libc::SIGTERM
                };
                self.signal(&child, sig);
            }
            Request::Kill(child) => self.signal(&child, libc::SIGKILL),
            Request::Acknowledge(child) => {
                if let Some(i) = self.child(&child)
                    && self.children[i].process.is_none()
                {
                    self.children.swap_remove(i);
                }
            }
            Request::Command(child, command) => self.command(child, &command),
            Request::Retire(group) => {
                if let Some(i) = self.listener(&group) {
                    self.listeners[i].retiring = true;
                }
                self.retired(&group);
            }
            Request::Release(attempt) => {
                if let Some(admitting) = &mut self.admitting {
                    admitting.workers.remove(&attempt);
                }
                if self.fenced {
                    self.deferred.push(attempt);
                } else {
                    self.reservations.remove(&attempt);
                }
            }
            Request::CollectBegin => {
                self.fenced = true;
                self.emit(Event::Fenced);
            }
            Request::Collect(roots) => self.collect(&roots),
            Request::Admit(group, attempt, input, approvals) => {
                self.admit(group, attempt, input, approvals)
            }
            Request::AdmitData(attempt, bytes) => self.admit_data(attempt, &bytes),
            Request::AdmitEnd(attempt) => self.admit_end(attempt),
            Request::Assemble(attempt) => self.assemble(attempt),
            Request::CutoverAck(child, attempt) => drop(self.cutovers.remove(&(child, attempt))),
            Request::CollectAbort => {
                self.unfence();
                self.emit(Event::Collected(0));
            }
        }
    }

    fn admission_failed(&mut self, attempt: Attempt, why: impl Into<String>) {
        if let Some(admitting) = &mut self.admitting {
            admitting.workers.remove(&attempt);
        }
        self.emit(Event::AdmissionFailed(
            attempt,
            Failure::Invalid(why.into()),
        ));
    }

    fn admit(&mut self, group: Group, attempt: Attempt, input: Input, approvals: Approvals) {
        let refusal = match (&self.admitting, self.listener(&group)) {
            (None, _) => Some("the launcher has no code store"),
            (_, None) => Some("unknown group"),
            (_, Some(i)) if self.listeners[i].retiring => Some("the group is retiring"),
            (Some(a), _) if a.workers.contains_key(&attempt) => Some("the attempt is admitting"),
            (Some(a), _) if a.workers.len() >= ADMISSIONS => {
                Some("busy: every admission slot runs")
            }
            _ => None,
        };
        if let Some(why) = refusal {
            return self.emit(Event::AdmissionFailed(
                attempt,
                Failure::Invalid(why.into()),
            ));
        }
        let root = |r: &[u8]| {
            <[u8; 32]>::try_from(r)
                .ok()
                .map(hirsel_core_ids::ContentHash)
        };
        let source = match &input {
            Input::Attested(r) => root(r).map(admit::Source::Attested),
            Input::Publication(r) => root(r).map(admit::Source::Publication),
            Input::Source(_) => {
                return self.emit(Event::AdmissionFailed(
                    attempt,
                    Failure::Invalid("source admission is not built".into()),
                ));
            }
        };
        let Some(source) = source else {
            return self.emit(Event::AdmissionFailed(
                attempt,
                Failure::Invalid("the recipe root is not 32 bytes".into()),
            ));
        };
        let admitting = self.admitting.as_mut().expect("checked above");
        let id = admitting.next;
        admitting.next += 1;
        let (sender, waker) = (admitting.sender.clone(), admitting.waker.clone());
        let worker = admit::Worker::spawn(
            admitting.admission.clone(),
            &self.epoll,
            id,
            attempt,
            source,
            approvals,
            move |note| {
                let _ = sender.send(note);
                admit::wake(&waker);
            },
        );
        match worker {
            Ok(worker) => drop(admitting.workers.insert(attempt, worker)),
            Err(e) => self.admission_failed(attempt, format!("admission worker: {e}")),
        }
    }

    fn admit_data(&mut self, attempt: Attempt, bytes: &[u8]) {
        let Some(admitting) = &mut self.admitting else {
            return self.admission_failed(attempt, "the launcher has no code store");
        };
        let bound = admitting.admission.input_bound();
        let why = match admitting.workers.get_mut(&attempt) {
            None => "unknown attempt",
            Some(worker) if worker.ending => "input after AdmitEnd",
            Some(worker) => match &mut worker.conn {
                None => "a publication takes no input",
                Some(_) if worker.received.saturating_add(bytes.len() as u64) > bound => {
                    "admission input exceeds its bound"
                }
                Some(conn) => {
                    worker.received += bytes.len() as u64;
                    conn.output.extend_from_slice(bytes);
                    return;
                }
            },
        };
        self.admission_failed(attempt, why);
    }

    fn admit_end(&mut self, attempt: Attempt) {
        match self
            .admitting
            .as_mut()
            .and_then(|a| a.workers.get_mut(&attempt))
        {
            Some(worker) => worker.ending = true,
            None => self.admission_failed(attempt, "unknown attempt"),
        }
    }

    fn assemble(&mut self, attempt: Attempt) {
        match self
            .admitting
            .as_mut()
            .and_then(|a| a.workers.get_mut(&attempt))
        {
            Some(worker) if worker.admitted => worker.assemble(),
            _ => self.admission_failed(attempt, "the attempt is not admitted"),
        }
    }

    /// Apply the workers' notes. A note of a cancelled worker is dropped: its assembly
    /// record is then no reservation's and the next collection may remove it.
    fn notes(&mut self) {
        let Some(admitting) = &mut self.admitting else {
            return;
        };
        let mut drained = [0; 64];
        while matches!((&admitting.wake).read(&mut drained), Ok(n) if n > 0) {}
        let mut events = Vec::new();
        let mut reserved = Vec::new();
        while let Ok(note) = admitting.notes.try_recv() {
            let (id, attempt) = match &note {
                admit::Note::Admitted { id, attempt, .. }
                | admit::Note::Assembled { id, attempt, .. }
                | admit::Note::Failed(id, attempt, _) => (*id, *attempt),
            };
            if admitting.workers.get(&attempt).is_none_or(|w| w.id != id) {
                continue;
            }
            match note {
                admit::Note::Admitted {
                    root,
                    summary,
                    display,
                    ..
                } => {
                    admitting.workers.get_mut(&attempt).unwrap().admitted = true;
                    events.push(Event::Admitted(attempt, root, summary, display));
                }
                admit::Note::Assembled {
                    key,
                    digest,
                    fetch,
                    assembly,
                    ..
                } => {
                    admitting.workers.remove(&attempt);
                    reserved.push((attempt, key));
                    events.push(Event::Assembled(
                        attempt,
                        key.to_vec(),
                        digest,
                        fetch,
                        assembly,
                    ));
                }
                admit::Note::Failed(_, _, why) => {
                    admitting.workers.remove(&attempt);
                    events.push(Event::AdmissionFailed(attempt, why));
                }
            }
        }
        for (attempt, key) in reserved {
            self.reserve(attempt, BTreeSet::from([key]));
        }
        self.out.extend(events);
    }

    /// Move each attested worker's input and Need.
    fn pump(&mut self) {
        let Some(admitting) = &mut self.admitting else {
            return;
        };
        let mut events = Vec::new();
        for (&attempt, worker) in &mut admitting.workers {
            for (keys, last) in worker.pump() {
                events.push(Event::Need(attempt, keys, last));
            }
        }
        self.out.extend(events);
    }

    /// Whether some attested input waits on its worker past the window, so the session is
    /// not read until the worker takes it.
    fn backlogged(&self) -> bool {
        self.admitting.as_ref().is_some_and(|a| {
            a.workers.values().any(|w| {
                w.conn
                    .as_ref()
                    .is_some_and(|c| c.output.len() > session::MAX_FRAME)
            })
        })
    }

    /// End the fence and apply the releases it deferred.
    fn unfence(&mut self) {
        self.fenced = false;
        for attempt in std::mem::take(&mut self.deferred) {
            self.reservations.remove(&attempt);
        }
    }

    fn hello(&mut self) {
        // Hello ends the old session: its unsent events are dropped, and its fence ends
        // without removing anything.
        self.out.clear();
        self.unfence();
        // A new session ends every reservation and admission of the old one (§14.5).
        self.reservations.clear();
        if let Some(admitting) = &mut self.admitting {
            admitting.workers.clear();
        }
        let children = self.children.iter().map(|k| k.state.clone()).collect();
        let mut cold = Vec::new();
        for i in 0..self.listeners.len() {
            let group = self.listeners[i].group.clone();
            if !self.serving_live(&group) && readable(self.listeners[i].socket.as_raw_fd()) {
                self.listeners[i].reported = true;
                cold.push(group);
            }
        }
        let cutovers = self.cutovers.keys().cloned().collect();
        self.emit(Event::Inventory(children, cold, cutovers));
        for entry in &self.children {
            let serving_open = entry.process.as_ref().and_then(|p| p.opened.as_ref());
            if let Some(reply) = serving_open.or(entry.install_reply.as_ref()) {
                self.out
                    .push(Event::Placed(entry.state.child.clone(), reply.clone()));
            }
        }
    }

    fn bind(
        &mut self,
        group: Group,
        address: &str,
        label: Label,
        bindings: Bindings,
        limits: Limits,
    ) {
        if label == Label::Contained {
            if self.containment.is_none() {
                return self.emit(Event::BindFailed(
                    group,
                    "contained: no delegated cgroup root".into(),
                ));
            }
            if bindings.store.is_empty() || bindings.dirs.iter().any(String::is_empty) {
                return self.emit(Event::BindFailed(
                    group,
                    "contained: empty directory binding".into(),
                ));
            }
        }
        if let Some(held) = self.listeners.iter().find(|l| l.group.name == group.name) {
            let event = if held.group.incarnation != group.incarnation {
                let held = held.group.incarnation;
                Event::BindFailed(group, format!("the name is bound at incarnation {held}"))
            } else if (held.label, &held.bindings, held.limits) != (label, &bindings, limits) {
                Event::BindFailed(
                    group,
                    "the group already has different bindings, limits or label".into(),
                )
            } else {
                Event::Bound(group)
            };
            return self.emit(event);
        }
        if self.live(&group).next().is_some() {
            return self.emit(Event::BindFailed(
                group,
                "the group has live children".into(),
            ));
        }
        match TcpListener::bind(address) {
            Ok(socket) => {
                self.listeners.push(Listening {
                    watch: self.epoll.watch(socket.as_raw_fd()),
                    group: group.clone(),
                    socket,
                    label,
                    bindings,
                    limits,
                    reported: false,
                    retiring: false,
                });
                self.emit(Event::Bound(group));
            }
            Err(e) => self.emit(Event::BindFailed(group, e.to_string())),
        }
    }

    fn start(&mut self, launch: Launch) {
        let child = launch.child();
        if let Some(i) = self.child(&child) {
            let phase = self.children[i].state.phase.clone();
            self.emit(Event::Launched(child.clone(), phase));
            if let Some(reply) = self.children[i].install_reply.clone() {
                self.emit(Event::Placed(child, reply));
            }
            return;
        }
        let refusal = match self.listener(&launch.group) {
            None => Some(Refusal::UnknownGroup),
            Some(i) if self.listeners[i].retiring => Some(Refusal::Retiring),
            Some(_) if launch.assembly.len() != 32 => Some(Refusal::Mismatch),
            Some(_)
                if launch.recording.as_ref().is_some_and(|r| {
                    r.id.len() != 16 || !matches!(launch.mode, Mode::Serving(..))
                }) =>
            {
                Some(Refusal::Mismatch)
            }
            Some(_) if matches!(launch.mode, Mode::Install(_, InstallKind::Standalone(0))) => {
                Some(Refusal::Mismatch)
            }
            Some(_) if !self.one_launch(&launch) => Some(Refusal::Held),
            Some(_) => None,
        };
        if let Some(why) = refusal {
            return self.emit(Event::Refused(child, why));
        }
        let image = match self
            .images
            .resolve(launch.assembly.as_slice().try_into().unwrap())
        {
            Ok(image) => image,
            Err(why) => return self.emit(Event::Refused(child, Refusal::Image(why))),
        };
        let listener = &self.listeners[self.listener(&launch.group).expect("a bound group")];
        match self.spawn(&launch, &image, listener) {
            Ok(process) => {
                if let Role::Serving(_) = child.role
                    && let Some(i) = self.listener(&launch.group)
                {
                    self.listeners[i].reported = false;
                }
                self.children.push(Kept {
                    state: ChildState {
                        child: child.clone(),
                        phase: Phase::Started,
                    },
                    process: Some(process),
                    install_reply: None,
                });
                self.emit(Event::Launched(child, Phase::Started));
            }
            Err(e) => self.emit(Event::Refused(child, Refusal::Spawn(e.to_string()))),
        }
    }

    /// Start one child with its mode's row of launch.md "Child modes": the serving child
    /// sees its listener (3), private endpoint (4) and wake report (5); the other modes
    /// see descriptors 0-2. The runtime consumes the verified image descriptor at entry.
    fn spawn(&self, launch: &Launch, image: &Image, listener: &Listening) -> io::Result<Process> {
        let workdir = Path::new(&launch.workdir);
        let n_place = format!("--hirsel-n-place={}", image.n_place);
        let mut command = Command::new(&image.path);
        let mut slots: Vec<(OwnedFd, RawFd)> = Vec::new();
        let mut endpoint = None;
        let mut report = None;
        let mut answer = None;
        let mut install_target = None;
        let mut input = None;
        let mut export_path = None;
        let scratch = self.scratch_of(&launch.group);
        let host_pid = if listener.label == Label::Contained {
            1
        } else {
            std::process::id()
        };
        match &launch.mode {
            Mode::Serving(_, serve) => {
                if listener.label == Label::Contained {
                    command = Command::new("/bin/sh");
                    command
                        .args([
                            "-c",
                            "LISTEN_PID=$$ LISTEN_FDS=1 exec \"$@\"",
                            "hirsel-place",
                        ])
                        .arg(&image.path);
                }
                command.arg("--hirsel-wake");
                if serve.idle != 0 {
                    command.arg(format!("--hirsel-idle={}", serve.idle));
                }
                if let Some((hi, lo)) = serve.store_id {
                    command.arg(format!("--hirsel-store-id={hi:016x}{lo:016x}"));
                }
                command
                    .args(&image.prefix)
                    .arg(&n_place)
                    .arg(format!("--hirsel-activation={}", serve.activation))
                    .args(&launch.args);
                std::fs::create_dir_all(&scratch)?;
                command
                    .env("HIRSEL_CONTROL_FD", "4")
                    .env("HIRSEL_SCRATCH", &scratch)
                    .env("HIRSEL_HOST_PID", host_pid.to_string());
                if let Some(recording) = &launch.recording {
                    command
                        .env("HIRSEL_RECORD", &recording.destination)
                        .env("HIRSEL_RECORD_ID", hex(&recording.id));
                }
                let (ours, theirs) = UnixStream::pair()?;
                let (read, write) = pipe()?;
                slots.push((above_launch_slots(listener.socket.as_raw_fd())?, 3));
                slots.push((above_launch_slots(theirs.as_raw_fd())?, 4));
                slots.push((above_launch_slots(write.as_raw_fd())?, 5));
                endpoint = Some(Conn::new(&self.epoll, ours)?);
                report = Some(read);
            }
            Mode::Describe(_) | Mode::Preview(_, _) => {
                let flag = if let Mode::Preview(..) = launch.mode {
                    "--hirsel-preview"
                } else {
                    "--hirsel-describe"
                };
                command.args(&image.prefix).arg(&n_place).arg(flag);
                let out = tempfile::tempfile()?;
                answer = Some((out, image.n_place));
                if let Mode::Preview(_, old) = &launch.mode {
                    let mut old_entries = tempfile::tempfile()?;
                    old_entries.write_all(old)?;
                    old_entries.rewind()?;
                    input = Some(old_entries);
                }
            }
            Mode::Prepare(attempt, export) => {
                if export.is_empty()
                    || !export
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "export name"));
                }
                let path = scratch.join(export);
                command
                    .args(&image.prefix)
                    .arg(&n_place)
                    .arg("--hirsel-prepare")
                    .arg(&path)
                    .arg(hex(&attempt.bytes()))
                    .arg(hex(&image.digest));
                export_path = Some(path);
            }
            Mode::Install(_, kind) => {
                let selection = match kind {
                    InstallKind::Genesis => 0,
                    InstallKind::Standalone(activation) => *activation,
                };
                command
                    .args(&image.prefix)
                    .arg(&n_place)
                    .arg(format!("--hirsel-install-child={selection}"))
                    .args(&launch.args)
                    .env("HIRSEL_HOST_PID", host_pid.to_string());
                install_target = Some(selection.max(1));
                // HostReply::InstallRefused(WrongVersion) is the largest answer.
                answer = Some((tempfile::tempfile()?, 16));
            }
        }
        let child = if listener.label == Label::Contained {
            if let Some(input) = input {
                slots.push((input.into(), 0));
            }
            if let Some((out, _)) = &answer {
                slots.push((out.try_clone()?.into(), 1));
            }
            Running::Contained(self.spawn_contained(
                launch,
                listener,
                image,
                &command,
                slots,
                export_path.as_deref(),
            )?)
        } else {
            let null = File::options().read(true).write(true).open("/dev/null")?;
            let stdin = input.as_ref().unwrap_or(&null);
            let stdout = answer.as_ref().map_or(&null, |(out, _)| out);
            for (source, target) in [(stdin, 0), (stdout, 1), (&null, 2)] {
                slots.push((above_launch_slots(source.as_raw_fd())?, target));
            }
            let serving = matches!(launch.mode, Mode::Serving(..));
            if !serving {
                command
                    .env_remove("HIRSEL_CONTROL_FD")
                    .env_remove("HIRSEL_SCRATCH");
            }
            command
                .env_remove("LISTEN_FDS")
                .env_remove("LISTEN_PID")
                .env_remove("LISTEN_FDNAMES")
                .env_remove("LISTEN_PIDFDID");
            // Only a recorded serving start records: its destination was set above, and no
            // other child inherits one from the launcher's own environment.
            if launch.recording.is_none() {
                command
                    .env_remove("HIRSEL_RECORD")
                    .env_remove("HIRSEL_RECORD_ID");
            }
            if serving {
                command.env("LISTEN_FDS", "1");
            }
            match &image.executable {
                ImageFile::Verified(file) => {
                    let slot = spawn::IMAGE_FD;
                    slots.push((above_launch_slots(file.as_raw_fd())?, slot));
                    let parent = std::process::id();
                    let digest = hex(&image.digest);
                    command.env(LAUNCH_IMAGE_ENV, format!("{parent}:{slot}:{digest}"));
                }
                #[cfg(test)]
                ImageFile::Script => {
                    command.env_remove(LAUNCH_IMAGE_ENV);
                }
            }
            let group = self
                .cgroup_root
                .as_ref()
                .map(|root| cgroup::Group::create(root, &container_id(launch)))
                .transpose()?;
            let started = (|| {
                let directory = group.as_ref().map(cgroup::Group::directory).transpose()?;
                spawn::start(&command, workdir, &slots, directory.as_ref(), serving)
            })();
            let (pid, pidfd) = match started {
                Ok(started) => started,
                Err(error) => {
                    if let Some(group) = group {
                        group.kill()?;
                        group.wait_empty()?;
                        group.reap_orphans()?;
                        group.remove()?;
                    }
                    return Err(error);
                }
            };
            Running::Ordinary(Ordinary {
                pid,
                pidfd,
                group,
                status: None,
            })
        };
        let mut watch = self.epoll.watch(child.pidfd());
        if let Err(error) = watch.set(READ) {
            if child.signal(libc::SIGKILL).is_ok() {
                let _ = child.wait();
            }
            return Err(error);
        }
        Ok(Process {
            watch: Some(watch),
            child: Some(child),
            endpoint,
            opened: None,
            report,
            answer,
            install_target,
        })
    }

    fn spawn_contained(
        &self,
        launch: &Launch,
        listener: &Listening,
        image: &Image,
        command: &Command,
        slots: Vec<(OwnedFd, RawFd)>,
        export: Option<&Path>,
    ) -> io::Result<hirsel_contain::Contained> {
        use hirsel_contain::{Authority, LaunchError};
        let host = self
            .containment
            .as_ref()
            .expect("a contained bind requires a host");
        let id = container_id(launch);
        let bindings = hirsel_contain::Bindings {
            store: PathBuf::from(&listener.bindings.store),
            dirs: listener.bindings.dirs.iter().map(PathBuf::from).collect(),
            net: listener.bindings.net.clone(),
        };
        let authority = Authority::of(id.role, &bindings, export)
            .map_err(|why| io::Error::other(format!("contained authority: {why:?}")))?
            .with_serving_scratch(&self.scratch_of(&launch.group));
        let args: Vec<_> = command.get_args().map(OsString::from).collect();
        let env: Vec<_> = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_owned(), value?.to_owned())))
            .collect();
        let limits = hirsel_contain::Limits {
            memory: listener.limits.memory,
            cpu: listener.limits.cpu,
            pids: listener.limits.pids,
        };
        host.launch(hirsel_contain::Launch {
            id: &id,
            authority: &authority,
            program: Path::new(command.get_program()),
            args: &args,
            env: &env,
            workdir: &std::path::absolute(&launch.workdir)?,
            read_only: &runtime_files(&image.path),
            fds: slots.into_iter().map(|(fd, target)| (target, fd)).collect(),
            limits,
        })
        .map_err(|error| match error {
            LaunchError::Io(error) => error,
            LaunchError::Refused(why) => io::Error::other(format!("contained launch: {why:?}")),
        })
    }

    fn signal(&mut self, child: &Child, signal: i32) {
        if let Some(i) = self.child(child)
            && let Some(process) = &self.children[i].process
            && let Some(running) = &process.child
        {
            let _ = running.signal(signal);
        }
    }

    fn command(&mut self, child: Child, command: &HostCommand) {
        // A cutover is kept by (OLD, attempt): an identical re-send is forwarded once and
        // answered with OLD's saved reply.
        let key = match command {
            HostCommand::Cutover(a, ..) => <[u8; 16]>::try_from(a.as_slice()).ok().map(|a| {
                let hi = u64::from_le_bytes(a[..8].try_into().unwrap());
                let lo = u64::from_le_bytes(a[8..].try_into().unwrap());
                (child.clone(), Attempt { hi, lo })
            }),
            _ => None,
        };
        if let Some(key) = &key
            && let Some(held) = self.cutovers.get(key)
        {
            if held.command != *command {
                return self.emit(Event::Placed(
                    child,
                    HostReply::Refused("a different cutover holds this attempt".into()),
                ));
            }
            if let Some(reply) = held.reply.clone() {
                self.emit(Event::CutoverResult(key.0.clone(), key.1, reply));
            }
            return;
        }
        let process = self
            .child(&child)
            .and_then(|i| self.children[i].process.as_mut());
        let accepting = *command == HostCommand::StoreAccepted;
        let endpoint = process.and_then(|p| {
            if accepting {
                p.opened.take()?;
            }
            p.endpoint.as_mut()
        });
        match endpoint {
            Some(conn) => {
                conn.queue(command);
                conn.flush();
                if let Some(key) = key {
                    self.cutovers.insert(
                        key,
                        Forwarded {
                            command: command.clone(),
                            reply: None,
                        },
                    );
                }
            }
            None if accepting => (),
            None => self.emit(Event::Placed(
                child,
                HostReply::Refused("the child has no live endpoint".into()),
            )),
        }
    }

    fn retired(&mut self, group: &Group) {
        if let Some(i) = self.listener(group)
            && self.listeners[i].retiring
            && self.live(group).next().is_none()
        {
            self.emit(Event::Retired(group.clone()));
        }
    }

    fn collect(&mut self, roots: &[Vec<u8>]) {
        if !self.fenced {
            return self.emit(Event::Collected(0));
        }
        // No worker writes a record while the roots are read and removed, and every
        // record already written has its note applied, so its reservation is held.
        let admission = self.admitting.as_ref().map(|a| a.admission.clone());
        let _publishing = admission
            .as_ref()
            .map(|a| a.publish.lock().unwrap_or_else(|e| e.into_inner()));
        self.notes();
        let mut keep = BTreeSet::new();
        let mut whole = true;
        for root in roots {
            match <[u8; 32]>::try_from(root.as_slice()) {
                Ok(root) => drop(keep.insert(root)),
                Err(_) => whole = false,
            }
        }
        for objects in self.reservations.values() {
            keep.extend(objects);
        }
        // A launcher without a whole root set removes nothing.
        // Code objects an admission has reserved but not yet published stay, under the
        // admission store's collection lock.
        let removed = match (&admission, whole) {
            (_, false) => Ok(0),
            (Some(a), true) => a.under_collection(|reserved| self.images.collect(&keep, reserved)),
            (None, true) => self.images.collect(&keep, &|_| false),
        };
        let removed = removed.unwrap_or(0);
        self.unfence();
        self.emit(Event::Collected(removed));
    }

    /// Relay every whole frame a child's endpoint holds.
    fn drain_endpoint(&mut self, i: usize) {
        let Some(conn) = self.children[i]
            .process
            .as_mut()
            .and_then(|p| p.endpoint.as_mut())
        else {
            return;
        };
        conn.fill();
        let mut replies = Vec::new();
        while let Some(frame) = conn.frame() {
            match HostReply::decode(&frame) {
                Ok(reply) => replies.push(reply),
                Err(_) => conn.closed = true,
            }
        }
        let child = self.children[i].state.child.clone();
        for reply in replies {
            if matches!(reply, HostReply::Opened(_))
                && let Some(process) = self.children[i].process.as_mut()
            {
                process.opened = Some(reply.clone());
            }
            if let HostReply::StoreMismatch(ehi, elo, ahi, alo) = reply {
                eprintln!(
                    "hirsel host: group {}: store identity mismatch (expected {ehi:016x}{elo:016x}, found {ahi:016x}{alo:016x})",
                    child.group.name
                );
            }
            // OLD's next answer after a forwarded cutover is that cutover's; a held-write
            // acknowledgement is a notice.
            if !matches!(
                reply,
                HostReply::Opened(_) | HostReply::WriteAck(_) | HostReply::StoreMismatch(..)
            ) && let Some(((c, a), forwarded)) = self
                .cutovers
                .iter_mut()
                .find(|((c, _), f)| *c == child && f.reply.is_none())
            {
                forwarded.reply = Some(reply.clone());
                self.out.push(Event::CutoverResult(c.clone(), *a, reply));
            } else {
                self.out.push(Event::Placed(child.clone(), reply));
            }
        }
    }

    /// A child has exited: relay its endpoint's last frames, read its report and answer,
    /// sample its listener, send its end, and only then let the listener's watch start.
    fn ended(&mut self, i: usize, status: Status) -> io::Result<()> {
        self.drain_endpoint(i);
        let mut process = self.children[i].process.take().expect("a live child");
        let report = process.report.as_mut().and_then(read_report);
        let child = self.children[i].state.child.clone();
        if let Some((mut out, bound)) = process.answer.take()
            && (status == Status::Code(0)
                || (process.install_target.is_some() && status == Status::Code(1)))
        {
            let mut answer = Vec::new();
            out.rewind()?;
            Read::by_ref(&mut out)
                .take(bound + 1)
                .read_to_end(&mut answer)?;
            if answer.len() as u64 <= bound {
                if let Some(target) = process.install_target {
                    let reply = match (HostReply::decode(&answer), status) {
                        (Ok(reply @ HostReply::Installed(version, false)), Status::Code(0))
                            if version == target =>
                        {
                            Some(reply)
                        }
                        (Ok(reply @ HostReply::InstallRefused(_)), Status::Code(1)) => Some(reply),
                        _ => None,
                    };
                    if let Some(reply) = reply {
                        self.children[i].install_reply = Some(reply.clone());
                        self.emit(Event::Placed(child.clone(), reply));
                    }
                } else {
                    self.emit(Event::Answered(child.clone(), answer));
                }
            }
        }
        let mut sample = false;
        if let Role::Serving(_) = child.role
            && let Some(l) = self.listener(&child.group)
        {
            sample = readable(self.listeners[l].socket.as_raw_fd());
            self.listeners[l].reported = sample;
        }
        let end = End {
            status,
            report,
            readable: sample,
        };
        // OLD ended with a forwarded cutover unanswered: the launcher cannot tell whether
        // it installed, so its saved reply is Uncertain.
        for ((c, a), forwarded) in &mut self.cutovers {
            if *c == child && forwarded.reply.is_none() {
                forwarded.reply = Some(HostReply::Uncertain);
                self.out
                    .push(Event::CutoverResult(c.clone(), *a, HostReply::Uncertain));
            }
        }
        self.children[i].state.phase = Phase::Exited(end.clone());
        self.emit(Event::Ended(child.clone(), end));
        self.retired(&child.group);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn turn(&mut self, timeout: Duration) -> io::Result<()> {
        self.wait_turn(Some(timeout))
    }

    /// One wait for the host session, child lifecycle, endpoints and cold listeners.
    fn wait_turn(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        for kept in &mut self.children {
            if let Some(conn) = kept.process.as_mut().and_then(|p| p.endpoint.as_mut()) {
                conn.watch()?;
            }
        }
        for i in 0..self.listeners.len() {
            let listener = &self.listeners[i];
            let cold =
                !listener.reported && !listener.retiring && !self.serving_live(&listener.group);
            self.listeners[i].watch.set(if cold { READ } else { 0 })?;
        }
        let backlogged = self.backlogged();
        if let Some(conn) = &mut self.session {
            conn.paused = backlogged;
            conn.watch()?;
        }
        if let Some(admitting) = &mut self.admitting {
            for worker in admitting.workers.values_mut() {
                if let Some(conn) = &mut worker.conn {
                    conn.watch()?;
                }
            }
        }
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
        let timeout = match (timeout, self.bootstrap.as_ref().and_then(|b| b.timeout())) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let count = self.epoll.wait(timeout, &mut events)?;
        let events = &events[..count];
        if let Some(bootstrap) = &mut self.bootstrap {
            bootstrap.turn(&self.epoll, events)?;
        }
        for i in 0..self.children.len() {
            if self.children[i]
                .process
                .as_ref()
                .and_then(|p| p.endpoint.as_ref())
                .is_some_and(|conn| conn.watch.fired(events))
            {
                self.drain_endpoint(i);
            }
        }
        for i in 0..self.children.len() {
            if let Some(process) = self.children[i].process.as_mut()
                && process
                    .watch
                    .as_ref()
                    .is_some_and(|watch| watch.fired(events))
                && let Some(status) = process.complete(&self.epoll)?
            {
                self.ended(i, status)?;
            }
        }
        for listener in &mut self.listeners {
            if listener.watch.fired(events)
                && !listener.reported
                && readable(listener.socket.as_raw_fd())
            {
                listener.reported = true;
                self.out.push(Event::Arrived(listener.group.clone()));
            }
        }
        if self
            .control_watch
            .as_ref()
            .is_some_and(|watch| watch.fired(events))
        {
            self.accept();
        }
        self.pump();
        self.notes();
        self.serve_session();
        Ok(())
    }

    /// A new connection replaces the session; it speaks once it has sent `Hello`.
    fn accept(&mut self) {
        let mut accepted = None;
        if let Some(control) = &self.control {
            while let Ok((stream, _)) = control.accept() {
                // With a bootstrap host, a connection from any other process is closed
                // before it can replace the session (launch.md "The operator socket").
                if let Some(bootstrap) = &self.bootstrap
                    && (bootstrap.pid().is_none()
                        || bootstrap::peer_pid(&stream) != bootstrap.pid())
                {
                    continue;
                }
                accepted = Conn::new(&self.epoll, stream).ok().or(accepted);
            }
        }
        if let Some(conn) = accepted {
            self.session = Some(conn);
            self.out.clear();
            self.unfence();
        }
    }

    fn serve_session(&mut self) {
        let backlogged = self.backlogged();
        if let Some(conn) = &mut self.session
            && !backlogged
        {
            conn.fill();
        }
        // One request at a time: admission input past the window holds the rest back.
        while !self.backlogged() {
            let Some(conn) = &mut self.session else { break };
            let Some(frame) = conn.frame() else { break };
            match Request::decode(&frame) {
                Ok(request) => self.handle(request),
                Err(_) => conn.closed = true,
            }
            self.pump();
        }
        if let Some(conn) = &mut self.session {
            for event in self.out.drain(..) {
                conn.queue(&event);
            }
            conn.flush();
            if conn.closed {
                // The session is lost: the children keep running, the fence ends without
                // removing anything, and the next host learns the rest from Hello.
                self.session = None;
                self.unfence();
            }
        }
        for kept in &mut self.children {
            if let Some(conn) = kept.process.as_mut().and_then(|p| p.endpoint.as_mut()) {
                conn.flush();
            }
        }
    }
}

impl<I> Drop for Launcher<I> {
    fn drop(&mut self) {
        for kept in &mut self.children {
            if let Some(mut process) = kept.process.take()
                && let Some(child) = process.child.take()
            {
                process.watch.take();
                if child.signal(libc::SIGKILL).is_ok() {
                    let _ = child.wait();
                }
            }
        }
    }
}

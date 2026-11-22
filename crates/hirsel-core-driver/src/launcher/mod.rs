//! The thin launcher: the one local process that holds the groups' listeners and starts
//! their children for the platform host (docs/spec/launch.md "The launcher", RUNNER-SPEC
//! §14.3-§14.5).
//!
//! Everything runs in one state loop ([`Launcher::turn`]). The host talks to it over one
//! control session at a time; each request is answered, and each child end, arrival and
//! endpoint notice is reported, as one ordered event stream. The launcher keeps no record
//! that a decision reads again: what it knows of its children is in memory and reaches a
//! new host through the inventory that answers `Hello`.
mod cgroup;
#[cfg(test)]
mod laws;
mod ready;
pub(crate) mod session;
mod spawn;
mod stores;

use hirsel_core_ids::launch::LAUNCH_IMAGE_ENV;
use ready::{CHANGE, Epoll, READ, WRITE, Watch};

use session::{
    Attempt, Bindings, Child, ChildState, End, Event, Group, HostCommand, HostReply, Label, Launch,
    Limits, Mode, Phase, Refusal, Request, Role, Status, Wire,
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
use std::time::Duration;

use stores::Stores;

/// Lock `state`, recover any containers, then serve the control session on `SOCKET`.
pub(crate) fn run(
    invocation: crate::cli::Invocation,
) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    use crate::cli::OptionId as Opt;
    let (mut control, mut state, mut cgroup_root) = (None, None, None);
    for (option, value) in invocation.options {
        let value = PathBuf::from(value.unwrap_or_default());
        match option {
            Opt::Control => control = Some(value),
            Opt::HostState => state = Some(value),
            Opt::CgroupRoot => cgroup_root = Some(value),
            _ => return Err("unexpected launcher option".into()),
        }
    }
    let state = crate::host::private_dir(state.ok_or("--state DIR is required")?)?;
    let control = control.ok_or("--control SOCKET is required")?;
    let recipe_bytes = crate::host::limits().recipe_bytes;
    let mut launcher = Launcher::new(&state, cgroup_root.as_deref(), || {
        Stores::open(&state, recipe_bytes)
    })?;
    launcher.listen(crate::host::bind_control(&control)?)?;
    loop {
        launcher.wait_turn(None)?;
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

/// The launcher's content-addressed objects: assembly records and their images.
pub(crate) trait Images {
    fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String>;
    /// Remove every object that no kept assembly record reaches; answer how many.
    fn collect(&self, keep: &BTreeSet<[u8; 32]>) -> io::Result<u64>;
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
        })
    }

    fn watch(&mut self) -> io::Result<()> {
        let interest = if self.closed {
            0
        } else if self.output.is_empty() {
            READ
        } else {
            READ | WRITE
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
    /// A serving child's wake report (descriptor 5), the launcher's read end.
    report: Option<File>,
    /// A describe or preview child's standard output, with its bound.
    answer: Option<(File, u64)>,
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
    control_watch: Option<Watch>,
    control: Option<UnixListener>,
    session: Option<Conn>,
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
            control_watch: None,
            control: None,
            session: None,
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

    /// Keep the objects that `attempt` synced until the host releases it. Admission calls
    /// this once steps 1-3 of "Durable publication" are synced.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "admission streams into the launcher with P3's platform host"
        )
    )]
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
            Request::Stop(child) => self.signal(&child, libc::SIGTERM),
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
        }
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
        let children = self.children.iter().map(|k| k.state.clone()).collect();
        let mut cold = Vec::new();
        for i in 0..self.listeners.len() {
            let group = self.listeners[i].group.clone();
            if !self.serving_live(&group) && readable(self.listeners[i].socket.as_raw_fd()) {
                self.listeners[i].reported = true;
                cold.push(group);
            }
        }
        self.emit(Event::Inventory(children, cold));
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
            return self.emit(Event::Launched(child, phase));
        }
        let refusal = match self.listener(&launch.group) {
            None => Some(Refusal::UnknownGroup),
            Some(i) if self.listeners[i].retiring => Some(Refusal::Retiring),
            Some(_) if launch.assembly.len() != 32 => Some(Refusal::Mismatch),
            Some(_) if matches!(launch.mode, Mode::Install(..)) => Some(Refusal::Unbuilt(
                "install child: the runtime has no install-only launch".into(),
            )),
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
        let mut input = None;
        let mut export_path = None;
        let scratch = self.scratch_of(&launch.group);
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
                command
                    .args(&image.prefix)
                    .arg(&n_place)
                    .arg(format!("--hirsel-activation={}", serve.activation))
                    .args(&launch.args);
                std::fs::create_dir_all(&scratch)?;
                let host_pid = if listener.label == Label::Contained {
                    1
                } else {
                    std::process::id()
                };
                command
                    .env("HIRSEL_CONTROL_FD", "4")
                    .env("HIRSEL_SCRATCH", &scratch)
                    .env("HIRSEL_HOST_PID", host_pid.to_string());
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
            Mode::Install(..) => return Err(io::ErrorKind::Unsupported.into()),
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
                .env_remove("LISTEN_PIDFDID")
                .env_remove("HIRSEL_RECORD")
                .env_remove("HIRSEL_RECORD_ID");
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
            report,
            answer,
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
        let endpoint = self
            .child(&child)
            .and_then(|i| self.children[i].process.as_mut())
            .and_then(|p| p.endpoint.as_mut());
        match endpoint {
            Some(conn) => {
                conn.queue(command);
                conn.flush();
            }
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
        let removed = if whole {
            self.images.collect(&keep).unwrap_or(0)
        } else {
            0
        };
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
            self.emit(Event::Placed(child.clone(), reply));
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
            && status == Status::Code(0)
        {
            let mut answer = Vec::new();
            out.rewind()?;
            Read::by_ref(&mut out)
                .take(bound + 1)
                .read_to_end(&mut answer)?;
            if answer.len() as u64 <= bound {
                self.emit(Event::Answered(child.clone(), answer));
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
        if let Some(conn) = &mut self.session {
            conn.watch()?;
        }
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
        let count = self.epoll.wait(timeout, &mut events)?;
        let events = &events[..count];
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
        self.serve_session();
        Ok(())
    }

    /// A new connection replaces the session; it speaks once it has sent `Hello`.
    fn accept(&mut self) {
        let mut accepted = None;
        if let Some(control) = &self.control {
            while let Ok((stream, _)) = control.accept() {
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
        let mut requests = Vec::new();
        if let Some(conn) = &mut self.session {
            conn.fill();
            while let Some(frame) = conn.frame() {
                match Request::decode(&frame) {
                    Ok(request) => requests.push(request),
                    Err(_) => conn.closed = true,
                }
            }
        }
        for request in requests {
            self.handle(request);
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

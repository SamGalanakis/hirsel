//! The thin launcher: the one local process that holds the groups' listeners and starts
//! their children for the platform host (docs/spec/launch.md "The launcher", RUNNER-SPEC
//! §14.3-§14.5).
//!
//! Everything runs in one state loop ([`Launcher::turn`]). The host talks to it over one
//! control session at a time; each request is answered, and each child end, arrival and
//! endpoint notice is reported, as one ordered event stream. The launcher keeps no record
//! that a decision reads again: what it knows of its children is in memory and reaches a
//! new host through the inventory that answers `Hello`.
#[cfg(test)]
mod laws;
pub(crate) mod session;
mod stores;

use session::{
    Attempt, Child, ChildState, End, Event, Group, HostCommand, HostReply, Label, Launch, Mode,
    Phase, Refusal, Request, Role, Status, Wire,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use stores::Stores;

/// `hirsel launcher --control SOCKET --state DIR`: lock `state`, serve the control session
/// on `SOCKET` and run the state loop until killed.
pub(crate) fn run(
    invocation: crate::cli::Invocation,
) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    use crate::cli::OptionId as Opt;
    let (mut control, mut state) = (None, None);
    for (option, value) in invocation.options {
        let value = PathBuf::from(value.unwrap_or_default());
        match option {
            Opt::Control => control = Some(value),
            Opt::HostState => state = Some(value),
            _ => return Err("unexpected launcher option".into()),
        }
    }
    let state = crate::host::private_dir(state.ok_or("--state DIR is required")?)?;
    let control = control.ok_or("--control SOCKET is required")?;
    let recipe_bytes = crate::host::limits().recipe_bytes;
    let mut launcher = Launcher::new(&state, || Stores::open(&state, recipe_bytes))?;
    launcher.listen(crate::host::bind_control(&control)?)?;
    loop {
        launcher.turn(Duration::from_secs(1))?;
    }
}

/// What the launcher runs for an assembly record: its image and the recipe's launch words.
pub(crate) struct Image {
    pub(crate) path: PathBuf,
    pub(crate) n_place: u64,
    /// The recipe's runtime prefix words (`--hirsel-exec=` approvals).
    pub(crate) prefix: Vec<String>,
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
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    closed: bool,
}

impl Conn {
    fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            closed: false,
        })
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
    group: Group,
    socket: TcpListener,
    /// The host has been told this listener is readable, by `Arrived`, an inventory or an
    /// end's sample; the watch stays off until a serving child has run again.
    reported: bool,
    retiring: bool,
}

struct Process {
    child: std::process::Child,
    pidfd: OwnedFd,
    /// A serving child's private endpoint (descriptor 4).
    endpoint: Option<Conn>,
    /// A serving child's wake report (descriptor 5), the launcher's read end.
    report: Option<File>,
    /// A describe or preview child's standard output, with its bound.
    answer: Option<(File, u64)>,
}

struct Kept {
    state: ChildState,
    process: Option<Process>,
}

pub(crate) struct Launcher<I> {
    images: I,
    scratch: PathBuf,
    listeners: Vec<Listening>,
    children: Vec<Kept>,
    out: Vec<Event>,
    fenced: bool,
    reservations: BTreeMap<Attempt, BTreeSet<[u8; 32]>>,
    deferred: Vec<Attempt>,
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
    // SAFETY: fcntl returns a fresh close-on-exec descriptor numbered 6 or more, so no dup2
    // into the launch slots 3 to 5 overwrites a source.
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 6) };
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

impl<I: Images> Launcher<I> {
    /// A launcher over `state`, which it locks before it opens its objects with `open`;
    /// `state/scratch` holds each group's private scratch and export directories.
    pub(crate) fn new(state: &Path, open: impl FnOnce() -> io::Result<I>) -> io::Result<Self> {
        let lock = lock(state)?;
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
            scratch,
            listeners: Vec::new(),
            children: Vec::new(),
            out: Vec::new(),
            fenced: false,
            reservations: BTreeMap::new(),
            deferred: Vec::new(),
            control: None,
            session: None,
            _lock: lock,
        })
    }

    /// Serve the control session on `listener`; the socket's permission is the host's.
    pub(crate) fn listen(&mut self, listener: UnixListener) -> io::Result<()> {
        listener.set_nonblocking(true)?;
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
            Request::Bind(group, address, label) => self.bind(group, &address, label),
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

    fn bind(&mut self, group: Group, address: &str, label: Label) {
        if label == Label::Contained {
            return self.emit(Event::BindFailed(
                group,
                "contained: this launcher runs no contained group".into(),
            ));
        }
        if let Some(held) = self.listeners.iter().find(|l| l.group.name == group.name) {
            let event = if held.group.incarnation == group.incarnation {
                Event::Bound(group)
            } else {
                let held = held.group.incarnation;
                Event::BindFailed(group, format!("the name is bound at incarnation {held}"))
            };
            return self.emit(event);
        }
        match TcpListener::bind(address) {
            Ok(socket) => {
                self.listeners.push(Listening {
                    group: group.clone(),
                    socket,
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
        match self.spawn(&launch, &image) {
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
    /// gets the listener (3), its private endpoint (4) and its wake report (5); every other
    /// mode gets descriptors 0-2 only.
    fn spawn(&self, launch: &Launch, image: &Image) -> io::Result<Process> {
        let workdir = Path::new(&launch.workdir);
        let n_place = format!("--hirsel-n-place={}", image.n_place);
        let mut command;
        let mut slots: Vec<(OwnedFd, RawFd)> = Vec::new();
        let mut endpoint = None;
        let mut report = None;
        let mut answer = None;
        match &launch.mode {
            Mode::Serving(_, serve) => {
                let listener = self
                    .listeners
                    .iter()
                    .find(|l| l.group == launch.group)
                    .ok_or(io::ErrorKind::NotFound)?;
                // The shell sets LISTEN_PID to its own PID and exec keeps it.
                command = Command::new("/bin/sh");
                command
                    .args([
                        "-c",
                        "LISTEN_PID=$$ LISTEN_FDS=1 exec \"$@\"",
                        "hirsel-place",
                    ])
                    .arg(&image.path)
                    .arg("--hirsel-wake");
                if serve.idle != 0 {
                    command.arg(format!("--hirsel-idle={}", serve.idle));
                }
                command
                    .args(&image.prefix)
                    .arg(&n_place)
                    .arg(format!("--hirsel-activation={}", serve.activation))
                    .args(&launch.args);
                let scratch = self.scratch_of(&launch.group);
                std::fs::create_dir_all(&scratch)?;
                command
                    .env("HIRSEL_CONTROL_FD", "4")
                    .env("HIRSEL_SCRATCH", scratch)
                    .env("HIRSEL_HOST_PID", std::process::id().to_string());
                let (ours, theirs) = UnixStream::pair()?;
                let (read, write) = pipe()?;
                slots.push((above_launch_slots(listener.socket.as_raw_fd())?, 3));
                slots.push((above_launch_slots(theirs.as_raw_fd())?, 4));
                slots.push((above_launch_slots(write.as_raw_fd())?, 5));
                endpoint = Some(Conn::new(ours)?);
                report = Some(read);
            }
            Mode::Describe(_) | Mode::Preview(_, _) => {
                command = Command::new(&image.path);
                let flag = if let Mode::Preview(..) = launch.mode {
                    "--hirsel-preview"
                } else {
                    "--hirsel-describe"
                };
                command.args(&image.prefix).arg(&n_place).arg(flag);
                let out = tempfile::tempfile()?;
                command.stdout(Stdio::from(out.try_clone()?));
                answer = Some((out, image.n_place));
                if let Mode::Preview(_, old) = &launch.mode {
                    let mut input = tempfile::tempfile()?;
                    input.write_all(old)?;
                    input.rewind()?;
                    command.stdin(Stdio::from(input));
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
                // The runtime's prepare launch replaces the image's Dir bindings with the
                // export directory and refuses a host control descriptor.
                command = Command::new(&image.path);
                command
                    .args(&image.prefix)
                    .arg(&n_place)
                    .arg("--hirsel-prepare")
                    .arg(self.scratch_of(&launch.group).join(export))
                    .arg(hex(&attempt.bytes()))
                    .arg(hex(&launch.assembly));
            }
            Mode::Install(..) => return Err(io::ErrorKind::Unsupported.into()),
        }
        if !matches!(launch.mode, Mode::Describe(_) | Mode::Preview(..)) {
            command.stdin(Stdio::null());
        }
        if !matches!(launch.mode, Mode::Serving(..)) {
            command
                .env_remove("HIRSEL_CONTROL_FD")
                .env_remove("HIRSEL_SCRATCH");
        }
        command
            .current_dir(workdir)
            .env_remove("LISTEN_FDS")
            .env_remove("LISTEN_PID")
            .env_remove("LISTEN_FDNAMES")
            .env_remove("LISTEN_PIDFDID")
            .env_remove("HIRSEL_RECORD")
            .env_remove("HIRSEL_RECORD_ID");
        let parent = std::process::id() as libc::pid_t;
        let raw: Vec<(RawFd, RawFd)> = slots.iter().map(|(f, to)| (f.as_raw_fd(), *to)).collect();
        // SAFETY: the closure makes only system calls. Each source is numbered above the
        // launch slots, so no dup2 overwrites another source.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                for &(from, to) in &raw {
                    if libc::dup2(from, to) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        // Our copies of the child's descriptors close here: the report's write end
        // especially, so the report reads end once the child does.
        drop(slots);
        let pidfd = pidfd(child.id())?;
        Ok(Process {
            child,
            pidfd,
            endpoint,
            report,
            answer,
        })
    }

    fn signal(&mut self, child: &Child, signal: i32) {
        if let Some(i) = self.child(child)
            && let Some(process) = &self.children[i].process
        {
            // SAFETY: a pidfd signal reaches only the process it pins.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    process.pidfd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
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
    fn ended(&mut self, i: usize) -> io::Result<()> {
        self.drain_endpoint(i);
        let mut process = self.children[i].process.take().expect("a live child");
        let status = process.child.wait()?;
        let status = match status.code() {
            Some(code) => Status::Code(code),
            None => Status::Signal(status.signal().unwrap_or(0)),
        };
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

    /// Run the loop once: wait up to `timeout` for a child end, an endpoint notice, a cold
    /// listener's arrival or a session frame, and act on each in that order.
    pub(crate) fn turn(&mut self, timeout: Duration) -> io::Result<()> {
        enum Source {
            Exit(usize),
            Endpoint(usize),
            Watch(usize),
            Control,
            Session,
        }
        let mut fds = Vec::new();
        let mut sources = Vec::new();
        let mut watch = |fd: RawFd, events: i16, source: Source| {
            fds.push(libc::pollfd {
                fd,
                events,
                revents: 0,
            });
            sources.push(source);
        };
        for (i, kept) in self.children.iter().enumerate() {
            if let Some(process) = &kept.process {
                watch(process.pidfd.as_raw_fd(), libc::POLLIN, Source::Exit(i));
                if let Some(conn) = &process.endpoint {
                    watch(conn.stream.as_raw_fd(), libc::POLLIN, Source::Endpoint(i));
                }
            }
        }
        for (i, l) in self.listeners.iter().enumerate() {
            // The watch is level-triggered and runs only while the group is cold.
            let cold = !self.children.iter().any(|k| {
                k.process.is_some()
                    && k.state.child.group == l.group
                    && matches!(k.state.child.role, Role::Serving(_))
            });
            if cold && !l.reported && !l.retiring {
                watch(l.socket.as_raw_fd(), libc::POLLIN, Source::Watch(i));
            }
        }
        if let Some(control) = &self.control {
            watch(control.as_raw_fd(), libc::POLLIN, Source::Control);
        }
        if let Some(conn) = &self.session {
            let events = if conn.output.is_empty() {
                libc::POLLIN
            } else {
                libc::POLLIN | libc::POLLOUT
            };
            watch(conn.stream.as_raw_fd(), events, Source::Session);
        }
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: poll borrows the initialized records for its duration.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(())
            } else {
                Err(error)
            };
        }
        let fired: Vec<_> = fds
            .iter()
            .zip(sources)
            .filter(|(fd, _)| fd.revents != 0)
            .map(|(_, source)| source)
            .collect();
        for source in &fired {
            if let Source::Endpoint(i) = *source {
                self.drain_endpoint(i);
            }
        }
        for source in &fired {
            if let Source::Exit(i) = *source {
                self.ended(i)?;
            }
        }
        for source in &fired {
            if let Source::Watch(i) = *source
                && !self.listeners[i].reported
                && readable(self.listeners[i].socket.as_raw_fd())
            {
                self.listeners[i].reported = true;
                let group = self.listeners[i].group.clone();
                self.emit(Event::Arrived(group));
            }
        }
        if fired.iter().any(|s| matches!(s, Source::Control)) {
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
                accepted = Conn::new(stream).ok().or(accepted);
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
        // An uncontained child gets EXEC-6's best-effort death with its launcher.
        for kept in &mut self.children {
            if let Some(process) = &mut kept.process {
                let _ = process.child.kill();
                let _ = process.child.wait();
            }
        }
    }
}

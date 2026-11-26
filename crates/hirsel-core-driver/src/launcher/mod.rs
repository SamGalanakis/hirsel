//! The thin launcher: the one local process that holds the groups' listeners and starts
//! their children for the platform host (docs/spec/launch.md "The launcher", RUNNER-SPEC
//! §14.3-§14.5).
//!
//! Everything runs in one state loop ([`Launcher::turn`]). The host talks to it over one
//! control session at a time; each request is answered, and each child end, arrival and
//! endpoint notice is reported, as one ordered event stream. The launcher keeps no record
//! that a decision reads again: what it knows of its children is in memory and reaches a
//! new host through the inventory that answers `Hello`.
pub(crate) mod admit;
mod bootstrap;
mod cgroup;
mod cli;
pub(crate) mod dev;
mod grants;
#[cfg(test)]
mod laws;
pub(crate) mod objects;
mod places;
mod ready;
pub(crate) mod session;
#[cfg(test)]
mod source_laws;
pub(crate) mod spawn;
mod stores;

pub(crate) use cgroup::Lifecycle;
#[cfg(test)]
use cli::bind_control;
pub(crate) use cli::{parse_lifecycle, resolve_lifecycle, run};

use hirsel_core_ids::{hex, launch::LAUNCH_IMAGE_ENV};
use ready::{CHANGE, NONE, READ, Readiness, ReadyEvent, WRITE, Watch};

use session::{
    Approvals, Attempt, Child, ChildState, End, Event, Failure, Group, HostCommand, HostReply,
    Input, InstallKind, Launch, Mode, Phase, Refusal, Request, Role, Status, Wire,
};
use std::collections::{BTreeMap, BTreeSet};
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

#[cfg(test)]
use admit::limits;
pub(crate) use admit::{Admission, Compiler, deploy_foreign_lines, read_recipe};

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

/// Concurrent admissions; a further `Admit` fails until one ends.
const ADMISSIONS: usize = 4;

/// The staging bound of the launcher's code store.
pub(super) const STAGING_BYTES: u64 = 512 << 20;

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
    fn new(poll: &Readiness, stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            watch: poll.watch(stream.as_raw_fd()),
            stream,
            input: Vec::new(),
            output: Vec::new(),
            closed: false,
            paused: false,
        })
    }

    fn watch(&mut self) -> io::Result<()> {
        let read = if self.paused { NONE } else { READ };
        let interest = if self.closed {
            NONE
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
        self.closed |= session::write_frame(&mut self.output, value).is_err();
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
    /// The host has been told this listener is readable, by `Arrived`, an inventory or an
    /// end's sample; the watch stays off until a serving child has run again.
    reported: bool,
    retiring: bool,
}

struct Process {
    watch: Option<Watch>,
    child: Option<RunningChild>,
    /// A serving child's private endpoint (descriptor 4).
    endpoint: Option<Conn>,
    /// A serving open awaiting the host's durable acceptance. Replay it after Hello.
    opened: Option<HostReply>,
    /// A serving child's wake report (descriptor 5), the launcher's read end.
    report: Option<File>,
    /// Standard output of a child with a bounded answer.
    answer: Option<(File, u64)>,
    install_target: Option<u64>,
    /// A prepare child's `TRIGGERS` file in its export directory, with its bound: the
    /// child's answer once it has exited 0 (launch.md "Preparing the pending trigger
    /// report").
    triggers: Option<(PathBuf, u64)>,
}

struct RunningChild {
    process: spawn::Spawned,
    group: Option<cgroup::Group>,
    status: Option<Status>,
}

impl RunningChild {
    fn signal(&self, signal: i32) -> io::Result<()> {
        match (&self.group, signal, &self.status) {
            (Some(group), libc::SIGKILL, _) => group.kill(),
            (_, _, Some(_)) => Ok(()),
            _ => self.process.signal(signal),
        }
    }

    fn wait(mut self) -> io::Result<Status> {
        self.reap_leader()?;
        if let Some(group) = &self.group {
            group.wait_empty()?;
        }
        self.finish()
    }

    fn reap_leader(&mut self) -> io::Result<()> {
        if self.status.is_none() {
            self.status = Some(status_of(wait_pid(self.process.pid())?));
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
    fn complete(&mut self, poll: &Readiness) -> io::Result<Option<Status>> {
        self.watch.take();
        let child = self.child.as_mut().expect("a live child");
        child.reap_leader()?;
        if let Some(group) = &child.group
            && !group.empty()?
        {
            let mut watch = poll.watch(group.event_fd());
            watch.set(CHANGE)?;
            self.watch = Some(watch);
            return Ok(None);
        }
        self.child.take().expect("a live child").finish().map(Some)
    }
}

fn status_of(status: ExitStatus) -> Status {
    match status.code() {
        Some(code) => Status::Code(code),
        None => Status::Signal(status.signal().unwrap_or(0)),
    }
}

pub(crate) fn wait_pid(pid: libc::pid_t) -> io::Result<ExitStatus> {
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
    cgroup_root: Option<PathBuf>,
    poll: Readiness,
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
    pub(super) bootstrap: Option<bootstrap::Bootstrap>,
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

pub(crate) fn above_launch_slots(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor above every launch slot.
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, spawn::IMAGE_FD + 1) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is fresh and owned.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn pipe() -> io::Result<(File, OwnedFd)> {
    let (read, write) = crate::host::pipe()?;
    // The writer is the child's; only the reader is nonblocking.
    // SAFETY: fcntl on an owned descriptor.
    if unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((File::from(read), write))
}

impl<I: Images> Launcher<I> {
    /// A launcher over `state`, which it locks before it opens its objects with `open`;
    /// `state/scratch` holds each group's private scratch and export directories.
    pub(crate) fn new(
        state: &Path,
        lifecycle: Lifecycle,
        open: impl FnOnce() -> io::Result<I>,
    ) -> io::Result<Self> {
        Self::new_with(state, lifecycle, open, cgroup::delegated)
    }

    pub(super) fn new_with(
        state: &Path,
        lifecycle: Lifecycle,
        open: impl FnOnce() -> io::Result<I>,
        delegate: impl FnOnce(&Path) -> io::Result<PathBuf>,
    ) -> io::Result<Self> {
        let lock = lock(state)?;
        let cgroup_root = match lifecycle {
            Lifecycle::Leader => None,
            Lifecycle::Managed if !crate::host::CGROUPS => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "--lifecycle managed requires a delegated cgroup v2 subtree",
                ));
            }
            Lifecycle::Managed => Some(delegate(state)?),
        };
        if let Some(root) = &cgroup_root {
            cgroup::recover(root)?;
            crate::host::own_orphans()?;
        }
        let poll = Readiness::new()?;
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
            cgroup_root,
            poll,
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
        let mut watch = self.poll.watch(listener.as_raw_fd());
        watch.set(READ)?;
        self.control_watch = Some(watch);
        self.control = Some(listener);
        Ok(())
    }

    /// Admit releases with `admission`'s stores; without it every `Admit` fails.
    pub(crate) fn admit_with(&mut self, admission: Admission) -> io::Result<()> {
        let (wake, write) = pipe()?;
        let mut watch = self.poll.watch(wake.as_raw_fd());
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
            hex::encode(group.name.as_bytes()),
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
            Request::Bind(group, address) => self.bind(group, &address),
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
            Request::Admit(group, attempt, input, approvals, deadline_ms) => {
                self.admit(group, attempt, input, approvals, deadline_ms)
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

    fn admit(
        &mut self,
        group: Group,
        attempt: Attempt,
        input: Input,
        approvals: Approvals,
        deadline_ms: u64,
    ) {
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
        let deadline = admit::deadline_from_wall_ms(deadline_ms);
        let Some(deadline) = deadline else {
            return self.admission_failed(attempt, "the admission deadline passed before Admit");
        };
        let root = |r: &[u8]| {
            <[u8; 32]>::try_from(r)
                .ok()
                .map(hirsel_core_ids::ContentHash)
        };
        let source = match input {
            Input::Attested(r) => root(&r).map(admit::Source::Attested),
            Input::Publication(r) => root(&r).map(admit::Source::Publication),
            Input::Source(bytes, gated) => match admit::source(&bytes, gated) {
                Ok(source) => Some(source),
                Err(why) => return self.emit(Event::AdmissionFailed(attempt, why)),
            },
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
            &self.poll,
            id,
            attempt,
            source,
            approvals,
            deadline,
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

    fn bind(&mut self, group: Group, address: &str) {
        if let Some(held) = self.listeners.iter().find(|l| l.group.name == group.name) {
            let event = if held.group.incarnation != group.incarnation {
                let held = held.group.incarnation;
                Event::BindFailed(group, format!("the name is bound at incarnation {held}"))
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
                    watch: self.poll.watch(socket.as_raw_fd()),
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
                        selection: match &launch.mode {
                            Mode::Serving(_, serve) => Some(session::Selection::new(
                                serve.activation,
                                launch.assembly.clone(),
                            )),
                            _ => None,
                        },
                        answer: None,
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
        let mut triggers = None;
        let scratch = self.scratch_of(&launch.group);
        let host_pid = std::process::id();
        match &launch.mode {
            Mode::Serving(_, serve) => {
                command.arg("--hirsel-wake");
                if serve.idle != 0 {
                    command.arg(format!("--hirsel-idle={}", serve.idle));
                }
                if let Some((hi, lo)) = serve.store_id {
                    command.arg(format!("--hirsel-store-id={hi:016x}{lo:016x}"));
                }
                if let Some(version) = serve.witness {
                    command.arg(format!("--hirsel-witness={version}"));
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
                        .env("HIRSEL_RECORD_ID", hex::encode(&recording.id));
                }
                let (ours, theirs) = UnixStream::pair()?;
                let (read, write) = pipe()?;
                slots.push((above_launch_slots(listener.socket.as_raw_fd())?, 3));
                slots.push((above_launch_slots(theirs.as_raw_fd())?, 4));
                slots.push((above_launch_slots(write.as_raw_fd())?, 5));
                endpoint = Some(Conn::new(&self.poll, ours)?);
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
            Mode::Prepare(attempt, export, fates) => {
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
                    .args([
                        hex::encode(&attempt.bytes()),
                        hex::encode(&image.digest),
                        hex::encode(&fates),
                    ]);
                triggers = Some((path.join("TRIGGERS"), image.n_place));
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
                let digest = hex::encode(&image.digest);
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
            .map(|root| cgroup::Group::create(root, &launch.child()))
            .transpose()?;
        let started = (|| {
            let directory = group.as_ref().map(cgroup::Group::directory).transpose()?;
            spawn::start(&command, workdir, &slots, directory.as_ref(), serving)
        })();
        let process = match started {
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
        let child = RunningChild {
            process,
            group,
            status: None,
        };
        let watch = match self.poll.watch_exit(&child.process) {
            Ok(watch) => watch,
            Err(error) => {
                if child.signal(libc::SIGKILL).is_ok() {
                    let _ = child.wait();
                }
                return Err(error);
            }
        };
        Ok(Process {
            watch: Some(watch),
            child: Some(child),
            endpoint,
            opened: None,
            report,
            answer,
            install_target,
            triggers,
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

    /// One wait for the host session, child lifecycle, endpoints and cold listeners.
    pub(super) fn wait_turn(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        for kept in &mut self.children {
            if let Some(conn) = kept.process.as_mut().and_then(|p| p.endpoint.as_mut()) {
                conn.watch()?;
            }
        }
        for i in 0..self.listeners.len() {
            let listener = &self.listeners[i];
            let cold =
                !listener.reported && !listener.retiring && !self.serving_live(&listener.group);
            Watch::set(&mut self.listeners[i].watch, if cold { READ } else { NONE })?;
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
        let mut events = [ReadyEvent::EMPTY; 64];
        let timeout = match (timeout, self.bootstrap.as_ref().and_then(|b| b.timeout())) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let count = self.poll.wait(timeout, &mut events)?;
        let events = &events[..count];
        if let Some(bootstrap) = &mut self.bootstrap {
            bootstrap.turn(&self.poll, events)?;
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
                && let Some(status) = process.complete(&self.poll)?
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
                accepted = Conn::new(&self.poll, stream).ok().or(accepted);
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

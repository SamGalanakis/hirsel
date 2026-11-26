//! The thin launcher: the one local process that holds the groups' listeners and starts
//! their children for the platform host (docs/spec/launch.md "The launcher", RUNNER-SPEC
//! §14.3-§14.5).
//!
//! Everything runs in one state loop ([`Launcher::wait_turn`]). The host talks to it over one
//! control session at a time; each request is answered, and each child end, arrival and
//! endpoint notice is reported, as one ordered event stream. The launcher keeps no record
//! that a decision reads again: what it knows of its children is in memory and reaches a
//! new host through the inventory that answers `Hello`.
pub(crate) mod admit;
mod bootstrap;
mod cgroup;
mod cli;
mod control_session;
pub(crate) mod dev;
mod embedded_host;
mod grants;
#[cfg(test)]
mod laws;
mod lifecycle;
pub(crate) mod objects;
mod places;
mod ready;
pub(crate) mod session;
#[cfg(test)]
mod source_laws;
pub(crate) mod spawn;
mod stores;
mod supervision;
#[cfg(test)]
mod supervision_laws;

pub(crate) use cgroup::Lifecycle;
#[cfg(test)]
use cli::bind_control;
pub(crate) use cli::{parse_lifecycle, resolve_lifecycle, run};

use control_session::{Conn, ControlSession, PeerPolicy};
#[cfg(test)]
use hirsel_core_ids::hex;
use lifecycle::ObjectLifecycle;
use ready::{READ, Readiness, ReadyEvent, Watch};
use session::{
    Approvals, Attempt, Child, Event, Failure, Group, HostCommand, HostReply, Input, Launch,
    Refusal, Request, Role, Status, Wire,
};
use spawn::RunningChild;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{Arc, mpsc};
use std::time::Duration;
use supervision::{ChildNotice, ChildSupervisor, GroupAdmitState, StartCheck};

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
    poll: Readiness,
    supervisor: ChildSupervisor,
    pub(super) objects: ObjectLifecycle<I>,
    session: ControlSession,
    admitting: Option<Admitting>,
    cutovers: BTreeMap<(Child, Attempt), Forwarded>,
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
            poll,
            supervisor: ChildSupervisor::new(cgroup_root, scratch),
            objects: ObjectLifecycle::new(images),
            session: ControlSession::default(),
            admitting: None,
            cutovers: BTreeMap::new(),
            bootstrap: None,
            _lock: lock,
        })
    }

    /// Serve the control session on `listener`; the socket's permission is the host's.
    pub(crate) fn listen(&mut self, listener: UnixListener) -> io::Result<()> {
        self.session.listen(&self.poll, listener)
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
        self.objects.reserve(attempt, objects);
    }

    /// The events not yet sent, for a caller that drives the launcher without a session.
    #[cfg(test)]
    pub(crate) fn take_events(&mut self) -> Vec<Event> {
        self.session.take_events()
    }

    /// Handle `request` and wait for any pending start reply (used by `laws.rs` `Rig::ask`).
    #[cfg(test)]
    pub(super) fn ask_request(&mut self, request: Request) -> Vec<Event> {
        let starting = match &request {
            Request::Start(launch) => Some(launch.child()),
            _ => None,
        };
        self.handle(request);
        match starting {
            Some(child) if self.has_pending_starts() => vec![self.wait_start_reply(&child)],
            _ => self.take_events(),
        }
    }

    /// Drive the event loop until `child`'s `Launched` or `Refused` reply arrives, leaving
    /// any other events queued.
    #[cfg(test)]
    pub(super) fn wait_start_reply(&mut self, child: &Child) -> Event {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(ev) = self.session.take_start_reply(child) {
                return ev;
            }
            assert!(
                std::time::Instant::now() < end,
                "timed out waiting for start {child:?}"
            );
            self.turn(Duration::from_millis(20)).unwrap();
        }
    }

    /// The group's scratch directory, which a serving child gets as `HIRSEL_SCRATCH` and in
    /// which a prepare child's export directory lies.
    pub(crate) fn scratch_of(&self, group: &Group) -> PathBuf {
        self.supervisor.scratch_of(group)
    }

    #[cfg(test)]
    pub(super) fn has_pending_starts(&self) -> bool {
        self.supervisor.has_pending_starts()
    }

    #[cfg(test)]
    pub(super) fn has_cgroup_root(&self) -> bool {
        self.supervisor.has_cgroup_root()
    }

    #[cfg(test)]
    pub(super) fn listener_addr(&self, group: &Group) -> Option<std::net::SocketAddr> {
        self.supervisor.listener_addr(group)
    }

    #[cfg(test)]
    pub(super) fn is_child_live(&self, child: &Child) -> bool {
        self.supervisor.is_child_live(child)
    }

    #[cfg(test)]
    pub(super) fn has_session(&self) -> bool {
        self.session.has_session()
    }

    #[cfg(test)]
    pub(super) fn control_fd(&self) -> Option<RawFd> {
        self.session.control_fd()
    }

    fn emit(&mut self, event: Event) {
        self.session.emit(event);
    }

    fn apply_notices(&mut self, notices: Vec<ChildNotice>) {
        for notice in notices {
            match notice {
                ChildNotice::Event(event) => self.session.emit(event),
                ChildNotice::ClaimReleased(child) => self.objects.release_claim(&child),
                ChildNotice::Reply(child, reply) => {
                    // OLD's next answer after a forwarded cutover is that cutover's; a held-write
                    // acknowledgement is a notice.
                    if !matches!(
                        reply,
                        HostReply::Opened(_)
                            | HostReply::WriteAck(_)
                            | HostReply::StoreMismatch(..)
                    ) && let Some(((c, a), forwarded)) = self
                        .cutovers
                        .iter_mut()
                        .find(|((c, _), f)| *c == child && f.reply.is_none())
                    {
                        forwarded.reply = Some(reply.clone());
                        self.session
                            .emit(Event::CutoverResult(c.clone(), *a, reply));
                    } else {
                        if matches!(
                            reply,
                            HostReply::Current(..)
                                | HostReply::Refused(_)
                                | HostReply::StoreMismatch(..)
                        ) {
                            self.supervisor.record_answer(&child, &reply);
                        }
                        self.session.emit(Event::Placed(child, reply));
                    }
                }
                ChildNotice::Exited(child) => {
                    // OLD ended with a forwarded cutover unanswered: the launcher cannot tell
                    // whether it installed, so its saved reply is Uncertain.
                    for ((c, a), forwarded) in &mut self.cutovers {
                        if *c == child && forwarded.reply.is_none() {
                            forwarded.reply = Some(HostReply::Uncertain);
                            self.session.emit(Event::CutoverResult(
                                c.clone(),
                                *a,
                                HostReply::Uncertain,
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Answer one request. Its events follow every event already queued.
    pub(crate) fn handle(&mut self, request: Request) {
        match request {
            Request::Hello => self.hello(),
            Request::Bind(group, address) => {
                let event = self.supervisor.bind(&self.poll, group, &address);
                self.emit(event);
            }
            Request::Close(group) => {
                let event = self.supervisor.close(group);
                self.emit(event);
            }
            Request::Start(launch) => self.start(launch),
            Request::Stop(child) => {
                let sig = if matches!(child.role, Role::Install(_)) {
                    libc::SIGKILL
                } else {
                    libc::SIGTERM
                };
                self.supervisor.signal(&child, sig);
            }
            Request::Kill(child) => self.supervisor.signal(&child, libc::SIGKILL),
            Request::Acknowledge(child) => self.supervisor.acknowledge(&child),
            Request::Command(child, command) => self.command(child, &command),
            Request::Retire(group) => {
                let mut notices = Vec::new();
                self.supervisor.retire(&group, &mut notices);
                self.apply_notices(notices);
            }
            Request::Release(attempt) => {
                if let Some(admitting) = &mut self.admitting {
                    admitting.workers.remove(&attempt);
                }
                self.objects.release(attempt);
            }
            Request::CollectBegin => {
                self.objects.fence();
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
                self.objects.unfence();
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
        let refusal = match (&self.admitting, self.supervisor.group_admit_state(&group)) {
            (None, _) => Some("the launcher has no code store"),
            (_, GroupAdmitState::Unknown) => Some("unknown group"),
            (_, GroupAdmitState::Retiring) => Some("the group is retiring"),
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
        self.session.extend(events);
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
        self.session.extend(events);
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

    fn hello(&mut self) {
        // Hello ends the old session: its unsent events are dropped, and its fence ends
        // without removing anything.
        self.session.clear_out();
        // A new session ends every reservation and admission of the old one (§14.5).
        self.objects.new_session();
        if let Some(admitting) = &mut self.admitting {
            admitting.workers.clear();
        }
        let (children, cold, replayed) = self.supervisor.inventory_for_hello();
        let cutovers = self.cutovers.keys().cloned().collect();
        self.emit(Event::Inventory(children, cold, cutovers));
        for (child, reply) in replayed {
            self.session.emit(Event::Placed(child, reply));
        }
    }

    fn start(&mut self, launch: Launch) {
        let child = launch.child();
        match self.supervisor.check_start(&launch) {
            StartCheck::Existing(phase, install_reply) => {
                self.emit(Event::Launched(child.clone(), phase));
                if let Some(reply) = install_reply {
                    self.emit(Event::Placed(child, reply));
                }
                return;
            }
            StartCheck::Pending => return,
            StartCheck::Refused(why) => return self.emit(Event::Refused(child, why)),
            StartCheck::Ready => {}
        }
        let image = match self.objects.claim_image(&launch) {
            None => return self.emit(Event::Refused(child, Refusal::Held)),
            Some(Err(why)) => return self.emit(Event::Refused(child, Refusal::Image(why))),
            Some(Ok(image)) => image,
        };
        let mut notices = Vec::new();
        self.supervisor
            .start_reserved(&self.poll, launch, image, &mut notices);
        self.apply_notices(notices);
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
        let accepting = *command == HostCommand::StoreAccepted;
        if self.supervisor.send_command(&child, command, accepting) {
            if let Some(key) = key {
                self.cutovers.insert(
                    key,
                    Forwarded {
                        command: command.clone(),
                        reply: None,
                    },
                );
            }
        } else if !accepting {
            self.emit(Event::Placed(
                child,
                HostReply::Refused("the child has no live endpoint".into()),
            ));
        }
    }

    fn collect(&mut self, roots: &[Vec<u8>]) {
        if !self.objects.is_fenced() {
            return self.emit(Event::Collected(0));
        }
        // No worker writes a record while the roots are read and removed, and every
        // record already written has its note applied, so its reservation is held.
        let admission = self.admitting.as_ref().map(|a| a.admission.clone());
        let _publishing = admission
            .as_ref()
            .map(|a| a.publish.lock().unwrap_or_else(|e| e.into_inner()));
        self.notes();
        let removed = self.objects.collect(roots, admission.as_deref());
        self.emit(Event::Collected(removed));
    }

    /// One wait for the host session, child lifecycle, endpoints and cold listeners.
    pub(super) fn wait_turn(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        let backlogged = self.backlogged();
        self.session.prepare_watch(backlogged)?;
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
        let mut notices = Vec::new();
        self.supervisor.step(&self.poll, events, &mut notices)?;
        self.apply_notices(notices);
        let policy = self
            .bootstrap
            .as_ref()
            .map_or(PeerPolicy::Any, |b| PeerPolicy::Only(b.pid()));
        if self.session.accept_if_ready(&self.poll, events, policy) {
            self.objects.unfence();
        }
        self.pump();
        self.notes();
        self.serve_session();
        Ok(())
    }

    fn serve_session(&mut self) {
        let backlogged = self.backlogged();
        self.session.fill(backlogged);
        // One request at a time: admission input past the window holds the rest back.
        while !self.backlogged() {
            let Some(request) = self.session.next_request() else {
                break;
            };
            self.handle(request);
            self.pump();
        }
        if self.session.flush() {
            // The session is lost: the children keep running, the fence ends without
            // removing anything, and the next host learns the rest from Hello.
            self.objects.unfence();
        }
    }
}

//! Exec child creation and custody. Only the supervisor creates children.
//! Wait, drop and orphan adoption use the same registry lock and pinned pidfds.
//! macOS has no pidfds: each child is pinned by a private kqueue holding its
//! NOTE_EXIT, which stays readable once the child exits, and is reaped and
//! signalled by PID, which stays the child's until this registry reaps it.
//! macOS has no subreaper either, so a child's own children are not adopted.

use std::{
    ffi::{CStr, CString},
    fs, io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

/// The launcher's resolved inputs, constructed after issue. The image is the
/// read-only description of the sealed and checked copy.
pub struct SpawnInput {
    pub image: Arc<OwnedFd>,
    pub interpreter: Option<Arc<CStr>>,
    pub cwd: OwnedFd,
    pub argv: Vec<CString>,
    pub envp: Vec<CString>,
    pub piped: [bool; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Termination {
    Exited(u8),
    Signalled(u8),
}

#[derive(Clone, Copy)]
pub enum Signal {
    Int,
    Term,
    Kill,
}

impl Signal {
    fn number(self) -> i32 {
        match self {
            Self::Int => libc::SIGINT,
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub(crate) enum SetupStep {
    Stdio = 1,
    Cwd,
    Close,
    Signals,
    Exec,
}

#[derive(Debug)]
pub struct SpawnError {
    pub error: io::Error,
    pub(crate) step: Option<SetupStep>,
}

impl From<io::Error> for SpawnError {
    fn from(error: io::Error) -> Self {
        Self { error, step: None }
    }
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(step) = self.step {
            write!(f, "{step:?}: ")?;
        }
        self.error.fmt(f)
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

pub struct Spawned {
    pub process: Process,
    pub stdin: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
}

struct Child {
    pid: libc::pid_t,
    fd: OwnedFd,
    disposition: Disposition,
    waiter: Option<(crate::tasks::Outside, u64)>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Disposition {
    Live,
    Reaped,
    Released,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Orphan,
}

#[derive(Default)]
struct Registry {
    children: Vec<Child>,
    spawn: Option<(SpawnInput, Option<SetupStep>)>,
    result: Option<Result<Started, SpawnError>>,
    stopping: bool,
    #[cfg(test)]
    orphans: Vec<(libc::pid_t, bool)>,
}

struct Shared {
    registry: Mutex<Registry>,
    completed: Condvar,
    change: OwnedFd,
    poller: OwnedFd,
}

/// Owns the thread that checks launch credentials, creates direct children,
/// and reaps released children. Drop completes orderly process cleanup.
pub struct Supervisor {
    shared: Arc<Shared>,
    spawn_slot: Mutex<()>,
    thread: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// The launcher supplies its startup tests. They run on the child-creating
    /// thread before it makes the run non-dumpable and enables subreaping.
    pub fn start(probe: fn() -> io::Result<()>) -> io::Result<Self> {
        let change = owned(unsafe {
            hirsel_core_net::sys::eventfd(
                0,
                hirsel_core_net::sys::EFD_CLOEXEC | hirsel_core_net::sys::EFD_NONBLOCK,
            )
        })?;
        let poller = owned(unsafe {
            hirsel_core_net::sys::epoll_create1(hirsel_core_net::sys::EPOLL_CLOEXEC)
        })?;
        watch(&poller, &change, 0, false)?;
        let shared = Arc::new(Shared {
            registry: Mutex::new(Registry::default()),
            completed: Condvar::new(),
            change,
            poller,
        });
        let state = shared.clone();
        let (ready, initialized) = std::sync::mpsc::sync_channel(0);
        let thread = thread::Builder::new()
            .name("hirsel-exec".into())
            .spawn(move || {
                let started = start_thread(probe);
                let running = started.is_ok();
                if ready.send(started).is_ok() && running {
                    state.run();
                }
            })?;
        let outcome = initialized.recv().expect("Exec supervisor startup");
        if let Err(error) = outcome {
            thread.join().expect("Exec supervisor startup");
            return Err(error);
        }
        Ok(Self {
            shared,
            spawn_slot: Mutex::new(()),
            thread: Some(thread),
        })
    }

    pub fn spawn(&self, input: SpawnInput) -> Result<Spawned, SpawnError> {
        self.spawn_with(input, None)
    }

    pub(crate) fn spawn_with(
        &self,
        input: SpawnInput,
        failure: Option<SetupStep>,
    ) -> Result<Spawned, SpawnError> {
        // The spawner thread starts the child; this unit waits off the slot.
        crate::units::off_slot(|| self.spawn_now(input, failure))
    }

    fn spawn_now(
        &self,
        input: SpawnInput,
        failure: Option<SetupStep>,
    ) -> Result<Spawned, SpawnError> {
        let _slot = self.spawn_slot.lock().unwrap();
        let mut registry = self.shared.registry.lock().unwrap();
        registry.spawn = Some((input, failure));
        self.shared.wake();
        let mut registry = self
            .shared
            .completed
            .wait_while(registry, |r| r.result.is_none())
            .unwrap();
        let Started {
            pidfd: fd,
            pipes: [stdin, stdout, stderr],
        } = registry.result.take().unwrap()?;
        Ok(Spawned {
            process: Process {
                shared: self.shared.clone(),
                fd,
                termination: None,
            },
            stdin,
            stdout,
            stderr,
        })
    }

    #[cfg(test)]
    pub(crate) fn child_pids(&self) -> Vec<libc::pid_t> {
        self.shared
            .registry
            .lock()
            .unwrap()
            .children
            .iter()
            .map(|child| child.pid)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn orphan_checks(&self) -> Vec<(libc::pid_t, bool)> {
        self.shared.registry.lock().unwrap().orphans.clone()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shared.registry.lock().unwrap().stopping = true;
        self.shared.wake();
        self.thread.take().unwrap().join().expect("Exec supervisor");
    }
}

/// A single direct child. Only this owner may reap it before release.
pub struct Process {
    shared: Arc<Shared>,
    fd: RawFd,
    termination: Option<Termination>,
}

impl Process {
    pub fn cached(&self) -> Option<Termination> {
        self.termination
    }

    /// The descriptor remains open until this process is dropped or its
    /// supervisor shuts down. A reactor may observe it while waiting.
    pub(crate) fn pidfd(&self) -> RawFd {
        self.fd
    }

    pub fn try_wait(&mut self) -> io::Result<Option<Termination>> {
        self.wait_or_park(None)
    }

    pub(crate) fn wait_or_park(&mut self, token: Option<u64>) -> io::Result<Option<Termination>> {
        if self.termination.is_some() {
            return Ok(self.termination);
        }
        let mut registry = self.shared.registry.lock().unwrap();
        let child = direct(&mut registry, self.pidfd())?;
        if let Some(status) = wait(child.pid, child.fd.as_raw_fd(), libc::WNOHANG)? {
            child.waiter = None;
            child.disposition = Disposition::Reaped;
            self.termination = Some(status);
            self.shared.wake();
        } else if let Some(token) = token {
            child.waiter = Some((crate::tasks::Outside::begin(), token));
        }
        Ok(self.termination)
    }

    pub(crate) fn withdraw_wait(&self) {
        let mut registry = self.shared.registry.lock().unwrap();
        if let Ok(child) = direct(&mut registry, self.pidfd()) {
            child.waiter = None;
        }
    }

    pub fn signal(&mut self, signal: Signal) -> io::Result<()> {
        if self.termination.is_some() {
            return Ok(());
        }
        let mut registry = self.shared.registry.lock().unwrap();
        let child = direct(&mut registry, self.pidfd())?;
        send(child.pid, child.fd.as_raw_fd(), signal)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut registry = self.shared.registry.lock().unwrap();
        if let Ok(child) = direct(&mut registry, self.fd) {
            if child.disposition == Disposition::Live {
                release(child);
            } else {
                registry
                    .children
                    .retain(|child| child.fd.as_raw_fd() != self.fd);
            }
            self.shared.wake();
        }
    }
}

fn direct(registry: &mut Registry, fd: RawFd) -> io::Result<&mut Child> {
    if registry.stopping {
        return Err(io::Error::from_raw_os_error(libc::ECHILD));
    }
    registry
        .children
        .iter_mut()
        .find(|child| child.fd.as_raw_fd() == fd)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ECHILD))
}

#[cfg(target_os = "linux")]
fn start_thread(probe: fn() -> io::Result<()>) -> io::Result<()> {
    // Block signals on the creating thread so the child cannot execute an
    // inherited handler during setup. SIGCHLD must not auto-reap our children.
    // SAFETY: the integer-only signal structures are initialized before use.
    unsafe {
        let all = u64::MAX;
        check(libc::syscall(
            libc::SYS_rt_sigprocmask,
            libc::SIG_BLOCK,
            &raw const all,
            std::ptr::null::<u8>(),
            8usize,
        ))?;
        let mut action: libc::sigaction = mem::zeroed();
        check(libc::sigaction(
            libc::SIGCHLD,
            std::ptr::null(),
            &mut action,
        ))?;
        if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            if action.sa_sigaction == libc::SIG_IGN {
                action.sa_sigaction = libc::SIG_DFL;
            }
            action.sa_flags &= !libc::SA_NOCLDWAIT;
            check(libc::sigaction(
                libc::SIGCHLD,
                &action,
                std::ptr::null_mut(),
            ))?;
        }
    }
    probe()?;
    // SAFETY: process-wide settings for a freshly admitted Exec run.
    unsafe {
        check(libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0))?;
        check(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0))?;
    }
    Ok(())
}

impl Shared {
    fn wake(&self) {
        let one = 1u64;
        // SAFETY: eventfd is live and reads exactly one u64. EAGAIN means
        // a wake is already pending in the saturated counter.
        while unsafe {
            hirsel_core_net::sys::eventfd_write(self.change.as_raw_fd(), (&raw const one).cast(), 8)
        } < 0
            && errno() == libc::EINTR
        {}
    }

    fn run(&self) {
        loop {
            let mut events = [hirsel_core_net::sys::epoll_event { events: 0, u64: 0 }; 16];
            // SAFETY: the epoll instance is live and writes at most this array.
            let count = unsafe {
                hirsel_core_net::sys::epoll_wait(
                    self.poller.as_raw_fd(),
                    events.as_mut_ptr(),
                    events.len() as i32,
                    -1,
                )
            };
            if count < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                crate::process::stop(b"ExecSupervisor");
            }
            if events[..count as usize].iter().any(|e| e.u64 == 0) {
                let mut pending = 0u64;
                // SAFETY: only this thread drains the live eventfd.
                unsafe {
                    hirsel_core_net::sys::eventfd_read(
                        self.change.as_raw_fd(),
                        (&raw mut pending).cast(),
                        8,
                    )
                };
            }
            let mut registry = self.registry.lock().unwrap();
            let mut ready: [_; 16] = std::array::from_fn(|_| None);
            for (event, slot) in events[..count as usize].iter().zip(&mut ready) {
                if let Some(fd) = event.u64.checked_sub(1)
                    && let Some(child) = registry
                        .children
                        .iter_mut()
                        .find(|child| child.fd.as_raw_fd() as u64 == fd)
                {
                    *slot = child.waiter.take();
                }
            }
            if ready.iter().any(Option::is_some) {
                // A scheduler may hold its ready queue while entering a callback.
                drop(registry);
                for (outside, token) in ready.into_iter().flatten() {
                    outside.complete(token);
                }
                registry = self.registry.lock().unwrap();
            }
            if let Some((input, failure)) = registry.spawn.take() {
                let started = self.clone_child(&mut registry, input, failure);
                drop(registry);
                let result = self.finish_spawn(started);
                registry = self.registry.lock().unwrap();
                registry.result = Some(result);
                self.completed.notify_one();
            }

            if registry.stopping {
                for child in &mut registry.children {
                    if child.disposition == Disposition::Live {
                        release(child);
                    }
                }
            }
            let stopping = registry.stopping;
            registry.children.retain(|child| match child.disposition {
                Disposition::Live => true,
                Disposition::Reaped => !stopping,
                Disposition::Released => wait(child.pid, child.fd.as_raw_fd(), libc::WNOHANG)
                    .unwrap_or_else(|_| crate::process::stop(b"ExecReap"))
                    .is_none(),
                Disposition::Orphan => {
                    matches!(
                        wait(child.pid, child.fd.as_raw_fd(), libc::WNOHANG),
                        Ok(None)
                    )
                }
            });
            self.adopt_orphans(&mut registry);
            if stopping
                && !registry
                    .children
                    .iter()
                    .any(|child| child.disposition == Disposition::Released)
            {
                return;
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn clone_child(
        &self,
        registry: &mut Registry,
        input: SpawnInput,
        failure: Option<SetupStep>,
    ) -> Result<Starting, SpawnError> {
        registry
            .children
            .try_reserve(1)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        let (reader, writer) = pipe()?;
        let mut streams = Streams::new(input.piped)?;
        let argv = pointers(&input.argv)?;
        let envp = pointers(&input.envp)?;
        let fds = [
            writer.as_raw_fd(),
            input.image.as_raw_fd(),
            input.cwd.as_raw_fd(),
            streams.child[0].as_raw_fd(),
            streams.child[1].as_raw_fd(),
            streams.child[2].as_raw_fd(),
        ];
        let run = std::process::id() as libc::pid_t;
        let mut pidfd = -1i32;
        // Linux clone_args version 0 (Linux 5.3).
        let mut args = [0u64; 8];
        args[0] = libc::CLONE_PIDFD as u64;
        args[1] = (&raw mut pidfd) as u64;
        args[4] = libc::SIGCHLD as u64;
        // SAFETY: clone3 copies the caller's descriptor table and uses the
        // inherited stack. The child never returns to Rust's runtime.
        let pid =
            unsafe { libc::syscall(libc::SYS_clone3, args.as_ptr(), mem::size_of_val(&args)) };
        if pid == 0 {
            // SAFETY: fork-like clone child, with private fds, pinned C strings
            // and pointer tables, and signals blocked until dispositions reset.
            unsafe { execute(fds, run, argv.as_ptr(), envp.as_ptr(), failure) }
        }
        check(pid)?;
        // SAFETY: CLONE_PIDFD transfers a fresh descriptor to the parent.
        let fd = unsafe { OwnedFd::from_raw_fd(pidfd) };
        let monitored = watch(&self.poller, &fd, pidfd as u64 + 1, true);
        registry.children.push(Child {
            pid: pid as libc::pid_t,
            fd,
            disposition: Disposition::Live,
            waiter: None,
        });
        drop(writer);
        let pipes = mem::take(&mut streams.parent);
        Ok(Starting {
            pid: pid as libc::pid_t,
            pidfd,
            reader,
            pipes,
            monitored,
        })
    }

    fn finish_spawn(&self, starting: Result<Starting, SpawnError>) -> Result<Started, SpawnError> {
        let Starting {
            pid,
            pidfd,
            reader,
            pipes,
            monitored,
        } = starting?;
        let error = match monitored {
            Ok(()) => match read_report(reader) {
                Ok(None) => return Ok(Started { pidfd, pipes }),
                Ok(Some(error)) => error,
                Err(error) => {
                    let _ = send(pid, pidfd, Signal::Kill);
                    error.into()
                }
            },
            Err(error) => {
                let _ = send(pid, pidfd, Signal::Kill);
                error.into()
            }
        };
        // No Process was issued. Only this thread can reap this child.
        if wait(pid, pidfd, 0).is_err() {
            crate::process::stop(b"ExecReap");
        }
        self.registry
            .lock()
            .unwrap()
            .children
            .retain(|child| child.fd.as_raw_fd() != pidfd);
        Err(error)
    }

    #[cfg(target_os = "linux")]
    fn adopt_orphans(&self, registry: &mut Registry) {
        let Ok(tasks) = fs::read_dir("/proc/self/task") else {
            return;
        };
        for task in tasks.flatten() {
            let Ok(list) = fs::read_to_string(task.path().join("children")) else {
                continue;
            };
            for pid in list
                .split_ascii_whitespace()
                .filter_map(|pid| pid.parse::<libc::pid_t>().ok())
            {
                if registry.children.iter().any(|child| child.pid == pid)
                    || registry.children.try_reserve(1).is_err()
                {
                    continue;
                }
                // SAFETY: the pid is only used to acquire a candidate fd.
                let Ok(fd) =
                    owned(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) } as i32)
                else {
                    continue;
                };
                let parent = wait(pid, fd.as_raw_fd(), libc::WNOHANG | libc::WNOWAIT).is_ok();
                if !parent {
                    continue;
                }
                #[cfg(test)]
                registry.orphans.push((pid, parent));
                if send(pid, fd.as_raw_fd(), Signal::Kill).is_err() {
                    continue;
                }
                match wait(pid, fd.as_raw_fd(), libc::WNOHANG) {
                    Ok(None)
                        if watch(&self.poller, &fd, fd.as_raw_fd() as u64 + 1, true).is_ok() =>
                    {
                        registry.children.push(Child {
                            pid,
                            fd,
                            disposition: Disposition::Orphan,
                            waiter: None,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

struct Started {
    pidfd: RawFd,
    pipes: [Option<OwnedFd>; 3],
}

struct Starting {
    pid: libc::pid_t,
    pidfd: RawFd,
    reader: OwnedFd,
    pipes: [Option<OwnedFd>; 3],
    monitored: io::Result<()>,
}

fn release(child: &mut Child) {
    child.waiter = None;
    if let Err(error) = send(child.pid, child.fd.as_raw_fd(), Signal::Kill)
        && error.raw_os_error() != Some(libc::ESRCH)
    {
        crate::process::stop(b"ExecKill");
    }
    child.disposition = Disposition::Released;
}

fn watch(poller: &OwnedFd, fd: &OwnedFd, tag: u64, once: bool) -> io::Result<()> {
    let mut event = hirsel_core_net::sys::epoll_event {
        events: (hirsel_core_net::sys::EPOLLIN
            | if once {
                hirsel_core_net::sys::EPOLLONESHOT
            } else {
                0
            }) as u32,
        u64: tag,
    };
    // SAFETY: both fds are live and the event is read synchronously.
    check(unsafe {
        hirsel_core_net::sys::epoll_ctl(
            poller.as_raw_fd(),
            hirsel_core_net::sys::EPOLL_CTL_ADD,
            fd.as_raw_fd(),
            &mut event,
        )
    })
}

#[cfg(target_os = "linux")]
fn wait(_pid: libc::pid_t, fd: RawFd, options: i32) -> io::Result<Option<Termination>> {
    loop {
        // SAFETY: waitid writes the documented siginfo variant on success.
        let mut info: libc::siginfo_t = unsafe { mem::zeroed() };
        let status = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                fd as libc::id_t,
                &mut info,
                libc::WEXITED | libc::__WALL | options,
            )
        };
        if status == 0 {
            return Ok(match info.si_code {
                libc::CLD_EXITED => Some(Termination::Exited(unsafe { info.si_status() } as u8)),
                libc::CLD_KILLED | libc::CLD_DUMPED => {
                    Some(Termination::Signalled(unsafe { info.si_status() } as u8))
                }
                _ => None,
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn send(_pid: libc::pid_t, fd: RawFd, signal: Signal) -> io::Result<()> {
    // SAFETY: the fd pins the process identity. Null siginfo requests standard user signalling.
    check(unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd,
            signal.number(),
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    })
}

fn errno() -> libc::c_int {
    // SAFETY: this thread owns its errno.
    unsafe { *hirsel_core_net::sys::errno_location() }
}

fn check<N: Into<i64>>(status: N) -> io::Result<()> {
    if status.into() < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn owned(fd: RawFd) -> io::Result<OwnedFd> {
    check(fd)?;
    // SAFETY: this receives a freshly allocated file descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut ends = [-1; 2];
    // SAFETY: pipe2 writes exactly two new descriptors.
    check(unsafe { hirsel_core_net::sys::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) })?;
    Ok((owned(ends[0])?, owned(ends[1])?))
}

struct Streams {
    child: [OwnedFd; 3],
    parent: [Option<OwnedFd>; 3],
}

impl Streams {
    fn new([stdin, stdout, stderr]: [bool; 3]) -> io::Result<Self> {
        let (input, input_parent) = Self::one(stdin, false)?;
        let (output, output_parent) = Self::one(stdout, true)?;
        let (error, error_parent) = Self::one(stderr, true)?;
        Ok(Self {
            child: [input, output, error],
            parent: [input_parent, output_parent, error_parent],
        })
    }

    fn one(piped: bool, write: bool) -> io::Result<(OwnedFd, Option<OwnedFd>)> {
        if !piped {
            let mode = if write {
                libc::O_WRONLY
            } else {
                libc::O_RDONLY
            };
            // SAFETY: constant path and no creation flags.
            let null = owned(unsafe { libc::open(c"/dev/null".as_ptr(), mode | libc::O_CLOEXEC) })?;
            return Ok((null, None));
        }
        let (read, send) = pipe()?;
        let (child, parent) = if write { (send, read) } else { (read, send) };
        // SAFETY: each pipe end has a distinct open file description. Change
        // the reactor's end only, leaving blocking stdio for the child.
        check(unsafe { libc::fcntl(parent.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) })?;
        Ok((child, Some(parent)))
    }
}

fn pointers(strings: &[CString]) -> io::Result<Vec<*const libc::c_char>> {
    let mut pointers = Vec::new();
    pointers
        .try_reserve_exact(strings.len().saturating_add(1))
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
    pointers.extend(strings.iter().map(|s| s.as_ptr()));
    pointers.push(std::ptr::null());
    Ok(pointers)
}

#[repr(C)]
struct SetupReport {
    step: i32,
    errno: i32,
}

fn read_report(reader: OwnedFd) -> io::Result<Option<SpawnError>> {
    use io::Read;
    let mut reader = fs::File::from(reader);
    let mut report = [0; mem::size_of::<SetupReport>()];
    let mut count = 0;
    while count != report.len() {
        match reader.read(&mut report[count..]) {
            Ok(0) if count == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from_raw_os_error(libc::EIO)),
            Ok(n) => count += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    let (step, error) = report.split_at(4);
    let step = match i32::from_ne_bytes(step.try_into().unwrap()) {
        1 => SetupStep::Stdio,
        2 => SetupStep::Cwd,
        3 => SetupStep::Close,
        4 => SetupStep::Signals,
        5 => SetupStep::Exec,
        _ => return Err(io::Error::from_raw_os_error(libc::EIO)),
    };
    Ok(Some(SpawnError {
        error: io::Error::from_raw_os_error(i32::from_ne_bytes(error.try_into().unwrap())),
        step: Some(step),
    }))
}

// The child runs on an inherited stack without access to the spawning
// process's locks, allocator or thread runtime. These helpers make only
// Linux syscalls or async-signal-safe libc calls and never unwind.
#[cfg(target_os = "linux")]
unsafe fn execute(
    mut fds: [RawFd; 6],
    run: libc::pid_t,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
    failure: Option<SetupStep>,
) -> ! {
    unsafe {
        let mut report = fds[0];
        inject(report, SetupStep::Stdio, failure);
        for (i, fd) in fds.iter_mut().enumerate() {
            if *fd <= 2 {
                let moved = libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 3);
                if moved < 0 {
                    failed(report, SetupStep::Stdio, errno());
                }
                *fd = moved;
                if i == 0 {
                    report = moved;
                }
            }
        }
        let [report, image, cwd, stdin, stdout, stderr] = fds;
        for (fd, target) in [(stdin, 0), (stdout, 1), (stderr, 2)] {
            if hirsel_core_net::sys::dup3(fd, target, 0) < 0 {
                failed(report, SetupStep::Stdio, errno());
            }
        }

        inject(report, SetupStep::Cwd, failure);
        if libc::fchdir(cwd) < 0 {
            failed(report, SetupStep::Cwd, errno());
        }

        inject(report, SetupStep::Close, failure);
        let (low, high) = (image.min(report) as u32, image.max(report) as u32);
        for (first, last) in [(3, low - 1), (low + 1, high - 1), (high + 1, u32::MAX)] {
            if first <= last && libc::syscall(libc::SYS_close_range, first, last, 0u32) < 0 {
                failed(report, SetupStep::Close, errno());
            }
        }

        inject(report, SetupStep::Signals, failure);
        // Linux x86_64 and aarch64 kernel sigaction: zero handler is SIG_DFL.
        // Use the kernel ABI to include 32 and 33, which glibc reserves and
        // refuses to reset. All fields, including restorer and mask, are zero.
        let action = [0u64; 4];
        for signal in 1..=64 {
            if signal != libc::SIGKILL
                && signal != libc::SIGSTOP
                && libc::syscall(
                    libc::SYS_rt_sigaction,
                    signal,
                    action.as_ptr(),
                    std::ptr::null::<u8>(),
                    8usize,
                ) < 0
            {
                failed(report, SetupStep::Signals, errno());
            }
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0
            || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0
        {
            failed(report, SetupStep::Signals, errno());
        }
        if libc::getppid() != run {
            failed(report, SetupStep::Signals, libc::ECHILD);
        }
        let empty = 0u64;
        if libc::syscall(
            libc::SYS_rt_sigprocmask,
            libc::SIG_SETMASK,
            &raw const empty,
            std::ptr::null::<u8>(),
            8usize,
        ) < 0
        {
            failed(report, SetupStep::Signals, errno());
        }

        inject(report, SetupStep::Exec, failure);
        libc::syscall(
            libc::SYS_execveat,
            image,
            c"".as_ptr(),
            argv,
            envp,
            libc::AT_EMPTY_PATH,
        );
        failed(report, SetupStep::Exec, errno());
    }
}

#[cfg(target_os = "linux")]
unsafe fn inject(report: RawFd, step: SetupStep, failure: Option<SetupStep>) {
    if failure == Some(step) {
        unsafe { failed(report, step, libc::EIO) }
    }
}

#[cfg(target_os = "linux")]
unsafe fn failed(report: RawFd, step: SetupStep, error: libc::c_int) -> ! {
    let message = SetupReport {
        step: step as i32,
        errno: error,
    };
    // SAFETY: the pipe writer is pinned; the fixed record fits in PIPE_BUF.
    unsafe {
        while libc::write(
            report,
            (&raw const message).cast(),
            mem::size_of_val(&message),
        ) < 0
            && errno() == libc::EINTR
        {}
        libc::_exit(127);
    }
}

#[cfg(target_os = "macos")]
fn start_thread(probe: fn() -> io::Result<()>) -> io::Result<()> {
    // Block signals on the creating thread. SIGCHLD must not auto-reap our
    // children. posix_spawn resets every disposition and the mask in the child.
    // SAFETY: the integer-only signal structures are initialized before use.
    unsafe {
        let mut all: libc::sigset_t = mem::zeroed();
        libc::sigfillset(&mut all);
        let blocked = libc::pthread_sigmask(libc::SIG_BLOCK, &all, std::ptr::null_mut());
        if blocked != 0 {
            return Err(io::Error::from_raw_os_error(blocked));
        }
        let mut action: libc::sigaction = mem::zeroed();
        check(libc::sigaction(
            libc::SIGCHLD,
            std::ptr::null(),
            &mut action,
        ))?;
        if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            if action.sa_sigaction == libc::SIG_IGN {
                action.sa_sigaction = libc::SIG_DFL;
            }
            action.sa_flags &= !libc::SA_NOCLDWAIT;
            check(libc::sigaction(
                libc::SIGCHLD,
                &action,
                std::ptr::null_mut(),
            ))?;
        }
    }
    probe()?;
    // Linux makes the run non-dumpable; the nearest macOS setting refuses
    // every debugger attachment.
    // SAFETY: a process-wide setting for a freshly admitted Exec run.
    check(unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) })?;
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn posix_spawn_file_actions_addfchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
impl Shared {
    fn clone_child(
        &self,
        registry: &mut Registry,
        input: SpawnInput,
        failure: Option<SetupStep>,
    ) -> Result<Starting, SpawnError> {
        registry
            .children
            .try_reserve(1)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        if let Some(step) = failure {
            return Err(SpawnError {
                error: io::Error::from_raw_os_error(libc::EIO),
                step: Some(step),
            });
        }
        // posix_spawn reports a setup failure as its result: no report pipe
        // is written, and the reader sees end of file at once.
        let (reader, _) = pipe()?;
        let mut streams = Streams::new(input.piped)?;
        // A stream at 0-2 would be overwritten by an earlier dup2.
        for fd in &mut streams.child {
            if fd.as_raw_fd() <= 2 {
                // SAFETY: duplicating a live descriptor above the stdio slots.
                *fd = owned(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) })?;
            }
        }
        let argv = pointers(&input.argv)?;
        let envp = pointers(&input.envp)?;
        let image = image_path(input.image.as_raw_fd())?;
        let mut pid: libc::pid_t = -1;
        // SAFETY: the actions and attributes are initialized before use and
        // destroyed after. argv, envp and the image path are pinned C strings.
        let spawned = unsafe {
            let mut actions: libc::posix_spawn_file_actions_t = mem::zeroed();
            let mut attributes: libc::posix_spawnattr_t = mem::zeroed();
            let mut status = libc::posix_spawn_file_actions_init(&mut actions);
            if status == 0 {
                status = libc::posix_spawnattr_init(&mut attributes);
                if status != 0 {
                    libc::posix_spawn_file_actions_destroy(&mut actions);
                }
            }
            if status == 0 {
                for (fd, target) in streams.child.iter().zip(0..) {
                    if status == 0 {
                        status = libc::posix_spawn_file_actions_adddup2(
                            &mut actions,
                            fd.as_raw_fd(),
                            target,
                        );
                    }
                }
                if status == 0 {
                    status =
                        posix_spawn_file_actions_addfchdir_np(&mut actions, input.cwd.as_raw_fd());
                }
                let mut defaults: libc::sigset_t = mem::zeroed();
                libc::sigfillset(&mut defaults);
                libc::sigdelset(&mut defaults, libc::SIGKILL);
                libc::sigdelset(&mut defaults, libc::SIGSTOP);
                let mut mask: libc::sigset_t = mem::zeroed();
                libc::sigemptyset(&mut mask);
                if status == 0 {
                    status = libc::posix_spawnattr_setsigdefault(&mut attributes, &defaults);
                }
                if status == 0 {
                    status = libc::posix_spawnattr_setsigmask(&mut attributes, &mask);
                }
                if status == 0 {
                    // Only the three dup2 targets survive into the child.
                    status = libc::posix_spawnattr_setflags(
                        &mut attributes,
                        (libc::POSIX_SPAWN_CLOEXEC_DEFAULT
                            | libc::POSIX_SPAWN_SETSIGDEF
                            | libc::POSIX_SPAWN_SETSIGMASK)
                            as libc::c_short,
                    );
                }
                if status == 0 {
                    status = libc::posix_spawn(
                        &mut pid,
                        image.as_ptr(),
                        &actions,
                        &attributes,
                        argv.as_ptr().cast(),
                        envp.as_ptr().cast(),
                    );
                }
                libc::posix_spawnattr_destroy(&mut attributes);
                libc::posix_spawn_file_actions_destroy(&mut actions);
            }
            status
        };
        if spawned != 0 {
            return Err(SpawnError {
                error: io::Error::from_raw_os_error(spawned),
                step: Some(SetupStep::Exec),
            });
        }
        let fd = match exit_watch(pid) {
            Ok(fd) => fd,
            Err(error) => {
                // SAFETY: the unreaped PID is this new child's.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                let _ = wait(pid, -1, 0);
                return Err(error.into());
            }
        };
        let pidfd = fd.as_raw_fd();
        let monitored = watch(&self.poller, &fd, pidfd as u64 + 1, true);
        registry.children.push(Child {
            pid,
            fd,
            disposition: Disposition::Live,
            waiter: None,
        });
        let pipes = mem::take(&mut streams.parent);
        Ok(Starting {
            pid,
            pidfd,
            reader,
            pipes,
            monitored,
        })
    }

    fn adopt_orphans(&self, _registry: &mut Registry) {}
}

/// The path of the pinned image copy, which macOS executes by name.
#[cfg(target_os = "macos")]
fn image_path(fd: RawFd) -> io::Result<CString> {
    let mut path = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes.
    check(unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) })?;
    Ok(CStr::from_bytes_until_nul(&path)
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?
        .to_owned())
}

/// A kqueue that becomes readable when `pid` exits: NOTE_EXIT, or a user
/// event already triggered when the child exited before the watch.
#[cfg(target_os = "macos")]
fn exit_watch(pid: libc::pid_t) -> io::Result<OwnedFd> {
    // SAFETY: success returns a new descriptor.
    let queue = owned(unsafe { libc::kqueue() })?;
    // SAFETY: setting close-on-exec on a live descriptor.
    check(unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
    let event = |ident, filter, flags, fflags| libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let change = event(
        pid as usize,
        libc::EVFILT_PROC,
        libc::EV_ADD | libc::EV_RECEIPT,
        libc::NOTE_EXIT,
    );
    // SAFETY: an all-integer receipt slot.
    let mut receipt: libc::kevent = unsafe { mem::zeroed() };
    // SAFETY: the queue, the change and the receipt are live.
    check(unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &change,
            1,
            &mut receipt,
            1,
            std::ptr::null(),
        )
    })?;
    if receipt.flags & libc::EV_ERROR != 0 && receipt.data != 0 {
        if receipt.data != libc::ESRCH as isize {
            return Err(io::Error::from_raw_os_error(receipt.data as i32));
        }
        let exited = [
            event(0, libc::EVFILT_USER, libc::EV_ADD, 0),
            event(0, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER),
        ];
        // SAFETY: the queue and both changes are live.
        check(unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                exited.as_ptr(),
                2,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        })?;
    }
    Ok(queue)
}

#[cfg(target_os = "macos")]
fn wait(pid: libc::pid_t, _fd: RawFd, options: i32) -> io::Result<Option<Termination>> {
    loop {
        let mut status = 0;
        // SAFETY: waitpid writes one int; only this registry reaps its children.
        let reaped = unsafe { libc::waitpid(pid, &mut status, options) };
        if reaped == pid {
            return Ok(if libc::WIFEXITED(status) {
                Some(Termination::Exited(libc::WEXITSTATUS(status) as u8))
            } else if libc::WIFSIGNALED(status) {
                Some(Termination::Signalled(libc::WTERMSIG(status) as u8))
            } else {
                None
            });
        }
        if reaped == 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "macos")]
fn send(pid: libc::pid_t, _fd: RawFd, signal: Signal) -> io::Result<()> {
    // SAFETY: the PID is a direct child that this registry has not reaped.
    check(unsafe { libc::kill(pid, signal.number()) })
}

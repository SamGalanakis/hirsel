//! Darwin Exec child creation and custody. Only the supervisor creates
//! children, with `posix_spawn`: the child never runs code of ours, so setup
//! happens in the kernel from the file actions and attributes below. A child
//! is named by its pid, which stays its own until this registry reaps it; the
//! registry lock orders every signal before the reap that frees the pid.
//!
//! Darwin limits against Linux: no subreaper, so a grandchild a child leaves
//! behind is not adopted; no parent-death signal, so a child outlives a
//! supervisor killed without unwinding; no non-dumpable flag.

use hirsel_core_net::ready::{Child as Watched, Event, Selector, Wake};
use std::{
    ffi::{CStr, CString},
    fs, io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

/// `POSIX_SPAWN_SETSID` from `<spawn.h>`, not yet exported by the libc crate.
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

const EXEC_HOST: &str =
    "Exec requires posix_spawn with confined opens and cloned images (macOS 26 or later)";

/// The launcher's resolved inputs, constructed after issue. The image is the
/// read-only description of the immutable private copy.
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

/// The setup a spawn performs, in order. `posix_spawn` reports one errno for
/// all of them, which this provider attributes to `Exec`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
#[expect(
    dead_code,
    reason = "the steps mirror Linux's; posix_spawn reports only Exec"
)]
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
    disposition: Disposition,
    waiter: Option<(crate::tasks::Outside, u64)>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Disposition {
    Live,
    Reaped,
    Released,
}

#[derive(Default)]
struct Registry {
    children: Vec<Child>,
    spawn: Option<(SpawnInput, Option<SetupStep>)>,
    result: Option<Result<Started, SpawnError>>,
    stopping: bool,
}

struct Shared {
    registry: Mutex<Registry>,
    completed: Condvar,
    change: Wake,
    poller: Selector,
}

/// Owns the thread that creates direct children and reaps released ones.
/// Drop completes orderly process cleanup.
pub struct Supervisor {
    shared: Arc<Shared>,
    spawn_slot: Mutex<()>,
    thread: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// The launcher supplies its startup tests. They run on the
    /// child-creating thread.
    pub fn start(probe: fn() -> io::Result<()>) -> io::Result<Self> {
        let poller = Selector::new()?;
        let change = Wake::new(&poller, 0)?;
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
            pid,
            pipes: [stdin, stdout, stderr],
        } = registry.result.take().unwrap()?;
        Ok(Spawned {
            process: Process {
                shared: self.shared.clone(),
                pid,
                termination: None,
            },
            stdin,
            stdout,
            stderr,
        })
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
    pid: libc::pid_t,
    termination: Option<Termination>,
}

impl Process {
    pub fn cached(&self) -> Option<Termination> {
        self.termination
    }

    pub fn try_wait(&mut self) -> io::Result<Option<Termination>> {
        self.wait_or_park(None)
    }

    pub(crate) fn wait_or_park(&mut self, token: Option<u64>) -> io::Result<Option<Termination>> {
        if self.termination.is_some() {
            return Ok(self.termination);
        }
        let mut registry = self.shared.registry.lock().unwrap();
        let child = direct(&mut registry, self.pid)?;
        if let Some(status) = wait(child.pid, libc::WNOHANG)? {
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
        if let Ok(child) = direct(&mut registry, self.pid) {
            child.waiter = None;
        }
    }

    pub fn signal(&mut self, signal: Signal) -> io::Result<()> {
        if self.termination.is_some() {
            return Ok(());
        }
        let mut registry = self.shared.registry.lock().unwrap();
        let child = direct(&mut registry, self.pid)?;
        send(child.pid, signal)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut registry = self.shared.registry.lock().unwrap();
        if let Ok(child) = direct(&mut registry, self.pid) {
            if child.disposition == Disposition::Live {
                release(child);
            } else {
                registry.children.retain(|child| child.pid != self.pid);
            }
            self.shared.wake();
        }
    }
}

fn direct(registry: &mut Registry, pid: libc::pid_t) -> io::Result<&mut Child> {
    if registry.stopping {
        return Err(io::Error::from_raw_os_error(libc::ECHILD));
    }
    registry
        .children
        .iter_mut()
        .find(|child| child.pid == pid)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ECHILD))
}

/// Exec's startup checks: an unprivileged run with confined opens and image
/// copies. `posix_spawn` needs no probe.
pub(crate) fn probe() -> io::Result<()> {
    // SAFETY: the id getters have no failure mode.
    let uids = unsafe { [libc::getuid(), libc::geteuid(), libc::geteuid()] };
    if !crate::exec::unprivileged(uids, [0; 2], [0; 2]) {
        return Err(io::Error::other("Exec requires an unprivileged run"));
    }
    let beneath = fs::File::open(".").and_then(|root| {
        use std::os::fd::AsFd;
        hirsel_core_net::unix::open_beneath(root.as_fd(), c".", crate::host::path::DIRECTORY_HANDLE)
    });
    if beneath.is_err() || hirsel_core_net::exec::probe_seals().is_err() {
        return Err(io::Error::other(EXEC_HOST));
    }
    Ok(())
}

fn start_thread(probe: fn() -> io::Result<()>) -> io::Result<()> {
    // Block signals on the supervisor thread, as the Linux provider does;
    // the child's mask is set explicitly at spawn. SIGCHLD must not
    // auto-reap our children.
    // SAFETY: the signal sets and actions are initialized before use.
    unsafe {
        let mut all: libc::sigset_t = mem::zeroed();
        libc::sigfillset(&mut all);
        let rc = libc::pthread_sigmask(libc::SIG_BLOCK, &all, std::ptr::null_mut());
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
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
    probe()
}

impl Shared {
    fn wake(&self) {
        self.change.wake();
    }

    fn run(&self) {
        loop {
            let mut events = [Event::EMPTY; 16];
            let count = match self.poller.wait(&mut events, None) {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => crate::process::stop(b"ExecSupervisor"),
            };
            if events[..count].iter().any(|e| e.token() == 0) {
                // Only this thread takes the pending wake.
                self.change.reset();
            }
            let mut registry = self.registry.lock().unwrap();
            let mut ready: [_; 16] = std::array::from_fn(|_| None);
            for (event, slot) in events[..count].iter().zip(&mut ready) {
                if let Some(pid) = event.token().checked_sub(1)
                    && let Some(child) = registry
                        .children
                        .iter_mut()
                        .find(|child| child.pid as u64 == pid)
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
                let result = self.spawn_child(&mut registry, input, failure);
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
                Disposition::Released => wait(child.pid, libc::WNOHANG)
                    .unwrap_or_else(|_| crate::process::stop(b"ExecReap"))
                    .is_none(),
            });
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

    fn spawn_child(
        &self,
        registry: &mut Registry,
        input: SpawnInput,
        failure: Option<SetupStep>,
    ) -> Result<Started, SpawnError> {
        if let Some(step) = failure {
            return Err(SpawnError {
                error: io::Error::from_raw_os_error(libc::EIO),
                step: Some(step),
            });
        }
        registry
            .children
            .try_reserve(1)
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        let mut streams = Streams::new(input.piped)?;
        let path = image_path(input.image.as_raw_fd())?;
        let argv = pointers(&input.argv)?;
        let envp = pointers(&input.envp)?;
        let mut actions = Actions::new()?;
        for (stream, target) in streams.child.iter().zip(0..) {
            // SAFETY: the actions object is initialized; the fd is ours.
            spawned(unsafe {
                libc::posix_spawn_file_actions_adddup2(&mut actions.0, stream.as_raw_fd(), target)
            })?;
        }
        // SAFETY: as above; the directory fd stays open through the spawn.
        spawned(unsafe {
            libc::posix_spawn_file_actions_addfchdir(&mut actions.0, input.cwd.as_raw_fd())
        })?;
        let attributes = Attributes::new()?;
        let mut pid: libc::pid_t = 0;
        // SAFETY: path, argv and envp are NUL-terminated and pinned through
        // the call; the actions and attributes are initialized.
        let rc = unsafe {
            libc::posix_spawn(
                &mut pid,
                path.as_ptr(),
                &actions.0,
                &attributes.0,
                argv.as_ptr().cast(),
                envp.as_ptr().cast(),
            )
        };
        if rc != 0 {
            return Err(SpawnError {
                error: io::Error::from_raw_os_error(rc),
                step: Some(SetupStep::Exec),
            });
        }
        // A child that exited before the watch is a zombie the next wait
        // reaps at once, so ESRCH needs no watch.
        if let Err(error) = self.poller.add_exit(Watched::new(pid), pid as u64 + 1)
            && error.raw_os_error() != Some(libc::ESRCH)
        {
            let _ = send(pid, Signal::Kill);
            // No Process was issued. Only this thread can reap this child.
            if wait(pid, 0).is_err() {
                crate::process::stop(b"ExecReap");
            }
            return Err(error.into());
        }
        registry.children.push(Child {
            pid,
            disposition: Disposition::Live,
            waiter: None,
        });
        Ok(Started {
            pid,
            pipes: mem::take(&mut streams.parent),
        })
    }
}

struct Started {
    pid: libc::pid_t,
    pipes: [Option<OwnedFd>; 3],
}

fn release(child: &mut Child) {
    child.waiter = None;
    if let Err(error) = send(child.pid, Signal::Kill)
        && error.raw_os_error() != Some(libc::ESRCH)
    {
        crate::process::stop(b"ExecKill");
    }
    child.disposition = Disposition::Released;
}

fn wait(pid: libc::pid_t, options: i32) -> io::Result<Option<Termination>> {
    loop {
        let mut status = 0;
        // SAFETY: waitpid writes the status of this unreaped child.
        let rc = unsafe { libc::waitpid(pid, &mut status, options) };
        if rc == pid {
            return Ok(if libc::WIFEXITED(status) {
                Some(Termination::Exited(libc::WEXITSTATUS(status) as u8))
            } else if libc::WIFSIGNALED(status) {
                Some(Termination::Signalled(libc::WTERMSIG(status) as u8))
            } else {
                None
            });
        }
        if rc == 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn send(pid: libc::pid_t, signal: Signal) -> io::Result<()> {
    // SAFETY: the registry has not reaped pid, so it still names our child.
    check(unsafe { libc::kill(pid, signal.number()) })
}

fn check(status: libc::c_int) -> io::Result<()> {
    if status < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `posix_spawn*` calls return their errno rather than setting it.
fn spawned(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

fn owned(fd: RawFd) -> io::Result<OwnedFd> {
    check(fd)?;
    // SAFETY: this receives a freshly allocated file descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The path of the pinned image: the immutable copy in the run's private
/// directory, which nothing else can rename or replace.
fn image_path(image: RawFd) -> io::Result<CString> {
    let mut buffer = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes, NUL included.
    check(unsafe { libc::fcntl(image, libc::F_GETPATH, buffer.as_mut_ptr()) })?;
    let path = CStr::from_bytes_until_nul(&buffer).map_err(|_| io::ErrorKind::InvalidData)?;
    Ok(path.to_owned())
}

/// A close-on-exec pipe; under `POSIX_SPAWN_CLOEXEC_DEFAULT` only the
/// dup2 targets reach the child anyway.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut ends = [-1; 2];
    // SAFETY: pipe writes exactly two new descriptors.
    check(unsafe { libc::pipe(ends.as_mut_ptr()) })?;
    let ends = (owned(ends[0])?, owned(ends[1])?);
    for end in [&ends.0, &ends.1] {
        // SAFETY: a descriptor this call just created.
        check(unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
    }
    Ok(ends)
}

/// Keep a child stream off 0..=2, so one dup2 action cannot overwrite the
/// source of a later one.
fn above_stdio(fd: OwnedFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        return Ok(fd);
    }
    // SAFETY: duplicates a live descriptor we own.
    owned(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) })
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
            return Ok((above_stdio(null)?, None));
        }
        let (read, send) = pipe()?;
        let (child, parent) = if write { (send, read) } else { (read, send) };
        // SAFETY: each pipe end has a distinct open file description. Change
        // the reactor's end only, leaving blocking stdio for the child.
        check(unsafe { libc::fcntl(parent.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) })?;
        Ok((above_stdio(child)?, Some(parent)))
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

/// The spawn's file actions: stdio onto 0..=2 and the working directory.
struct Actions(libc::posix_spawn_file_actions_t);

impl Actions {
    fn new() -> io::Result<Self> {
        let mut actions = std::ptr::null_mut();
        // SAFETY: init allocates the opaque actions object.
        spawned(unsafe { libc::posix_spawn_file_actions_init(&mut actions) })?;
        Ok(Self(actions))
    }
}

impl Drop for Actions {
    fn drop(&mut self) {
        // SAFETY: initialized in `new` and destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// The spawn's attributes: close every descriptor not named by an action,
/// start a new session, default every signal and clear the mask.
struct Attributes(libc::posix_spawnattr_t);

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut attributes = std::ptr::null_mut();
        // SAFETY: init allocates the opaque attributes object.
        spawned(unsafe { libc::posix_spawnattr_init(&mut attributes) })?;
        let mut this = Self(attributes);
        let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | POSIX_SPAWN_SETSID
            | libc::POSIX_SPAWN_SETSIGDEF
            | libc::POSIX_SPAWN_SETSIGMASK;
        // SAFETY: the sets are initialized before the attributes copy them.
        unsafe {
            let mut all: libc::sigset_t = mem::zeroed();
            let mut none: libc::sigset_t = mem::zeroed();
            libc::sigfillset(&mut all);
            libc::sigemptyset(&mut none);
            spawned(libc::posix_spawnattr_setflags(
                &mut this.0,
                flags as libc::c_short,
            ))?;
            spawned(libc::posix_spawnattr_setsigdefault(&mut this.0, &all))?;
            spawned(libc::posix_spawnattr_setsigmask(&mut this.0, &none))?;
        }
        Ok(this)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized in `new` and destroyed once.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

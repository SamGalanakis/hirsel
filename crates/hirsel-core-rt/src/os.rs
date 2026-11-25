//! Linux descriptor-relative host operations. No path is authorized by a prior
//! canonicalization. openat2 resolves every traversal beneath its granted fd.
use crate::{
    host::{self, clock},
    provider::*,
};

mod listing;
mod metadata;
mod path;
mod random;
mod tables;
pub(crate) mod temp;
mod write;
use listing::Listing;
use metadata::{stat, zero_extension};
pub(crate) use path::open_beneath;
use path::{
    beneath, close_file, leaf_kind, open_directory, parent, remove, remove_tree, valid_path,
};
use std::{
    collections::BTreeMap,
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    path::Path,
    sync::{Mutex, atomic::AtomicU64},
};
use tables::Tables;
#[cfg(test)]
pub(crate) use temp::TEMP_ATTEMPTS;
pub(crate) use write::write_all;
use write::write_request;

pub(crate) type IssueFile = unsafe extern "C" fn(*mut u8, *mut Request) -> Outcome;
/// Queue a SYNC or SYNC_DIR on the sync helper; None for a handle this
/// provider does not own.
pub(crate) type SyncFile = unsafe fn(*mut u8, *mut Request, u64) -> Option<Step>;

/// Operations required by an OS directory grant. The generated launcher
/// names this table only when it supplies directory paths.
pub struct DirectoryOperations {
    pub(crate) grant: fn(&mut OsProvider, &Path) -> io::Result<u64>,
    pub(crate) probe: fn(&OsProvider, u64) -> io::Result<()>,
    pub(crate) issue: IssueFile,
    pub(crate) sync: SyncFile,
    resume: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step,
    abort: unsafe extern "C" fn(*mut u8, *mut Request, u64, u8) -> Outcome,
}

#[unsafe(no_mangle)]
pub static hirsel_core_os_directory_ops: DirectoryOperations = DirectoryOperations {
    grant: OsProvider::grant,
    probe: OsProvider::probe_directory,
    issue: issue_file,
    sync: sync_file,
    resume: resume_with_directory,
    abort: abort_with_directory,
};

pub struct OsProvider {
    issue_file: Option<IssueFile>,
    sync_file: Option<SyncFile>,
    /// Host CLOCK_MONOTONIC at construction: mono_ns and sleep deadlines count from it.
    epoch: u64,
    /// Units on every slot thread issue file operations on this one provider
    /// at once, so its handle tables sit behind a lock. No lock is held across
    /// a syscall that can wait.
    tables: Mutex<Tables>,
    /// Written only while the run starts, before any unit issues.
    pub(crate) unix_roots: BTreeMap<String, OwnedFd>,
    next: AtomicU64,
    temporary: AtomicU64,
    pub(crate) inherited_listener: Mutex<Option<OwnedFd>>,
    /// The launcher's operator listener and the one peer UID it admits, until the first
    /// UNIX_LISTEN adopts it (docs/spec/launch.md "Starting the host").
    pub(crate) inherited_unix: Mutex<Option<(OwnedFd, u32)>>,
    /// The adopted operator listener's socket key and its admitted peer UID.
    pub(crate) operator: Mutex<Option<(u64, u32)>>,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
}
impl Drop for OsProvider {
    fn drop(&mut self) {
        let tables = self.tables.get_mut().unwrap_or_else(|e| e.into_inner());
        for cell in tables.in_flight_syncs.drain(..) {
            // SAFETY: provider is ending with a sync still in flight; detach
            // its issuer handle so the helper frees the cell on completion.
            unsafe { crate::os_sync::SyncCell::detach(cell) };
        }
    }
}
impl OsProvider {
    #[cfg(test)]
    pub(crate) fn file_descriptor(&self, handle: u64) -> i32 {
        self.file(handle)
    }
    #[cfg(test)]
    pub(crate) fn adopt_file(&mut self, fd: OwnedFd) -> u64 {
        self.insert_file(fd)
    }
    #[cfg(test)]
    pub(crate) fn adopt_directory(&mut self, fd: OwnedFd) -> u64 {
        self.insert_directory(fd)
    }
    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    pub fn new(stdout: OwnedFd, stderr: OwnedFd) -> Self {
        Self::from_streams(Some(stdout), Some(stderr))
    }

    pub(crate) fn from_streams(stdout: Option<OwnedFd>, stderr: Option<OwnedFd>) -> Self {
        let epoch = monotonic_ns();
        if std::env::var("HIRSEL_UDP_MEASURE_CLOCK").as_deref() == Ok("1") {
            eprintln!("UDP_CLOCK_EPOCH {epoch}");
        }
        Self {
            issue_file: None,
            sync_file: None,
            epoch,
            tables: Mutex::default(),
            unix_roots: BTreeMap::new(),
            next: AtomicU64::new(1),
            temporary: AtomicU64::new(0),
            inherited_listener: Mutex::new(None),
            inherited_unix: Mutex::new(None),
            operator: Mutex::new(None),
            stdout,
            stderr,
        }
    }
    pub(crate) fn stderr_fd(&self) -> i32 {
        self.stderr.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    /// Only run configuration can acquire a root from an ambient host path.
    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    pub fn grant(&mut self, path: &Path) -> io::Result<u64> {
        let file = crate::units::off_slot(|| {
            let file = std::fs::File::open(path)?;
            if !file.metadata()?.is_dir() {
                return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
            }
            Ok(file)
        })?;
        Ok(self.insert_directory(file.into()))
    }
    /// Startup-only capability probe. The per-operation confined open is unchanged.
    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    pub(crate) fn probe_directory(&self, handle: u64) -> io::Result<()> {
        let directory = self.directory(handle);
        crate::units::off_slot(|| {
            drop(beneath(directory, b".", host::path::DIRECTORY_HANDLE)?);
            Ok(())
        })
    }
    pub fn provider(&mut self) -> Provider {
        self.provider_with(Some(&hirsel_core_os_directory_ops))
    }

    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    pub(crate) fn provider_with(&mut self, directory: Option<&DirectoryOperations>) -> Provider {
        self.issue_file = directory.map(|ops| ops.issue);
        self.sync_file = directory.map(|ops| ops.sync);
        Provider {
            state: (self as *mut Self).cast(),
            wait,
            issue,
            resume: directory.map_or(resume, |ops| ops.resume),
            abort: directory.map_or(abort, |ops| ops.abort),
            deliver,
        }
    }
    fn execute_file(&self, request: &mut Request) -> io::Result<Outcome> {
        let mut out = Outcome::default();
        // SAFETY: emitter/provider ABI pins these buffers and exact extents.
        let path = if request.path_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(request.path, request.path_len) }
        };
        if matches!(
            request.operation,
            OPEN | MKDIR | REMOVE | REMOVE_TREE | OPEN_DIR | STAT
        ) {
            valid_path(path)?;
        }
        match request.operation {
            OPEN => {
                let flags = match request.mode {
                    0 => libc::O_RDONLY,
                    1 => libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    2 => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    3 => libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    4 => libc::O_RDWR | libc::O_CREAT,
                    MODE_DURABLE | MODE_DIRECT => {
                        libc::O_WRONLY
                            | libc::O_CREAT
                            | libc::O_TRUNC
                            | crate::host::sync::DURABLE_WRITE
                    }
                    _ => unreachable!(),
                };
                let fd = beneath(self.directory(request.handle), path, flags)?;
                if request.mode == MODE_DIRECT {
                    metadata::enable_direct(fd.as_fd(), flags)?;
                }
                out.value = self.insert_file(fd);
            }
            OPEN_DIR => {
                let dir = open_directory(self.directory(request.handle), path)?;
                out.value = self.insert_directory(dir);
            }
            ENTRIES => {
                let root = self.directory(request.handle);
                // Open a fresh description so independently opened listings have distinct cursors.
                let path = if path.is_empty() {
                    b".".as_slice()
                } else {
                    valid_path(path)?;
                    path
                };
                let fd = open_directory(root, path)?;
                let listing = Listing::open(fd)?;
                let id = self.id();
                self.tables().listings.insert(id, listing);
                out.value = id;
            }
            NEXT_ENTRIES => {
                // Taken out while it reads, so the read holds no lock. The
                // issuing unit owns the listing, so nothing else looks it up.
                let mut listing = self
                    .tables()
                    .listings
                    .remove(&request.handle)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
                let read = listing.append(&mut request.buffer);
                self.tables().listings.insert(request.handle, listing);
                match read {
                    Ok(count) => out.value = count,
                    Err(error) => {
                        out.error = IoError {
                            tag: 5,
                            code: error.raw_os_error().unwrap_or(libc::EIO) as u32,
                        };
                    }
                }
            }
            CLOSE_ENTRIES => {
                let listing = self
                    .tables()
                    .listings
                    .remove(&request.handle)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
                drop(listing);
            }
            STAT => {
                let root = self.directory(request.handle);
                let fd = beneath(root, path, host::path::LEAF_HANDLE)?;
                let result = stat(fd.as_fd());
                let _ = close_file(fd);
                return result;
            }
            STAT_FILE => {
                // SAFETY: the issuing unit's open handle keeps the descriptor live.
                return stat(unsafe { BorrowedFd::borrow_raw(self.file(request.handle)) });
            }
            READ | READ_AT => {
                let fd = self.file(request.handle);
                let offset = (request.operation == READ_AT).then(|| request.offset());
                let b = &mut request.buffer;
                // A single read either commits a prefix or fails without moving the
                // cursor. No read_exact retry can discard an already consumed prefix.
                if b.capacity > b.len {
                    // SAFETY: spare capacity is writable; fd is held by this provider.
                    let n = unsafe {
                        let data = b.data.add(b.len).cast();
                        let spare = b.capacity - b.len;
                        match offset {
                            Some(at) if at > i64::MAX as u64 => 0,
                            Some(at) => libc::pread(fd, data, spare, at as i64),
                            None => libc::read(fd, data, spare),
                        }
                    };
                    let n = syscall(n)? as usize;
                    b.len += n;
                    out.value = n as u64;
                }
            }
            WRITE | WRITE_AT => {
                return Ok(write_request(self.file(request.handle), request));
            }
            CLOSE => {
                let fd = self.tables().files.remove(&request.handle).unwrap();
                close_file(fd)?;
            }
            TRY_LOCK | UNLOCK => {
                let fd = self.file(request.handle);
                let flags = if request.operation == TRY_LOCK {
                    libc::LOCK_EX | libc::LOCK_NB
                } else {
                    libc::LOCK_UN
                };
                // SAFETY: fd is live, flags designate an advisory operation.
                let rc = unsafe { libc::flock(fd, flags) };
                if rc < 0
                    && request.operation == TRY_LOCK
                    && io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
                {
                    out.value = 0;
                } else {
                    syscall(rc as isize)?;
                    out.value = 1;
                }
            }
            RENAME => {
                let raw = path;
                let Some(split) = raw.iter().position(|b| *b == 0) else {
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                };
                let from = &raw[..split];
                let to = &raw[split + 1..];
                valid_path(from)?;
                valid_path(to)?;
                let (from_parent, from_name) = parent(self.directory(request.handle), from)?;
                let (to_parent, to_name) = parent(self.directory(request.handle), to)?;
                // core.md: only a regular file is renamed, only a regular file is
                // replaced, and neither final component is followed. renameat alone
                // would move a directory or a link, or replace a link.
                match leaf_kind(&from_parent, &from_name)? {
                    None => return Err(io::Error::from_raw_os_error(libc::ENOENT)),
                    Some(kind) if kind != libc::S_IFREG => {
                        return Err(io::Error::from_raw_os_error(libc::EACCES));
                    }
                    Some(_) => {}
                }
                if leaf_kind(&to_parent, &to_name)?.is_some_and(|kind| kind != libc::S_IFREG) {
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                }
                // SAFETY: pinned parents and single NUL-terminated leaves.
                syscall(unsafe {
                    libc::renameat(
                        from_parent.as_raw_fd(),
                        from_name.as_ptr(),
                        to_parent.as_raw_fd(),
                        to_name.as_ptr(),
                    )
                } as isize)?;
            }
            MKDIR | REMOVE | REMOVE_TREE => {
                let (parent, name) = parent(self.directory(request.handle), path)?;
                match request.operation {
                    MKDIR => {
                        // SAFETY: pinned parent and single NUL-terminated leaf.
                        syscall(
                            unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) }
                                as isize,
                        )?;
                    }
                    REMOVE => remove(parent.as_raw_fd(), &name)?,
                    REMOVE_TREE => remove_tree(parent.as_raw_fd(), &name)?,
                    _ => unreachable!(),
                }
            }
            MKDTEMP | MKSTEMP => {
                let fd = self.directory(request.handle);
                let opened = temp::create(fd, request, &self.temporary)?;
                out.value = if request.operation == MKSTEMP {
                    self.insert_file(opened)
                } else {
                    self.insert_directory(opened)
                };
            }
            SYNC => {
                let fd = self.file(request.handle);
                // fdatasync makes file contents and the metadata required to
                // read them back durable. A refusal is delivered as a host
                // error; the store owns the poison/recovery policy.
                syscall(crate::host::sync::sync_data(fd) as isize)?;
            }
            ZERO_EXTENSION => {
                let fd = self.directory(request.handle);
                // SAFETY: directory descriptors stay open until the run ends.
                out.value = u64::from(zero_extension(unsafe { BorrowedFd::borrow_raw(fd) })?);
            }
            SYNC_DIR => {
                let fd = self.directory(request.handle);
                // fsync on a directory persists its entry updates.
                syscall(crate::host::sync::sync_directory(fd) as isize)?;
            }
            _ => unreachable!(),
        }
        Ok(out)
    }
}

/// The host directory's stable descriptor and its effective write permission.
///
/// # Safety
/// The running task owns context and grant is a live Dir. The caller holds
/// the provider commit token. The OS provider retains directory descriptions
/// until the run ends, after the foreign call has returned.
pub(crate) unsafe fn foreign_root(
    context: *const crate::Context,
    grant: u64,
) -> Option<(i32, bool)> {
    use crate::entry::{hirsel_core_capability_handle, hirsel_core_capability_provider};
    use hirsel_core_ids::host_op::DIR_READ_ONLY;

    let handle = grant & !DIR_READ_ONLY;
    // SAFETY: the typed Dir designates immutable entry or dynamic grant metadata.
    unsafe {
        let provider = hirsel_core_capability_provider(context, handle).as_ref()?;
        let os_issue: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step = issue;
        if !std::ptr::fn_addr_eq(provider.issue, os_issue)
            || !std::ptr::fn_addr_eq(provider.wait, WAIT)
            || !std::ptr::fn_addr_eq(provider.deliver, crate::provider::DELIVER)
        {
            return None;
        }
        let key = hirsel_core_capability_handle(context, handle);
        // Only host Dir grants carry these callbacks and an OsProvider state.
        let os = &*provider.state.cast::<OsProvider>();
        let directory = os.tables().directories.get(&key)?.as_raw_fd();
        Some((directory, grant & DIR_READ_ONLY == 0))
    }
}

/// Host CLOCK_MONOTONIC at the OS provider's construction: its mono_ns and its
/// sleep and network deadlines count from it.
/// # Safety
/// State is a live OsProvider.
#[inline(always)]
pub(crate) unsafe fn epoch(state: *mut u8) -> u64 {
    // SAFETY: forwarded; epoch is immutable after construction.
    unsafe { (*state.cast::<OsProvider>()).epoch }
}

/// Whether `provider` calls the OS provider's own `wait`, `issue` and `deliver`,
/// so a call form may run their bodies directly (CAP-5). A recorder that
/// replaces any of them is never the OS provider. Test builds can force the
/// pointer calls (`tasks::own_callbacks`).
#[inline(always)]
pub(crate) fn owns(provider: &Provider) -> bool {
    crate::tasks::own_callbacks(
        std::ptr::fn_addr_eq(provider.wait, WAIT)
            && std::ptr::fn_addr_eq(provider.issue, ISSUE)
            && std::ptr::fn_addr_eq(provider.deliver, crate::provider::DELIVER),
    )
}

/// Stack a clock read may use on the current stack. `clock_gettime` is a vDSO
/// leaf whose frame is a few hundred bytes; below this much room the read runs
/// on the host stack instead.
const CLOCK_ROOM: usize = 4096;

/// The OS provider's WALL_MS and MONO_NS: one vDSO read, on the caller's stack
/// when the guard leaves `CLOCK_ROOM`, never switching stacks otherwise.
/// # Safety
/// State is the run's live OsProvider; request is a pinned clock request.
#[inline(always)]
pub(crate) unsafe fn clock(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: forwarded.
    unsafe {
        if crate::fiber::room(CLOCK_ROOM)
            && let Some(outcome) = read_clock(state, (*request).operation)
        {
            return outcome;
        }
        crate::tasks::host_operation(state, request, clock_on_stack)
    }
}
/// A clock read that failed, or had no room, stops (`Clock`) on the host stack.
unsafe extern "C" fn clock_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: forwarded.
    unsafe { read_clock(state, (*request).operation) }
        .unwrap_or_else(|| crate::process::stop(b"Clock"))
}
#[inline(always)]
unsafe fn read_clock(state: *mut u8, operation: u8) -> Option<Outcome> {
    let value = if operation == WALL_MS {
        clock::wall_ms()?
    } else {
        // SAFETY: forwarded.
        clock::monotonic_ns()
            .ok()?
            .saturating_sub(unsafe { epoch(state) })
    };
    Some(Outcome {
        value,
        ..Outcome::default()
    })
}

pub(crate) fn monotonic_ns() -> u64 {
    clock::monotonic_ns().unwrap_or(u64::MAX)
}

/// `wait` as a provider callback, for recognising the OS provider's own.
pub(crate) const WAIT: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Wait = wait;
/// SLEEP_UNTIL waits until its deadline (program mono_ns, from `epoch`), given to
/// the scheduler as host CLOCK_MONOTONIC; it is Ready once the deadline has
/// passed. SLEEP_UNTIL_WALL waits until CLOCK_REALTIME reaches its millisecond
/// reading (`wall_wait`). Every other operation is Ready at once. Never inlined, so `WAIT`
/// has one address (`provider::deliver`).
/// # Safety
/// State is the run's live OsProvider; request meets the provider ABI contract.
#[inline(never)]
pub unsafe extern "C" fn wait(state: *mut u8, request: *mut Request, _: u64) -> Wait {
    // SAFETY: pinned request and provider state.
    let wait = unsafe {
        let request = &*request;
        if request.buffer.len < 8 {
            Wait::default()
        } else if request.operation == SLEEP_UNTIL {
            sleep_wait(state, request)
        } else if request.operation == SLEEP_UNTIL_WALL {
            wall_wait(request)
        } else {
            Wait::default()
        }
    };
    #[cfg(test)]
    probe::note(probe::WAIT, request, u64::from(wait.kind));
    wait
}
/// # Safety
/// As `wait`, for a SLEEP_UNTIL request with an eight-byte deadline.
#[cold]
#[inline(never)]
unsafe fn sleep_wait(state: *mut u8, request: &Request) -> Wait {
    // SAFETY: forwarded.
    unsafe {
        let deadline = request.buffer.data.cast::<u64>().read_unaligned();
        let at = epoch(state).saturating_add(deadline);
        if at <= monotonic_ns() {
            Wait::default()
        } else {
            Wait {
                kind: WAIT_UNTIL,
                deadline_ns: at,
            }
        }
    }
}
/// # Safety
/// As `wait`, for a SLEEP_UNTIL_WALL request with an eight-byte wall reading.
/// The remaining wall time becomes a host CLOCK_MONOTONIC deadline. The scheduler
/// calls `wait` again when that deadline passes, so a wall clock set back since
/// is waited out from a fresh pair of readings, and the wait is Ready only once
/// CLOCK_REALTIME has reached the reading. These reads are host scheduling and
/// never reach a tape.
#[cold]
#[inline(never)]
unsafe fn wall_wait(request: &Request) -> Wait {
    // SAFETY: the caller checked the eight-byte buffer.
    let at = unsafe { request.buffer.data.cast::<u64>().read_unaligned() };
    let Some(now) = clock::wall_ms() else {
        crate::process::stop(b"Clock")
    };
    if at <= now {
        return Wait::default();
    }
    let remaining = (at - now).saturating_mul(1_000_000);
    Wait {
        kind: WAIT_UNTIL,
        deadline_ns: monotonic_ns().saturating_add(remaining),
    }
}
/// `issue` as a provider callback, for recognising the OS provider's own.
pub(crate) const ISSUE: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step = issue;
/// A duplicate of the descriptor behind file `handle` of `provider`, when
/// `provider` is the OS provider: the recording writer appends through it
/// off the workers (`tape::writer`). None for any other provider.
/// # Safety
/// `provider` is installed in the calling run, and `handle` is a file it
/// opened and has not closed.
pub(crate) unsafe fn duplicate_file(provider: &Provider, handle: u64) -> Option<OwnedFd> {
    if provider.issue as usize != ISSUE as usize {
        return None;
    }
    // SAFETY: the OS provider's state is its OsProvider.
    let os = unsafe { &*provider.state.cast::<OsProvider>() };
    let fd = os.tables().files.get(&handle)?.as_raw_fd();
    // SAFETY: a live descriptor; the duplicate is owned by the result.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    // SAFETY: a fresh descriptor nothing else owns.
    (dup >= 0).then(|| unsafe { OwnedFd::from_raw_fd(dup) })
}
/// Clocks and files finish immediately, except SYNC and SYNC_DIR, which park while
/// the sync helper runs them; sockets may park after a not-ready attempt.
/// Never inlined, so `ISSUE` has one address (`provider::deliver`).
/// # Safety
/// As `wait`.
#[inline(never)]
pub unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: UDP adapters pin the request extension through delivery.
    if unsafe { udp_operation((*request).operation) } {
        return unsafe { (crate::udp_net::operations(request).step)(request, token) };
    }
    #[cfg(feature = "record")]
    // SAFETY: the tape pins the source's local socket registry in a typed CLOSE.
    unsafe {
        let r = &*request;
        if r.operation == CLOSE && r.mode == crate::udp_net::record::CLOSE_MODE {
            crate::udp_net::record::close_source(r);
            return Step::default();
        }
        if r.operation == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
            return Step {
                outcome: Outcome {
                    value: u64::from(crate::net::close_socket_source(r)),
                    ..Outcome::default()
                },
                ..Step::default()
            };
        }
    }
    // SAFETY: these operations pin the Exec call and its callback table.
    unsafe {
        match ((*request).operation, (*request).mode) {
            (EXEC_SPAWN, _) => {
                let spawn = crate::exec_runtime::operations(request).spawn;
                return Step {
                    outcome: crate::tasks::host_operation(state, request, spawn),
                    ..Step::default()
                };
            }
            (EXEC_WAIT | EXEC_SIGNAL, _) | (CLOSE, crate::exec_runtime::PROC_CLOSE) => {
                return (crate::exec_runtime::operations(request).process)(&mut *request, token);
            }
            _ => {}
        }
    }
    if unsafe { matches!((*request).operation, WALL_MS | MONO_NS) } {
        let outcome = unsafe { clock(state, request) };
        #[cfg(test)]
        probe::note(probe::ISSUE, request, probe::answer(&outcome));
        return Step {
            kind: STEP_DONE,
            outcome,
            deadline_ns: 0,
        };
    }
    if unsafe { crate::os_net::is_operation((*request).operation) } {
        // SAFETY: socket adapters pin the request, and state is this run's
        // OS provider. Only the first LISTEN can take its inherited fd.
        let step = unsafe {
            let operations = socket_operations(request);
            let inherited = if (*request).operation == LISTEN {
                (*state.cast::<OsProvider>())
                    .inherited_listener
                    .lock()
                    .unwrap()
                    .take()
            } else {
                None
            };
            match inherited {
                Some(fd) => (operations.adopt_listener)(request, fd),
                None => (operations.step)(request, token, true),
            }
        };
        #[cfg(test)]
        probe::note(probe::ISSUE, request, probe::answer(&step.outcome));
        return step;
    }
    if unsafe { unix_operation((*request).operation) } {
        let step =
            unsafe { (unix_operations(request).step)(&*state.cast(), &mut *request, token, true) };
        #[cfg(test)]
        probe::note(probe::ISSUE, request, probe::answer(&step.outcome));
        return step;
    }
    if unsafe { (*request).operation == STDIN_READ } {
        // SAFETY: STDIN_READ carries the adapter's live Stdin request.
        let step = unsafe { (crate::stdin::operations(request).step)(request, token, true) };
        #[cfg(test)]
        probe::note(probe::ISSUE, request, probe::answer(&step.outcome));
        return step;
    }
    if unsafe { matches!((*request).operation, SYNC | SYNC_DIR) }
        && let Some(sync) = unsafe { (*state.cast::<OsProvider>()).sync_file }
        // SAFETY: the directory provider owns the request's handle.
        && let Some(step) = unsafe { sync(state, request, token) }
    {
        return step;
    }
    let outcome = unsafe { crate::tasks::host_operation(state, request, issue_on_stack) };
    #[cfg(test)]
    probe::note(probe::ISSUE, request, probe::answer(&outcome));
    Step {
        kind: STEP_DONE,
        outcome,
        deadline_ns: 0,
    }
}
/// Test-only: when a test switches it on, the OS provider's `wait`, `issue` and
/// `deliver` note each call, so a test can compare how they were called.
#[cfg(test)]
pub(crate) mod probe {
    use super::{Outcome, Request};
    use std::cell::RefCell;
    pub(crate) const WAIT: u8 = 0;
    pub(crate) const ISSUE: u8 = 1;
    pub(crate) const DELIVER: u8 = 2;
    /// One callback: which, the request's operation and site, whether a task
    /// was current, and its answer (a wait's kind; an outcome's status, error
    /// and count written).
    pub(crate) type Call = (u8, u8, u64, bool, u64);
    thread_local! { pub(crate) static CALLS: RefCell<Option<Vec<Call>>> = const { RefCell::new(None) }; }
    pub(crate) fn answer(outcome: &Outcome) -> u64 {
        u64::from(outcome.status)
            | u64::from(outcome.error.tag) << 8
            | u64::from(outcome.error.code) << 16
            | outcome.written << 48
    }
    pub(crate) fn note(callback: u8, request: *const Request, answer: u64) {
        record(callback, request, answer, crate::tasks::in_task());
    }
    /// A note for a `wait` a direct call form runs in place of the callback,
    /// which the scheduler always calls with no task current.
    pub(crate) fn note_outside(callback: u8, request: *const Request, answer: u64) {
        record(callback, request, answer, false);
    }
    fn record(callback: u8, request: *const Request, answer: u64, in_task: bool) {
        CALLS.with_borrow_mut(|calls| {
            if let Some(calls) = calls {
                // SAFETY: the callback's pinned request.
                let (operation, site) = unsafe { ((*request).operation, (*request).site) };
                calls.push((callback, operation, site, in_task, answer));
            }
        });
    }
}

/// The Net adapters pin the private extension of each socket request.
#[inline]
unsafe fn socket_operations(request: *const Request) -> &'static crate::os_net::Operations {
    unsafe { (*(*request).path.cast::<crate::net::NetworkRequest>()).os }
}
#[inline]
unsafe fn unix_operations(request: *const Request) -> &'static crate::os_unix::Operations {
    unsafe { (*(*request).path.cast::<crate::unix_net::UnixRequest>()).os }
}

/// The OS provider without directory operations parks socket and Stdin requests.
unsafe extern "C" fn resume(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: an outstanding request pins its socket or stdin operands.
    unsafe {
        if (*request).operation == EXEC_WAIT {
            (crate::exec_runtime::operations(request).process)(&mut *request, token)
        } else if (*request).operation == STDIN_READ {
            (crate::stdin::operations(request).step)(request, token, false)
        } else if unix_operation((*request).operation) {
            (unix_operations(request).step)(&*state.cast(), &mut *request, token, false)
        } else if udp_operation((*request).operation) {
            (crate::udp_net::operations(request).step)(request, token)
        } else {
            (socket_operations(request).step)(request, token, false)
        }
    }
}
unsafe extern "C" fn abort(
    state: *mut u8,
    request: *mut Request,
    token: u64,
    reason: u8,
) -> Outcome {
    // SAFETY: an outstanding request pins its socket or stdin operands.
    unsafe {
        if (*request).operation == EXEC_WAIT {
            (crate::exec_runtime::operations(request).abort)(&mut *request, reason)
        } else if (*request).operation == STDIN_READ {
            (crate::stdin::operations(request).abort)(request, token, reason)
        } else if unix_operation((*request).operation) {
            (unix_operations(request).abort)(&*state.cast(), &mut *request, token, reason)
        } else if udp_operation((*request).operation) {
            (crate::udp_net::operations(request).abort)(request, token, reason)
        } else {
            (socket_operations(request).abort)(request, token, reason)
        }
    }
}
unsafe extern "C" fn resume_with_directory(
    state: *mut u8,
    request: *mut Request,
    token: u64,
) -> Step {
    // SAFETY: the scheduler pins the pending request through resume.
    if unsafe { matches!((*request).operation, SYNC | SYNC_DIR) } {
        let (step, consumed) = unsafe { crate::os_sync::resume(request) };
        if let Some(cell) = consumed {
            // SAFETY: state is this run's live OsProvider.
            let os = unsafe { &*state.cast::<OsProvider>() };
            let mut tables = os.tables();
            if let Some(i) = tables.in_flight_syncs.iter().position(|&c| c == cell) {
                tables.in_flight_syncs.swap_remove(i);
            }
        }
        return step;
    }
    // SAFETY: only sync or socket requests reach this callback.
    unsafe { resume(state, request, token) }
}
unsafe extern "C" fn abort_with_directory(
    state: *mut u8,
    request: *mut Request,
    token: u64,
    reason: u8,
) -> Outcome {
    // SAFETY: the scheduler pins the pending request through abort.
    if unsafe { matches!((*request).operation, SYNC | SYNC_DIR) } {
        return crate::os_sync::abort();
    }
    // SAFETY: only sync or socket requests reach this callback.
    unsafe { abort(state, request, token, reason) }
}
unsafe extern "C" fn issue_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: a provider operation exclusively owns the request during issue.
    match unsafe { (*request).operation } {
        // Waiting is owned by the scheduler; issue completes at once.
        SLEEP_UNTIL | SLEEP_UNTIL_WALL => return Outcome::default(),
        RANDOM => return random::issue(unsafe { &mut *request }),
        OPEN | OPEN_DIR | STAT | STAT_FILE | CLOSE | WRITE | WRITE_AT
            if crate::units::uses_file_ring() =>
        {
            // SAFETY: the request is pinned until the CQE completes.
            return unsafe { issue_now(state, request) };
        }
        _ => {}
    }
    struct Issue(*mut u8, *mut Request);
    // SAFETY: the unit stays parked, and its operation pinned, until the
    // helper returns.
    unsafe impl Send for Issue {}
    let issue = Issue(state, request);
    // Every other operation can wait without bound, so it runs off the slot.
    crate::units::off_slot(move || {
        let issue = issue;
        // SAFETY: forwarded caller contract.
        unsafe { issue_now(issue.0, issue.1) }
    })
}

/// # Safety
/// As `issue_on_stack`.
unsafe fn issue_now(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: a provider operation exclusively owns the adapter during issue.
    let os = unsafe { &*state.cast::<OsProvider>() };
    let request = unsafe { &mut *request };
    match request.operation {
        STDOUT => write_request(os.stdout.as_ref().map_or(-1, AsRawFd::as_raw_fd), request),
        STDERR => write_request(os.stderr_fd(), request),
        _ => {
            let issue_file = os.issue_file.unwrap();
            // SAFETY: the directory provider was installed before the run issued this request.
            unsafe { issue_file(state, request) }
        }
    }
}

// The token `submit_on_stack` queues for, and the step it answers. Moved:
// the submit can park the unit on the memory thread's reply.
crate::units::moved_tls! {
    static SUBMIT: (u64, Option<Step>) = (0, None);
}

/// SYNC and SYNC_DIR park while the helper syncs a duplicate of the handle's
/// descriptor (`os_sync`). Submission runs on the host stack, because the
/// first one starts the helper thread.
unsafe fn sync_file(state: *mut u8, request: *mut Request, token: u64) -> Option<Step> {
    // SAFETY: forwarded caller contract.
    let (os, operation, handle) = unsafe {
        let request = &*request;
        (
            &*state.cast::<OsProvider>(),
            request.operation,
            request.handle,
        )
    };
    let tables = os.tables();
    let live = if operation == SYNC_DIR {
        tables.directories.contains_key(&handle)
    } else {
        tables.files.contains_key(&handle)
    };
    drop(tables);
    if !live {
        return None;
    }
    SUBMIT.set((token, None));
    // SAFETY: as `issue`.
    unsafe { crate::tasks::host_operation(state, request, submit_on_stack) };
    Some(SUBMIT.take().1.unwrap_or_default())
}

unsafe extern "C" fn submit_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: forwarded caller contract; `sync_file` checked the handle.
    let (os, req) = unsafe { (&*state.cast::<OsProvider>(), &*request) };
    let directory = req.operation == SYNC_DIR;
    let fd = if directory {
        os.directory(req.handle)
    } else {
        os.file(req.handle)
    };
    let token = SUBMIT.get().0;
    // SAFETY: pinned SYNC/SYNC_DIR request.
    let (step, untracked) = unsafe { crate::os_sync::submit(fd, directory, token, request) };
    if let Some(cell) = untracked {
        os.tables().in_flight_syncs.push(cell);
    }
    SUBMIT.set((token, Some(step)));
    Outcome::default()
}

unsafe extern "C" fn issue_file(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: forwarded caller contract.
    request_outcome(unsafe { (*state.cast::<OsProvider>()).execute_file(&mut *request) })
}

fn request_outcome(result: io::Result<Outcome>) -> Outcome {
    result.unwrap_or_else(|error| Outcome {
        error: IoError::from_errno(error.raw_os_error().unwrap_or(libc::EIO)),
        ..Outcome::default()
    })
}

fn syscall(rc: isize) -> io::Result<isize> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// # Safety
/// Live descriptor and borrowed diagnostic bytes; no user cancellation applies.
pub(crate) unsafe fn diagnostic(fd: i32, data: *const u8, len: usize) {
    // SAFETY: run diagnostics pin their byte view for this synchronous write.
    unsafe {
        let bytes = if len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(data, len)
        };
        let _ = write_all(fd, bytes);
    }
}

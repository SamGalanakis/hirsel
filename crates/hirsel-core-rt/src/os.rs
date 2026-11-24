//! Linux descriptor-relative host operations. No path is authorized by a prior
//! canonicalization. openat2 resolves every traversal beneath its granted fd.
use crate::provider::*;

mod listing;
use hirsel_core_net::sys;
use listing::Listing;
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString},
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    path::Path,
    sync::Mutex,
};
pub(crate) const TEMP_ATTEMPTS: usize = 128;

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
    directories: BTreeMap<u64, OwnedFd>,
    pub(crate) unix_roots: BTreeMap<String, OwnedFd>,
    files: BTreeMap<u64, OwnedFd>,
    listings: BTreeMap<u64, Listing>,
    next: u64,
    temporary: u64,
    in_flight_syncs: Vec<*mut crate::os_sync::SyncCell>,
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
        for cell in self.in_flight_syncs.drain(..) {
            // SAFETY: provider is ending with a sync still in flight; detach
            // its issuer handle so the helper frees the cell on completion.
            unsafe { crate::os_sync::SyncCell::detach(cell) };
        }
    }
}
impl OsProvider {
    #[cfg(test)]
    pub(crate) fn file_descriptor(&self, handle: u64) -> i32 {
        self.files[&handle].as_raw_fd()
    }
    #[cfg(test)]
    pub(crate) fn adopt_file(&mut self, fd: OwnedFd) -> u64 {
        let id = self.id();
        self.files.insert(id, fd);
        id
    }
    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    pub fn new(stdout: OwnedFd, stderr: OwnedFd) -> Self {
        Self::from_streams(Some(stdout), Some(stderr))
    }

    pub(crate) fn from_streams(stdout: Option<OwnedFd>, stderr: Option<OwnedFd>) -> Self {
        Self {
            issue_file: None,
            sync_file: None,
            epoch: monotonic_ns(),
            directories: BTreeMap::new(),
            unix_roots: BTreeMap::new(),
            files: BTreeMap::new(),
            listings: BTreeMap::new(),
            next: 1,
            temporary: 0,
            in_flight_syncs: Vec::new(),
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
        let _blocking = crate::units::blocking();
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let id = self.id();
        self.directories.insert(id, file.into());
        Ok(id)
    }
    /// Startup-only capability probe. The per-operation confined open is unchanged.
    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    pub(crate) fn probe_directory(&self, handle: u64) -> io::Result<()> {
        let _blocking = crate::units::blocking();
        drop(beneath(
            self.directories[&handle].as_raw_fd(),
            b".",
            hirsel_core_net::sys::O_PATH | libc::O_DIRECTORY,
        )?);
        Ok(())
    }
    fn id(&mut self) -> u64 {
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        id
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
    fn execute_file(&mut self, request: &mut Request) -> io::Result<Outcome> {
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
                    _ => unreachable!(),
                };
                let fd = beneath(self.directories[&request.handle].as_raw_fd(), path, flags)?;
                let id = self.id();
                self.files.insert(id, fd);
                out.value = id;
            }
            OPEN_DIR => {
                let root = self.directories[&request.handle].as_raw_fd();
                let flags = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY;
                let dir = beneath(root, path, flags).map_err(|error| {
                    if error.raw_os_error() == Some(libc::ENOTDIR) {
                        io::Error::from_raw_os_error(libc::EACCES)
                    } else {
                        error
                    }
                })?;
                let id = self.id();
                self.directories.insert(id, dir);
                out.value = id;
            }
            ENTRIES => {
                let root = self.directories[&request.handle].as_raw_fd();
                // Open a fresh description so independently opened listings have distinct cursors.
                let fd = beneath(root, b".", libc::O_RDONLY | libc::O_DIRECTORY)?;
                let listing = Listing::open(fd)?;
                let id = self.id();
                self.listings.insert(id, listing);
                out.value = id;
            }
            NEXT_ENTRIES => {
                let listing = self
                    .listings
                    .get_mut(&request.handle)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
                match listing.append(&mut request.buffer) {
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
                    .listings
                    .remove(&request.handle)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
                drop(listing);
            }
            STAT => {
                let root = self.directories[&request.handle].as_raw_fd();
                let fd = beneath(root, path, LINK_ITSELF)?;
                return stat(fd.as_raw_fd());
            }
            STAT_FILE => return stat(self.files[&request.handle].as_raw_fd()),
            READ | READ_AT => {
                let fd = self.files[&request.handle].as_raw_fd();
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
                return Ok(write_request(
                    self.files[&request.handle].as_raw_fd(),
                    request,
                ));
            }
            CLOSE => {
                let fd = self.files.remove(&request.handle).unwrap().into_raw_fd();
                // SAFETY: consumes the sole owned fd. Linux closes even on EINTR;
                // retrying could close another operation's newly reused descriptor.
                syscall(unsafe { libc::close(fd) } as isize)?;
            }
            TRY_LOCK | UNLOCK => {
                let fd = self.files[&request.handle].as_raw_fd();
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
                let (from_parent, from_name) =
                    parent(self.directories[&request.handle].as_raw_fd(), from)?;
                let (to_parent, to_name) =
                    parent(self.directories[&request.handle].as_raw_fd(), to)?;
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
                let (parent, name) = parent(self.directories[&request.handle].as_raw_fd(), path)?;
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
            MKDTEMP => {
                let fd = self.directories[&request.handle].as_raw_fd();
                // Only the completed basename must be valid: dot prefixes are
                // ordinary names once the nonempty alphanumeric suffix is added.
                let prefix = if request.buffer.len == 0 {
                    &[]
                } else {
                    // SAFETY: issue borrows the carrier's existing initialized bytes.
                    unsafe { std::slice::from_raw_parts(request.buffer.data, request.buffer.len) }
                };
                if prefix.contains(&b'/') || prefix.contains(&0) {
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                }
                for _ in 0..TEMP_ATTEMPTS {
                    let sequence = self.temporary;
                    self.temporary = sequence
                        .checked_add(1)
                        .unwrap_or_else(|| crate::process::stop(b"HostNames"));
                    let suffix = format!("h{:x}x{:x}", std::process::id(), sequence);
                    let mut name = prefix.to_vec();
                    name.extend_from_slice(suffix.as_bytes());
                    let name_c = CString::new(name)?;
                    // SAFETY: new single-component name in the granted directory.
                    let result = unsafe { libc::mkdirat(fd, name_c.as_ptr(), 0o700) };
                    if result < 0 {
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() == Some(libc::EEXIST) {
                            continue;
                        }
                        return Err(error);
                    }
                    let opened = beneath(
                        fd,
                        name_c.as_bytes(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    );
                    let directory = match opened {
                        Ok(directory) => directory,
                        Err(error) => {
                            // SAFETY: remove the empty directory just created, no link traversal.
                            unsafe {
                                libc::unlinkat(fd, name_c.as_ptr(), libc::AT_REMOVEDIR);
                            }
                            return Err(error);
                        }
                    };
                    // SAFETY: entry checked 32 spare bytes before issue; suffix
                    // is at most 27 ASCII bytes. Prefix bytes are never rewritten.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            suffix.as_ptr(),
                            request.buffer.data.add(request.buffer.len),
                            suffix.len(),
                        );
                    }
                    request.buffer.len += suffix.len();
                    let id = self.id();
                    self.directories.insert(id, directory);
                    out.value = id;
                    return Ok(out);
                }
                // Host namespace contention is recoverable. All attempts were
                // collisions, so no directory or carrier bytes were changed.
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            SYNC => {
                let fd = self.files[&request.handle].as_raw_fd();
                // fdatasync makes file contents and the metadata required to
                // read them back durable. A refusal is delivered as a host
                // error; the store owns the poison/recovery policy.
                syscall(unsafe { sys::fdatasync(fd) } as isize)?;
            }
            SYNC_DIR => {
                let fd = self.directories[&request.handle].as_raw_fd();
                // fsync on a directory persists its entry updates.
                syscall(unsafe { libc::fsync(fd) } as isize)?;
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
        let directory = os.directories.get(&key)?;
        Some((directory.as_raw_fd(), grant & DIR_READ_ONLY == 0))
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
    let (clock, wall) = if operation == WALL_MS {
        (libc::CLOCK_REALTIME, true)
    } else {
        (hirsel_core_net::sys::CLOCK_MONOTONIC, false)
    };
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec for a host clock.
    if unsafe { libc::clock_gettime(clock, &mut now) } != 0 {
        return None;
    }
    let value = if wall {
        u64::try_from(i128::from(now.tv_sec) * 1000 + i128::from(now.tv_nsec) / 1_000_000).ok()?
    } else {
        let ns = u64::try_from(now.tv_sec)
            .ok()?
            .checked_mul(1_000_000_000)?
            .checked_add(u64::try_from(now.tv_nsec).ok()?)?;
        // SAFETY: forwarded.
        ns.saturating_sub(unsafe { epoch(state) })
    };
    Some(Outcome {
        value,
        ..Outcome::default()
    })
}

fn monotonic_clock_ns() -> io::Result<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec for the host monotonic clock.
    if unsafe { libc::clock_gettime(hirsel_core_net::sys::CLOCK_MONOTONIC, &mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds =
        u64::try_from(now.tv_sec).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    let nanos =
        u64::try_from(now.tv_nsec).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanos))
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))
}
pub(crate) fn monotonic_ns() -> u64 {
    monotonic_clock_ns().unwrap_or(u64::MAX)
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
    let Some(now) = wall_clock_ms() else {
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
fn wall_clock_ms() -> Option<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec for the host wall clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) } != 0 {
        return None;
    }
    u64::try_from(i128::from(now.tv_sec) * 1000 + i128::from(now.tv_nsec) / 1_000_000).ok()
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
    let fd = os.files.get(&handle)?.as_raw_fd();
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
    #[cfg(feature = "record")]
    // SAFETY: the tape pins the source's local socket registry in a typed CLOSE.
    unsafe {
        let r = &*request;
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
    if unsafe {
        matches!(
            (*request).operation,
            CONNECT
                | LISTEN
                | ACCEPT
                | RECV_BOUNDED
                | RECV_READY
                | SEND_PAIR
                | TLS_CONNECT
                | SEND_SECRET
                | SCRAM
                | RESOLVE
                | CRED_FETCH
        )
    } {
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
            let os = unsafe { &mut *state.cast::<OsProvider>() };
            if let Some(i) = os.in_flight_syncs.iter().position(|&c| c == cell) {
                os.in_flight_syncs.swap_remove(i);
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
    // SAFETY: a provider operation exclusively owns the adapter during issue.
    let os = unsafe { &mut *state.cast::<OsProvider>() };
    let request = unsafe { &mut *request };
    match request.operation {
        STDOUT => write_request(os.stdout.as_ref().map_or(-1, AsRawFd::as_raw_fd), request),
        STDERR => write_request(os.stderr_fd(), request),
        // Waiting is owned by the scheduler; issue completes at once.
        SLEEP_UNTIL | SLEEP_UNTIL_WALL => Outcome::default(),
        RANDOM => {
            let _blocking = crate::units::blocking();
            request_outcome(random(request))
        }
        _ => {
            let issue_file = os.issue_file.unwrap();
            // SAFETY: the directory provider was installed before the run issued this request.
            unsafe { issue_file(state, request) }
        }
    }
}

thread_local! {
    /// The token `submit_on_stack` queues for, and the step it answers.
    static SUBMIT: std::cell::Cell<(u64, Step)> = std::cell::Cell::new((0, Step::default()));
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
    if operation == SYNC_DIR {
        os.directories.get(&handle)
    } else {
        os.files.get(&handle)
    }?;
    SUBMIT.set((token, Step::default()));
    // SAFETY: as `issue`; submit never suspends.
    unsafe { crate::tasks::host_operation(state, request, submit_on_stack) };
    Some(SUBMIT.get().1)
}

unsafe extern "C" fn submit_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: forwarded caller contract; `sync_file` checked the handle.
    let (os, req) = unsafe { (&mut *state.cast::<OsProvider>(), &*request) };
    let directory = req.operation == SYNC_DIR;
    let fd = if directory {
        &os.directories[&req.handle]
    } else {
        &os.files[&req.handle]
    }
    .as_raw_fd();
    let token = SUBMIT.get().0;
    // SAFETY: pinned SYNC/SYNC_DIR request.
    let (step, untracked) = unsafe { crate::os_sync::submit(fd, directory, token, request) };
    if let Some(cell) = untracked {
        os.in_flight_syncs.push(cell);
    }
    SUBMIT.set((token, step));
    Outcome::default()
}

unsafe extern "C" fn issue_file(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: forwarded caller contract.
    let operation = unsafe { (*request).operation };
    // Writes are marked by write_all_from, which also serves standard streams.
    let _blocking = (!matches!(operation, WRITE | WRITE_AT)).then(crate::units::blocking_file);
    request_outcome(unsafe { (&mut *state.cast::<OsProvider>()).execute_file(&mut *request) })
}

fn request_outcome(result: io::Result<Outcome>) -> Outcome {
    result.unwrap_or_else(|error| Outcome {
        error: IoError::from_errno(error.raw_os_error().unwrap_or(libc::EIO)),
        ..Outcome::default()
    })
}

fn write_request(fd: i32, request: &Request) -> Outcome {
    let bytes = if request.buffer.len == 0 {
        &[]
    } else {
        // SAFETY: the caller pins the readable input bytes through delivery.
        unsafe { std::slice::from_raw_parts(request.buffer.data, request.buffer.len) }
    };
    let offset = (request.operation == WRITE_AT).then(|| request.offset());
    write_all_from(fd, bytes, offset)
}

fn random(request: &mut Request) -> io::Result<Outcome> {
    let mut filled = 0usize;
    while filled < request.buffer.capacity.saturating_sub(request.buffer.len) {
        let spare = request.buffer.capacity - request.buffer.len - filled;
        // SAFETY: the carrier owns writable spare capacity and the
        // syscall is bounded to that exact extent.
        let n = unsafe {
            sys::getrandom(
                request.buffer.data.add(request.buffer.len + filled).cast(),
                spare,
                0,
            )
        };
        let n = syscall(n as isize)? as usize;
        if n == 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        filled += n;
    }
    request.buffer.len += filled;
    Ok(Outcome {
        written: filled as u64,
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
pub(crate) fn write_all(fd: i32, bytes: &[u8]) -> Outcome {
    write_all_from(fd, bytes, None)
}

fn write_all_from(fd: i32, bytes: &[u8], offset: Option<u64>) -> Outcome {
    let _blocking = crate::units::blocking_file();
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: live descriptor and remaining readable bytes.
        let n = unsafe {
            let data = bytes[written..].as_ptr().cast();
            let len = bytes.len() - written;
            match offset {
                Some(at) => {
                    let Some(at) = at
                        .checked_add(written as u64)
                        .and_then(|n| i64::try_from(n).ok())
                    else {
                        return Outcome {
                            written: written as u64,
                            error: IoError::from_errno(libc::EFBIG),
                            ..Outcome::default()
                        };
                    };
                    libc::pwrite(fd, data, len, at)
                }
                None => libc::write(fd, data, len),
            }
        };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Outcome {
                written: written as u64,
                error: IoError::from_errno(error.raw_os_error().unwrap_or(libc::EIO)),
                ..Outcome::default()
            };
        }
        if n == 0 {
            return Outcome {
                written: written as u64,
                error: IoError::from_errno(libc::EIO),
                ..Outcome::default()
            };
        }
        written += n as usize;
    }
    Outcome {
        written: written as u64,
        ..Outcome::default()
    }
}

// Validate user paths once, before any operation can touch the host. Internal
// descriptor walks may still use "." to duplicate their already granted parent.
fn valid_path(path: &[u8]) -> io::Result<()> {
    if !hirsel_core_net::valid_relative_path(path) {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(())
}
/// Open flags for a final symlink itself rather than its target: macOS has
/// no O_PATH, and opens the link with O_SYMLINK.
#[cfg(target_os = "linux")]
const LINK_ITSELF: i32 = libc::O_PATH | libc::O_NOFOLLOW;
#[cfg(target_os = "macos")]
const LINK_ITSELF: i32 = libc::O_SYMLINK;

fn beneath(fd: i32, path: &[u8], flags: i32) -> io::Result<OwnedFd> {
    open_beneath(fd, path, flags, sys::RESOLVE_NO_MAGICLINKS)
}

pub(crate) fn open_beneath(fd: i32, path: &[u8], flags: i32, resolve: u64) -> io::Result<OwnedFd> {
    let mut short = [0; 64];
    let long;
    let path: &CStr = if path.len() < short.len() {
        short[..path.len()].copy_from_slice(path);
        CStr::from_bytes_with_nul(&short[..=path.len()]).map_err(|_| io::ErrorKind::InvalidInput)?
    } else {
        long = CString::new(path)?;
        &long
    };
    let mode = if flags & libc::O_CREAT != 0 { 0o600 } else { 0 };
    // SAFETY: openat2 (or the host's equivalent open flags) atomically
    // enforces confinement, including symlinks. Unsupported hosts return
    // Other(ENOSYS); no unconstrained fallback.
    let raw = unsafe {
        sys::openat2(
            fd,
            path.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
            sys::RESOLVE_BENEATH | resolve,
        )
    };
    syscall(raw as isize)?;
    // SAFETY: successful openat2 transfers a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
}
fn parent(fd: i32, path: &[u8]) -> io::Result<(OwnedFd, CString)> {
    let split = path.iter().rposition(|&c| c == b'/');
    let (prefix, name) = match split {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => (b".".as_slice(), path),
    };
    Ok((
        beneath(fd, prefix, hirsel_core_net::sys::O_PATH | libc::O_DIRECTORY)?,
        CString::new(name)?,
    ))
}
/// The file type (`S_IFMT` bits) of `name` in `parent`, not following a final link;
/// `None` when nothing has that name.
fn leaf_kind(parent: &OwnedFd, name: &CString) -> io::Result<Option<libc::mode_t>> {
    Ok(leaf_stat(parent.as_raw_fd(), name)?.map(|st| st.st_mode & libc::S_IFMT))
}

fn leaf_stat(parent: i32, name: &CStr) -> io::Result<Option<libc::stat>> {
    // SAFETY: the parent descriptor and basename are live. The final link is not followed.
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    match syscall(rc as isize) {
        // SAFETY: a successful fstatat initialized the out parameter.
        Ok(_) => Ok(Some(unsafe { st.assume_init() })),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

fn stat(fd: i32) -> io::Result<Outcome> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the caller retains fd and fstat writes the supplied stat.
    syscall(unsafe { libc::fstat(fd, st.as_mut_ptr()) } as isize)?;
    let st = unsafe { st.assume_init() };
    let overflow = |_| io::Error::from_raw_os_error(libc::EOVERFLOW);
    let size = u64::try_from(st.st_size).map_err(overflow)?;
    let nanos = i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec);
    let nanos = i64::try_from(nanos).map_err(overflow)?;
    Ok(Outcome {
        value: size,
        written: nanos as u64,
        kind: file_kind(st.st_mode),
        ..Outcome::default()
    })
}

fn file_kind(mode: libc::mode_t) -> u8 {
    match mode & libc::S_IFMT {
        libc::S_IFREG => 0,
        libc::S_IFDIR => 1,
        libc::S_IFLNK => 2,
        _ => 3,
    }
}
fn remove(fd: i32, name: &CString) -> io::Result<()> {
    // SAFETY: unlinkat never follows the final symlink; directory removal is retried explicitly.
    let rc = unsafe { libc::unlinkat(fd, name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // macOS refuses to unlink a directory with EPERM rather than EISDIR.
    let directory = match error.raw_os_error() {
        Some(libc::EISDIR) => true,
        Some(libc::EPERM) if cfg!(target_os = "macos") => {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: the pinned parent, fixed leaf and result slot are live.
            unsafe {
                libc::fstatat(
                    fd,
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                ) == 0
                    && stat.assume_init().st_mode & libc::S_IFMT == libc::S_IFDIR
            }
        }
        _ => false,
    };
    if !directory {
        return Err(error);
    }
    // SAFETY: same pinned parent and fixed leaf, directory-only removal.
    syscall(unsafe { libc::unlinkat(fd, name.as_ptr(), libc::AT_REMOVEDIR) } as isize)?;
    Ok(())
}
/// The names in the directory `directory` holds open, without `.` and `..`.
#[cfg(target_os = "linux")]
fn entry_names(directory: &OwnedFd) -> io::Result<Vec<CString>> {
    use std::os::unix::ffi::OsStrExt;
    std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?
        .map(|entry| Ok(CString::new(entry?.file_name().as_bytes())?))
        .collect()
}

/// The names in the directory `directory` holds open, without `.` and `..`:
/// read through a duplicate, since closedir closes the descriptor it reads.
#[cfg(not(target_os = "linux"))]
fn entry_names(directory: &OwnedFd) -> io::Result<Vec<CString>> {
    let copy = directory.try_clone()?;
    // SAFETY: fdopendir takes ownership of the duplicate; closedir releases both.
    let stream = unsafe { libc::fdopendir(copy.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(copy);
    let mut names = Vec::new();
    // SAFETY: the stream is live until closedir; each entry's name is
    // NUL-terminated and copied before the next readdir.
    unsafe {
        libc::rewinddir(stream);
        loop {
            let entry = libc::readdir(stream);
            if entry.is_null() {
                break;
            }
            let name = CStr::from_ptr((*entry).d_name.as_ptr());
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                names.push(name.to_owned());
            }
        }
        libc::closedir(stream);
    }
    Ok(names)
}

fn remove_tree(fd: i32, name: &CString) -> io::Result<()> {
    // Descriptor-based walk; never follow a leaf link. std's read_dir owns the
    // /proc fd traversal, while every child open is relative to its pinned parent.
    let root = beneath(
        fd,
        name.as_bytes(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )?;
    let mut work = vec![(root, None::<(OwnedFd, CString)>)];
    while let Some((directory, finish)) = work.pop() {
        if let Some((parent, leaf)) = finish {
            // SAFETY: children have been removed; no final link is followed.
            syscall(
                unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), libc::AT_REMOVEDIR) }
                    as isize,
            )?;
            continue;
        }
        let mut children = Vec::new();
        for child in entry_names(&directory)? {
            let opened = beneath(
                directory.as_raw_fd(),
                child.as_bytes(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            );
            match opened {
                Ok(opened) => children.push((opened, directory.try_clone()?, child)),
                Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                    remove(directory.as_raw_fd(), &child)?
                }
                Err(error) => return Err(error),
            }
        }
        for (child, parent, leaf) in children {
            work.push((child.try_clone()?, Some((parent, leaf))));
            work.push((child, None));
        }
    }
    // SAFETY: remove only the named root, without following a replacement link.
    syscall(unsafe { libc::unlinkat(fd, name.as_ptr(), libc::AT_REMOVEDIR) } as isize)?;
    Ok(())
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

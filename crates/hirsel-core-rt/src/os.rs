//! Linux descriptor-relative host operations. No path is authorized by a prior
//! canonicalization. openat2 resolves every traversal beneath its granted fd.
use crate::provider::*;
use std::{
    collections::BTreeMap,
    ffi::CString,
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    path::Path,
    time::Instant,
};
pub(crate) const TEMP_ATTEMPTS: usize = 128;

pub struct OsProvider {
    directories: BTreeMap<u64, OwnedFd>,
    files: BTreeMap<u64, OwnedFd>,
    next: u64,
    clock: Instant,
    last_mono: u64,
    temporary: u64,
    pub stdout: OwnedFd,
    pub stderr: OwnedFd,
}
impl OsProvider {
    #[cfg(test)]
    pub(crate) fn file_descriptor(&self, handle: u64) -> i32 {
        self.files[&handle].as_raw_fd()
    }
    pub fn new(stdout: OwnedFd, stderr: OwnedFd) -> Self {
        Self {
            directories: BTreeMap::new(),
            files: BTreeMap::new(),
            next: 1,
            clock: Instant::now(),
            last_mono: 0,
            temporary: 0,
            stdout,
            stderr,
        }
    }
    /// Only run configuration can acquire a root from an ambient host path.
    pub fn grant(&mut self, path: &Path) -> io::Result<u64> {
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let id = self.id();
        self.directories.insert(id, file.into());
        Ok(id)
    }
    /// Startup-only capability probe. The per-operation confined open is unchanged.
    pub(crate) fn probe_directory(&self, handle: u64) -> io::Result<()> {
        drop(beneath(
            self.directories[&handle].as_raw_fd(),
            b".",
            libc::O_PATH | libc::O_DIRECTORY,
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
        Provider {
            state: (self as *mut Self).cast(),
            issue,
            deliver,
        }
    }
    fn execute(&mut self, request: &mut Request) -> io::Result<Outcome> {
        let mut out = Outcome::default();
        // SAFETY: emitter/provider ABI pins these buffers and exact extents.
        let path = if request.path_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(request.path, request.path_len) }
        };
        if matches!(request.operation, OPEN | MKDIR | REMOVE | REMOVE_TREE) {
            valid_path(path)?;
        }
        match request.operation {
            OPEN => {
                let flags = match request.mode {
                    0 => libc::O_RDONLY,
                    1 => libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    2 => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    3 => libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    _ => unreachable!(),
                };
                let fd = beneath(self.directories[&request.handle].as_raw_fd(), path, flags)?;
                let id = self.id();
                self.files.insert(id, fd);
                out.value = id;
            }
            READ => {
                let fd = self.files[&request.handle].as_raw_fd();
                let b = &mut request.buffer;
                // A single read either commits a prefix or fails without moving the
                // cursor. No read_exact retry can discard an already consumed prefix.
                if b.capacity > b.len {
                    // SAFETY: spare capacity is writable; fd is held by this provider.
                    let n = unsafe { libc::read(fd, b.data.add(b.len).cast(), b.capacity - b.len) };
                    let n = syscall(n)? as usize;
                    b.len += n;
                    out.value = n as u64;
                }
            }
            WRITE | STDOUT | STDERR => {
                let fd = match request.operation {
                    STDOUT => self.stdout.as_raw_fd(),
                    STDERR => self.stderr.as_raw_fd(),
                    _ => self.files[&request.handle].as_raw_fd(),
                };
                // SAFETY: the caller pins the readable input bytes through delivery.
                let bytes = if request.buffer.len == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(request.buffer.data, request.buffer.len) }
                };
                return Ok(write_all(fd, bytes));
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
            WALL_MS => {
                let mut now = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                // SAFETY: valid writable timespec for the host wall clock.
                if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) } != 0 {
                    crate::process::stop(b"Clock");
                }
                out.value = u64::try_from(
                    i128::from(now.tv_sec) * 1000 + i128::from(now.tv_nsec) / 1_000_000,
                )
                .unwrap_or_else(|_| crate::process::stop(b"Clock"));
            }
            SLEEP_UNTIL => {
                let deadline = if request.buffer.len >= 8 {
                    unsafe { *(request.buffer.data.cast::<u64>()) }
                } else {
                    0
                };
                self.last_mono = self.last_mono.max(deadline);
            }
            MONO_NS => {
                let now = u64::try_from(self.clock.elapsed().as_nanos())
                    .unwrap_or_else(|_| crate::process::stop(b"Clock"));
                self.last_mono = now.max(self.last_mono);
                out.value = self.last_mono;
            }
            _ => unreachable!(),
        }
        Ok(out)
    }
}

/// # Safety
/// State is the run's live OsProvider; request meets the provider ABI contract.
pub unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: OS adapter issues synchronously, without task callbacks or suspension.
    unsafe { crate::tasks::host_operation(state, request, issue_on_stack) }
}
unsafe extern "C" fn issue_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: a provider operation exclusively owns the adapter during issue.
    match unsafe { (&mut *state.cast::<OsProvider>()).execute(&mut *request) } {
        Ok(out) => out,
        Err(error) => Outcome {
            error: IoError::from_errno(error.raw_os_error().unwrap_or(libc::EIO)),
            ..Outcome::default()
        },
    }
}
fn syscall(rc: isize) -> io::Result<isize> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}
fn write_all(fd: i32, bytes: &[u8]) -> Outcome {
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: live descriptor and remaining readable bytes.
        let n = unsafe { libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written) };
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
    if path.contains(&0)
        || path
            .split(|&byte| byte == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(())
}
fn beneath(fd: i32, path: &[u8], flags: i32) -> io::Result<OwnedFd> {
    let path = CString::new(path)?;
    // SAFETY: open_how is an integer-only Linux UAPI structure.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (flags | libc::O_CLOEXEC) as u64;
    how.mode = if flags & libc::O_CREAT != 0 { 0o600 } else { 0 };
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;
    // SAFETY: Linux openat2 atomically enforces confinement, including symlinks.
    // Unsupported hosts return Other(ENOSYS); no unconstrained fallback.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            fd,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
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
        beneath(fd, prefix, libc::O_PATH | libc::O_DIRECTORY)?,
        CString::new(name)?,
    ))
}
fn remove(fd: i32, name: &CString) -> io::Result<()> {
    // SAFETY: unlinkat never follows the final symlink; directory removal is retried explicitly.
    let rc = unsafe { libc::unlinkat(fd, name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EISDIR) {
        return Err(error);
    }
    // SAFETY: same pinned parent and fixed leaf, directory-only removal.
    syscall(unsafe { libc::unlinkat(fd, name.as_ptr(), libc::AT_REMOVEDIR) } as isize)?;
    Ok(())
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
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
            use std::os::unix::ffi::OsStrExt;
            let entry = entry?;
            let child = CString::new(entry.file_name().as_bytes())?;
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

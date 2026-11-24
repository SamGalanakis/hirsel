//! Confined path resolution and descriptor ownership for the OS provider.
use super::syscall;
use hirsel_core_net::sys;
use std::{
    ffi::{CStr, CString},
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
};

// Validate user paths once, before any operation can touch the host. Internal
// descriptor walks may still use "." to duplicate their already granted parent.
pub(super) fn valid_path(path: &[u8]) -> io::Result<()> {
    if !hirsel_core_net::valid_relative_path(path) {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(())
}
pub(super) fn open_directory(fd: i32, path: &[u8]) -> io::Result<OwnedFd> {
    let flags = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY;
    beneath(fd, path, flags).map_err(|error| match error.raw_os_error() {
        Some(libc::ENOTDIR) => io::Error::from_raw_os_error(libc::EACCES),
        _ => error,
    })
}

pub(super) fn beneath(fd: i32, path: &[u8], flags: i32) -> io::Result<OwnedFd> {
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
    #[cfg(target_os = "linux")]
    if crate::units::uses_file_ring() {
        // SAFETY: open_how is an integer-only Linux UAPI structure.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (flags | libc::O_CLOEXEC) as u64;
        how.mode = mode.into();
        how.resolve = libc::RESOLVE_BENEATH | resolve;
        let entry = io_uring::opcode::OpenAt2::new(
            io_uring::types::Fd(fd),
            path.as_ptr(),
            (&raw const how).cast(),
        )
        .build();
        // SAFETY: the provider holds fd, and path and how are live through the CQE.
        let raw = unsafe { crate::units::file_request(entry) }?;
        // SAFETY: OPENAT2 succeeded and transfers a fresh descriptor.
        return Ok(unsafe { OwnedFd::from_raw_fd(raw) });
    }
    // SAFETY: openat2 (or the host's equivalent open flags) atomically enforces
    // confinement, including symlinks. Unsupported hosts return Other(ENOSYS);
    // no unconstrained fallback.
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

pub(super) fn close_file(fd: OwnedFd) -> io::Result<()> {
    let fd = fd.into_raw_fd();
    #[cfg(target_os = "linux")]
    if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::Close::new(io_uring::types::Fd(fd)).build();
        // SAFETY: the entry consumes the sole owned descriptor.
        unsafe { crate::units::file_request(entry) }?;
        return Ok(());
    }
    // SAFETY: consumes the sole owned fd. Linux closes even on EINTR;
    // retrying could close a newly reused descriptor.
    syscall(unsafe { libc::close(fd) } as isize)?;
    Ok(())
}
pub(super) fn parent(fd: i32, path: &[u8]) -> io::Result<(OwnedFd, CString)> {
    let split = path.iter().rposition(|&c| c == b'/');
    let (prefix, name) = match split {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => (b".".as_slice(), path),
    };
    Ok((
        beneath(fd, prefix, sys::O_PATH | libc::O_DIRECTORY)?,
        CString::new(name)?,
    ))
}
/// The file type (`S_IFMT` bits) of `name` in `parent`, not following a final link;
/// `None` when nothing has that name.
pub(super) fn leaf_kind(parent: &OwnedFd, name: &CString) -> io::Result<Option<libc::mode_t>> {
    Ok(leaf_stat(parent.as_raw_fd(), name)?.map(|st| st.st_mode & libc::S_IFMT))
}

pub(super) fn leaf_stat(parent: i32, name: &CStr) -> io::Result<Option<libc::stat>> {
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

pub(super) fn remove(fd: i32, name: &CString) -> io::Result<()> {
    // SAFETY: unlinkat never follows the final symlink; directory removal is retried explicitly.
    let rc = unsafe { libc::unlinkat(fd, name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // macOS refuses unlinkat on a directory with EPERM rather than EISDIR.
    let directory = match error.raw_os_error() {
        Some(libc::EISDIR) => true,
        Some(libc::EPERM) if cfg!(target_os = "macos") => {
            leaf_stat(fd, name)?.is_some_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR)
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

pub(super) fn remove_tree(fd: i32, name: &CString) -> io::Result<()> {
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

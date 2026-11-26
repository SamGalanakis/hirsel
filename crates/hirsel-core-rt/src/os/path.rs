//! Confined path resolution and descriptor ownership for the OS provider.
use super::syscall;
use crate::host::path::{self as host, Refuse};
use std::{
    ffi::{CStr, CString},
    io,
    os::fd::{AsRawFd, IntoRawFd, OwnedFd},
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
    open_beneath(fd, path, flags, Refuse::MagicLinks)
}

pub(crate) fn open_beneath(
    fd: i32,
    path: &[u8],
    flags: i32,
    refuse: Refuse,
) -> io::Result<OwnedFd> {
    let mut short = [0; 64];
    let long;
    let path: &CStr = if path.len() < short.len() {
        short[..path.len()].copy_from_slice(path);
        CStr::from_bytes_with_nul(&short[..=path.len()]).map_err(|_| io::ErrorKind::InvalidInput)?
    } else {
        long = CString::new(path)?;
        &long
    };
    host::open_beneath(fd, path, &host::how(flags, refuse))
}

pub(super) fn close_file(fd: OwnedFd) -> io::Result<()> {
    crate::host::file::close(fd.into_raw_fd())
}
pub(super) fn parent(fd: i32, path: &[u8]) -> io::Result<(OwnedFd, CString)> {
    let split = path.iter().rposition(|&c| c == b'/');
    let (prefix, name) = match split {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => (b".".as_slice(), path),
    };
    Ok((
        beneath(fd, prefix, host::DIRECTORY_HANDLE)?,
        CString::new(name)?,
    ))
}
/// The file type (`S_IFMT` bits) of `name` in `parent`, not following a final link;
/// `None` when nothing has that name.
pub(super) fn leaf_kind(parent: &OwnedFd, name: &CString) -> io::Result<Option<libc::mode_t>> {
    host::leaf_mode(parent.as_raw_fd(), name)
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
    // unlinkat never follows the final symlink. It refuses a directory with
    // EISDIR on Linux and with EPERM on Darwin, and the removal is then retried
    // as a directory's. A name that is no directory keeps the first error.
    match host::unlink_at(fd, name, 0) {
        Err(error) if matches!(error.raw_os_error(), Some(libc::EISDIR | libc::EPERM)) => {
            match host::unlink_at(fd, name, libc::AT_REMOVEDIR) {
                Err(second) if second.raw_os_error() == Some(libc::ENOTDIR) => Err(error),
                done => done,
            }
        }
        done => done,
    }
}
/// The names in `directory`, without `.` and `..`.
fn names(directory: &OwnedFd) -> io::Result<Vec<CString>> {
    // The stream owns a duplicate, which shares the directory's read position;
    // nothing else has read this descriptor.
    let duplicate = directory.try_clone()?;
    // SAFETY: a live directory descriptor, which the stream owns on success.
    let stream = unsafe { libc::fdopendir(duplicate.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    let _ = duplicate.into_raw_fd();
    let mut names = Vec::new();
    let listed = loop {
        // SAFETY: this function owns the live stream. readdir reports the end
        // and an error with null, and errno tells them apart.
        let entry = unsafe {
            *crate::host::sys::errno() = 0;
            libc::readdir(stream).as_ref()
        };
        let Some(entry) = entry else {
            let error = io::Error::last_os_error();
            break match error.raw_os_error() {
                Some(0) => Ok(names),
                _ => Err(error),
            };
        };
        // SAFETY: readdir returns a NUL-terminated name, valid until the next call.
        let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(name.to_owned());
        }
    };
    // SAFETY: the stream is live and closed once; this also closes its descriptor.
    unsafe { libc::closedir(stream) };
    listed
}
pub(super) fn remove_tree(fd: i32, name: &CString) -> io::Result<()> {
    // Descriptor-based walk; never follow a leaf link. Each directory is listed
    // through its own descriptor, and every child open is relative to its pinned
    // parent.
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
        for child in names(&directory)? {
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

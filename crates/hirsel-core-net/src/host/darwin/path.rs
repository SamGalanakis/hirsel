//! Darwin confined opens. The kernel enforces O_RESOLVE_BENEATH.
use std::{
    ffi::CStr,
    io,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
};

/// Resolve every component beneath the starting directory (xnu `fcntl.h`).
const O_RESOLVE_BENEATH: i32 = 0x1000;
/// A directory handle that grants lookups but no reads.
#[cfg(target_os = "macos")]
pub(crate) const DIRECTORY: i32 = libc::O_SEARCH | libc::O_DIRECTORY;

pub(crate) fn open_beneath(root: BorrowedFd<'_>, path: &CStr, flags: i32) -> io::Result<OwnedFd> {
    let flags = flags | libc::O_CLOEXEC | O_RESOLVE_BENEATH | libc::O_NOFOLLOW_ANY;
    // SAFETY: the C path is live for the call; a success transfers a descriptor.
    let fd = unsafe { libc::openat(root.as_raw_fd(), path.as_ptr(), flags, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(libc::ENOTCAPABLE) => io::Error::from_raw_os_error(libc::EXDEV),
            _ => error,
        });
    }
    // SAFETY: openat returned an owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

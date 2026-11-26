//! Confined opens and Unix socket naming on Linux: `openat2` resolves beneath
//! a directory without symlinks, a bind runs on a thread with its own fs
//! state, and a pinned endpoint is reached through `/proc/self/fd`.
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        linux::fs::MetadataExt as _,
        unix::net::UnixStream,
    },
};

/// A directory handle that grants lookups but no reads.
pub(crate) const DIRECTORY: i32 = libc::O_PATH | libc::O_DIRECTORY;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

pub(crate) fn open_beneath(root: BorrowedFd<'_>, path: &CStr, flags: i32) -> io::Result<OwnedFd> {
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x08 | 0x04, // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS
    };
    // SAFETY: open_how and the C path are pinned for the resolved open.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of_val(&how),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel returned an owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

/// Give this thread its own working directory, `dir`.
pub(crate) fn isolate_binding_thread(dir: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: the probe and the bind helper use only this thread's copied fs state.
    unsafe {
        if libc::unshare(libc::CLONE_FS) < 0 || libc::fchdir(dir.as_raw_fd()) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Bind `socket` at the empty slot `leaf` of `dir`, from a thread made for the
/// call: its working directory becomes `dir`. Linux's bind follows no link at
/// the final component and refuses any node there with EADDRINUSE.
pub(crate) fn bind_in(socket: BorrowedFd<'_>, dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<()> {
    isolate_binding_thread(dir)?;
    crate::unix::bind_named(socket, leaf)
}

/// Connect to the socket named `leaf` in `dir`, pinning the endpoint first so a
/// renamed or replaced entry cannot redirect the connection.
pub(crate) fn connect_pinned(dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<UnixStream> {
    let endpoint = open_beneath(dir, leaf, libc::O_PATH | libc::O_NOFOLLOW)?;
    if File::from(endpoint.try_clone()?).metadata()?.st_mode() & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::other("the endpoint is not a socket"));
    }
    UnixStream::connect(format!("/proc/self/fd/{}", endpoint.as_raw_fd()))
}

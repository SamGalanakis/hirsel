//! File operations on Darwin: plain syscalls; there is no file ring.
use std::{io, os::fd::RawFd};

fn checked(rc: isize) -> io::Result<isize> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// Write a prefix of `bytes` at `offset` (the file position when `None`).
/// No unit owns a file ring on Darwin, so `ring` is always false.
pub(crate) fn write_part(
    fd: RawFd,
    bytes: &[u8],
    offset: Option<i64>,
    _ring: bool,
    _align: u64,
) -> io::Result<usize> {
    // SAFETY: fd and bytes remain live through the syscall.
    let result = unsafe {
        let data = bytes.as_ptr().cast();
        match offset {
            Some(at) => libc::pwrite(fd, data, bytes.len(), at),
            None => libc::write(fd, data, bytes.len()),
        }
    };
    checked(result).map(|result| result as usize)
}

/// Close the sole owner of `fd`. Darwin's `close` releases the descriptor
/// even when interrupted; retrying could close a reused one.
pub(crate) fn close(fd: RawFd) -> io::Result<()> {
    // SAFETY: consumes the sole owned fd.
    checked(unsafe { libc::close(fd) } as isize)?;
    Ok(())
}

/// Read a prefix of `len` bytes into `data` at `offset` (the file position
/// when `None`). There is no file ring on Darwin, so `ring` is always false.
///
/// # Safety
/// `data` is writable for `len` bytes, and the caller holds `fd` and `data`
/// until the call returns.
pub(crate) unsafe fn read_part(
    fd: RawFd,
    data: *mut u8,
    len: usize,
    offset: Option<u64>,
    _ring: bool,
) -> io::Result<isize> {
    // SAFETY: by the contract; the syscall completes before return.
    checked(unsafe {
        match offset {
            Some(at) => libc::pread(fd, data.cast(), len, at as i64),
            None => libc::read(fd, data.cast(), len),
        }
    })
}

/// One sync of `fd`, never retried: `F_FULLFSYNC` for files and directories
/// alike (`host::sync`).
pub(crate) fn sync(fd: RawFd, directory: bool, _ring: bool) -> io::Result<()> {
    let rc = if directory {
        super::sync::sync_directory(fd)
    } else {
        super::sync::sync_data(fd)
    };
    checked(rc as isize).map(drop)
}

/// Write the vector `iov` at `at`, returning the bytes written.
pub(crate) fn zero_part(fd: RawFd, iov: &[libc::iovec], at: i64, _ring: bool) -> io::Result<usize> {
    // SAFETY: fd and the initialized vector are live throughout pwritev.
    checked(unsafe { libc::pwritev(fd, iov.as_ptr(), iov.len() as i32, at) }).map(|n| n as usize)
}

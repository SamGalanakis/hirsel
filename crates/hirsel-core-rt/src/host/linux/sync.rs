//! Durable writes on Linux: `fdatasync` for files, `fsync` for directories,
//! `O_DSYNC` for writes durable on return and `O_DIRECT` for direct segments.
use std::os::fd::RawFd;

/// Open flag for a file whose every write is durable when it returns.
pub(crate) const DURABLE_WRITE: i32 = libc::O_DSYNC;

/// Make a file's contents, and the metadata needed to read them back,
/// durable. One call, never retried; a failure leaves its errno on this
/// thread for the caller to read before anything else runs.
#[inline(always)]
pub(crate) fn sync_data(fd: RawFd) -> i32 {
    // SAFETY: the caller holds the descriptor open across the call.
    unsafe { libc::fdatasync(fd) }
}

/// Make a directory's entry updates durable, with the same contract as
/// [`sync_data`].
#[inline(always)]
pub(crate) fn sync_directory(fd: RawFd) -> i32 {
    // SAFETY: the caller holds the descriptor open across the call.
    unsafe { libc::fsync(fd) }
}

/// Switch an open file with status `flags` to direct I/O, bypassing the page
/// cache. Returns the host's result with its errno, as [`sync_data`] does.
#[inline(always)]
pub(crate) fn set_direct(fd: RawFd, flags: i32) -> i32 {
    // SAFETY: the caller owns the live open file description.
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_DIRECT) }
}

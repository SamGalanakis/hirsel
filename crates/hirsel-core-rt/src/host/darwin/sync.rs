//! Durable writes on Darwin (STORE-SPEC §3.3c, macOS decision D5): every
//! durability point is `fcntl(F_FULLFSYNC)`, and nothing weaker. Plain `fsync`
//! returns before the drive has flushed its cache, `O_DSYNC` is not documented
//! to flush it, and `F_BARRIERFSYNC` only orders writes. The Darwin libSystem
//! allowlist leaves out `fsync` and `fdatasync`, so a path that reaches either
//! does not link.
use crate::provider::{IoError, MODE_DIRECT, MODE_DURABLE, Outcome};
use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd, RawFd},
};

/// No open flag makes a Darwin write durable on return; [`finish_write`]
/// flushes instead.
pub(crate) const DURABLE_WRITE: i32 = 0;

/// Flush the file's data and metadata through the drive's cache to media.
/// One call, never retried and never replaced by a weaker sync: a file
/// system that refuses `F_FULLFSYNC` answers its errno to the program.
#[inline(always)]
fn full_sync(fd: RawFd) -> i32 {
    // SAFETY: the caller holds the descriptor open across the call.
    unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) }
}

/// Make a file's contents, and the metadata needed to read them back,
/// durable, with the Linux provider's contract: a failure leaves its errno on
/// this thread for the caller to read before anything else runs.
#[inline(always)]
pub(crate) fn sync_data(fd: RawFd) -> i32 {
    full_sync(fd)
}

/// Make a directory's entry updates durable: `F_FULLFSYNC` on the directory
/// descriptor. That it flushes a rename is a requirement with a Mac law
/// (STORE-SPEC §3.3c).
#[inline(always)]
pub(crate) fn sync_directory(fd: RawFd) -> i32 {
    full_sync(fd)
}

/// Finish a write on a handle opened in `mode`. A `Durable` or `Direct` write
/// completes only after its bytes are written and an `F_FULLFSYNC` of the
/// handle returns. The flush covers every byte the write reports, including
/// the prefix before a failed write. If the flush fails, no byte of the write
/// is known durable: it reports nothing written and the flush's error.
pub(crate) fn finish_write(fd: RawFd, mode: u8, out: &mut Outcome) {
    if !matches!(mode, MODE_DURABLE | MODE_DIRECT) || out.written == 0 {
        return;
    }
    if full_sync(fd) < 0 {
        let errno = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        out.written = 0;
        out.error = IoError::from_errno(errno);
    }
}

/// Darwin has no `O_DIRECT`: a `Direct` handle bypasses the cache with
/// `F_NOCACHE` and keeps `Durable`'s completion rule. The provider checks the
/// alignment contract itself, as on every host. A file system that refuses
/// the hint keeps a buffered handle, which has the same contract.
pub(crate) fn enable_direct(fd: BorrowedFd<'_>, _flags: i32) -> io::Result<()> {
    // SAFETY: fd is the exclusive live open file description from open.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_NOCACHE, 1) } < 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) {
            return Err(error);
        }
    }
    Ok(())
}

/// Darwin's extended ranges are not trusted to read back zeros after a crash
/// until a Mac measurement shows APFS does, so a preallocated window writes
/// its zeros (STORE-SPEC §3.3c).
pub(crate) fn zero_extension(_fd: BorrowedFd<'_>) -> io::Result<bool> {
    Ok(false)
}

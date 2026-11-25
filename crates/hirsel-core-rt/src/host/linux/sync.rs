//! Durable writes on Linux: `fdatasync` for files, `fsync` for directories,
//! `O_DSYNC` for writes durable on return and `O_DIRECT` for direct segments
//! (STORE-SPEC §3.3b).
use super::stat::statx_fd;
use crate::{
    os::metadata::file_stat,
    provider::{DIRECT_ALIGNMENT, Outcome},
};
use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd, RawFd},
};

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

/// Finish a write on a handle opened in `mode`. `O_DSYNC` already made each
/// accepted byte durable when its write returned, so nothing is left to do.
#[inline(always)]
pub(crate) fn finish_write(_fd: RawFd, _mode: u8, _out: &mut Outcome) {}

/// Switch a `Direct` handle with open status `flags` to `O_DIRECT` when the
/// file system's direct-I/O alignment divides the Store's. Otherwise it stays
/// a buffered `O_DSYNC` handle, which keeps the same completion contract.
pub(crate) fn enable_direct(fd: BorrowedFd<'_>, flags: i32) -> io::Result<()> {
    let Ok(st) = statx_fd(fd, libc::STATX_DIOALIGN) else {
        return Ok(());
    };
    let usable = |n: u32| n != 0 && DIRECT_ALIGNMENT.is_multiple_of(u64::from(n));
    if st.stx_mask & libc::STATX_DIOALIGN != 0
        && usable(st.stx_dio_mem_align)
        && usable(st.stx_dio_offset_align)
    {
        // SAFETY: fd is the exclusive live open file description from open.
        let rc = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_DIRECT) };
        if rc < 0 {
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::EOPNOTSUPP)) {
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Whether extending the file system under directory `fd` reads back zeros
/// after a crash: tmpfs, and ext4 with ordered or journalled data.
pub(crate) fn zero_extension(fd: BorrowedFd<'_>) -> io::Result<bool> {
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the directory owns fd, and fstatfs initializes the output on success.
    if unsafe { libc::fstatfs(fd.as_raw_fd(), fs.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs succeeded.
    match unsafe { fs.assume_init() }.f_type {
        0x0102_1994 => return Ok(true), // tmpfs
        0xEF53 => (),                   // ext4
        _ => return Ok(false),
    }
    let dev = file_stat(fd)?.st_dev;
    let major = libc::major(dev);
    let minor = libc::minor(dev);
    let Ok(partitions) = std::fs::read_to_string("/proc/partitions") else {
        return Ok(false);
    };
    let name = partitions.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let row_major = fields.next()?.parse::<libc::c_uint>().ok()?;
        let row_minor = fields.next()?.parse::<libc::c_uint>().ok()?;
        fields.next()?; // block count
        let name = fields.next()?;
        ((row_major, row_minor) == (major, minor)).then_some(name)
    });
    Ok(name
        .and_then(|name| std::fs::read_to_string(format!("/proc/fs/ext4/{name}/options")).ok())
        .is_some_and(|options| {
            options
                .lines()
                .any(|line| matches!(line, "data=ordered" | "data=journal"))
        }))
}

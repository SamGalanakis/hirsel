//! File metadata every host reads the same way: `fstat`, and the Outcome a
//! STAT delivers. A host provider's `stat` (`host::stat`) may read it through
//! its own mechanism instead.
use super::syscall;
use crate::provider::Outcome;
use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd},
};

pub(crate) fn file_stat(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the caller retains fd and fstat writes the supplied stat.
    syscall(unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) } as isize)?;
    // SAFETY: fstat succeeded.
    Ok(unsafe { st.assume_init() })
}

/// STAT's Outcome from `fstat`.
pub(crate) fn fstat(fd: BorrowedFd<'_>) -> io::Result<Outcome> {
    let st = file_stat(fd)?;
    let size =
        u64::try_from(st.st_size).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    stat_outcome(
        size,
        i128::from(st.st_mtime),
        i128::from(st.st_mtime_nsec),
        st.st_mode,
    )
}

pub(crate) fn stat_outcome(
    size: u64,
    sec: i128,
    nsec: i128,
    mode: libc::mode_t,
) -> io::Result<Outcome> {
    let nanos = i64::try_from(sec * 1_000_000_000 + nsec)
        .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    Ok(Outcome {
        value: size,
        written: nanos as u64,
        kind: file_kind(mode),
        ..Outcome::default()
    })
}

pub(super) fn file_kind(mode: libc::mode_t) -> u8 {
    match mode & libc::S_IFMT {
        libc::S_IFREG => 0,
        libc::S_IFDIR => 1,
        libc::S_IFLNK => 2,
        _ => 3,
    }
}

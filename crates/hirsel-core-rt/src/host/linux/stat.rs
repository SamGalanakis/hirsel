//! STAT on Linux: `statx`, through the file ring when the pool uses one, else
//! `fstat`.
use crate::{
    os::metadata::{fstat, stat_outcome},
    provider::Outcome,
};
use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd},
};

pub(crate) fn stat(fd: BorrowedFd<'_>) -> io::Result<Outcome> {
    if crate::units::uses_file_ring() {
        let st = statx_fd(fd, libc::STATX_TYPE | libc::STATX_SIZE | libc::STATX_MTIME)?;
        return stat_outcome(
            st.stx_size,
            i128::from(st.stx_mtime.tv_sec),
            i128::from(st.stx_mtime.tv_nsec),
            u32::from(st.stx_mode),
        );
    }
    fstat(fd)
}

pub(super) fn statx_fd(fd: BorrowedFd<'_>, mask: u32) -> io::Result<libc::statx> {
    let mut st = std::mem::MaybeUninit::<libc::statx>::zeroed();
    if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::Statx::new(
            io_uring::types::Fd(fd.as_raw_fd()),
            c"".as_ptr(),
            st.as_mut_ptr().cast(),
        )
        .flags(libc::AT_EMPTY_PATH)
        .mask(mask)
        .build();
        // SAFETY: the caller owns fd and statx storage lives through the CQE.
        unsafe { crate::units::file_request(entry) }?;
    } else {
        // SAFETY: statx reads the empty name and initializes the pinned output.
        let rc = unsafe {
            libc::statx(
                fd.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                mask,
                st.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: statx successfully initialized the output.
    Ok(unsafe { st.assume_init() })
}

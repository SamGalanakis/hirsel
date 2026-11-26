//! Reaching a pinned AF_UNIX endpoint on Linux: the address names the pinned
//! `O_PATH` descriptor through `/proc/self/fd`, so the kernel connects to
//! exactly the inode the descriptor holds.
use super::path::{self, LEAF_HANDLE, Refuse};
use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, OwnedFd},
};

/// A socket endpoint resolved beneath a granted directory.
pub(crate) struct Endpoint {
    target: OwnedFd,
}

impl Endpoint {
    /// Pin the socket `name` in `parent`: an `O_PATH` handle on the leaf
    /// itself, never on a link's target. A leaf that is not a socket is EACCES.
    pub(crate) fn pin(parent: OwnedFd, name: &str) -> io::Result<Self> {
        let leaf = CString::new(name)?;
        let how = path::how(LEAF_HANDLE, Refuse::Symlinks);
        let endpoint = Self {
            target: path::open_beneath(parent.as_raw_fd(), &leaf, &how)?,
        };
        if endpoint.status()?.st_mode & libc::S_IFMT != libc::S_IFSOCK {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(endpoint)
    }

    fn status(&self) -> io::Result<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        loop {
            // SAFETY: a live leaf descriptor and its result slot.
            if unsafe { libc::fstat(self.target.as_raw_fd(), st.as_mut_ptr()) } == 0 {
                // SAFETY: fstat succeeded.
                return Ok(unsafe { st.assume_init() });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    /// The socket's device and inode, as `std::fs::Metadata` reports them.
    pub(crate) fn identity(&self) -> io::Result<(u64, u64)> {
        let st = self.status()?;
        Ok((st.st_dev, st.st_ino))
    }
}

/// Run `call` with a path that reaches the endpoint from this thread.
pub(crate) fn reach<R>(
    endpoint: &Endpoint,
    call: impl FnOnce(&[u8]) -> Result<R, i32>,
) -> Result<R, i32> {
    call(format!("/proc/self/fd/{}", endpoint.target.as_raw_fd()).as_bytes())
}

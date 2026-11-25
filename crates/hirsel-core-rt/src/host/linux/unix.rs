//! Reaching a pinned AF_UNIX endpoint on Linux: the address names the pinned
//! `O_PATH` descriptor through `/proc/self/fd`, so the kernel connects to
//! exactly the inode the descriptor holds.
use std::os::fd::{AsRawFd, OwnedFd};

/// A socket endpoint resolved beneath a granted directory.
pub(crate) struct Endpoint {
    target: OwnedFd,
}

impl Endpoint {
    /// `target` is a leaf handle on the socket `name` in `parent`.
    pub(crate) fn new(_parent: OwnedFd, _name: &str, target: OwnedFd) -> Self {
        Self { target }
    }

    /// The socket's device and inode, as `std::fs::Metadata` reports them.
    pub(crate) fn identity(&self) -> std::io::Result<(u64, u64)> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: a live leaf descriptor and its result slot.
        if unsafe { libc::fstat(self.target.as_raw_fd(), st.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fstat succeeded.
        let st = unsafe { st.assume_init() };
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

//! Reaching a pinned AF_UNIX endpoint on Darwin. There is no path that names
//! a descriptor and a socket file cannot be opened (EOPNOTSUPP), so the pin is
//! the leaf's device and inode. The call runs with this thread's working
//! directory set to a private directory (`pthread_fchdir_np`) and names a
//! private hard link to the pinned socket (`hirsel_core_net::unix::pinned::freeze`).
use std::{
    ffi::CString,
    io,
    os::fd::{AsFd, AsRawFd, OwnedFd},
};

use hirsel_core_net::unix::pinned::{Frozen, freeze, socket_identity};

unsafe extern "C" {
    /// Set this thread's working directory; -1 returns to the process's.
    fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
}

/// A socket endpoint resolved beneath a granted directory.
pub(crate) struct Endpoint {
    parent: OwnedFd,
    name: CString,
    identity: (u64, u64),
}

impl Endpoint {
    /// Pin the socket `name` in `parent` by its device and inode. A leaf that
    /// is not a socket, a link included, is EACCES.
    pub(crate) fn pin(parent: OwnedFd, name: &str) -> io::Result<Self> {
        let name = CString::new(name)?;
        let identity = socket_identity(parent.as_fd(), &name)?;
        Ok(Self {
            parent,
            name,
            identity,
        })
    }

    /// The socket's device and inode, as `std::fs::Metadata` reports them.
    pub(crate) fn identity(&self) -> io::Result<(u64, u64)> {
        Ok(self.identity)
    }
}

/// Run `call` with a path that reaches the endpoint from this thread: the
/// pinned socket frozen under a private name nobody can change
/// (`hirsel_core_net::unix::pinned::freeze`), so no rename in the parent can redirect
/// the call. A leaf that no longer holds the pinned socket is EACCES.
pub(crate) fn reach<R>(
    endpoint: &Endpoint,
    call: impl FnOnce(&[u8]) -> Result<R, i32>,
) -> Result<R, i32> {
    let frozen = freeze(endpoint.parent.as_fd(), &endpoint.name, endpoint.identity)
        .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
    // SAFETY: a live directory descriptor; only this thread's cwd changes.
    if unsafe { pthread_fchdir_np(frozen.dir().as_raw_fd()) } < 0 {
        // SAFETY: this thread's errno.
        return Err(unsafe { *libc::__error() });
    }
    let result = call(Frozen::NAME.to_bytes());
    // SAFETY: returns this thread to the process working directory.
    if unsafe { pthread_fchdir_np(-1) } < 0 {
        crate::process::stop(b"UnixThreadDirectory");
    }
    result
}

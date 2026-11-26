//! Reaching a pinned AF_UNIX endpoint on Darwin. There is no path that names
//! a descriptor and a socket file cannot be opened (EOPNOTSUPP), so the pin is
//! the leaf's device and inode. The call runs with this thread's working
//! directory set to the pinned parent (`pthread_fchdir_np`) and names the
//! leaf; the leaf must be the pinned socket before and after the call.
use std::{
    ffi::{CStr, CString},
    io,
    os::fd::{AsRawFd, OwnedFd},
};

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

/// The device and inode of the socket `name` in `parent`, not following a
/// link. Any other node there is EACCES.
fn socket_at(parent: &OwnedFd, name: &CStr) -> io::Result<(u64, u64)> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a live directory, a C name and the result slot.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat succeeded.
    let st = unsafe { st.assume_init() };
    if st.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok((st.st_dev as u64, st.st_ino))
}

impl Endpoint {
    /// Pin the socket `name` in `parent` by its device and inode. A leaf that
    /// is not a socket, a link included, is EACCES.
    pub(crate) fn pin(parent: OwnedFd, name: &str) -> io::Result<Self> {
        let name = CString::new(name)?;
        let identity = socket_at(&parent, &name)?;
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

    /// Whether the name still holds the pinned socket.
    fn holds(&self) -> Result<bool, i32> {
        match socket_at(&self.parent, &self.name) {
            Ok(identity) => Ok(identity == self.identity),
            Err(error) => Err(error.raw_os_error().unwrap_or(libc::EIO)),
        }
    }
}

/// Run `call` with a path that reaches the endpoint from this thread.
pub(crate) fn reach<R>(
    endpoint: &Endpoint,
    call: impl FnOnce(&[u8]) -> Result<R, i32>,
) -> Result<R, i32> {
    if !endpoint.holds()? {
        return Err(libc::EACCES);
    }
    // SAFETY: a live directory descriptor; only this thread's cwd changes.
    if unsafe { pthread_fchdir_np(endpoint.parent.as_raw_fd()) } < 0 {
        // SAFETY: this thread's errno.
        return Err(unsafe { *libc::__error() });
    }
    let result = call(endpoint.name.as_bytes());
    // SAFETY: returns this thread to the process working directory.
    if unsafe { pthread_fchdir_np(-1) } < 0 {
        crate::process::stop(b"UnixThreadDirectory");
    }
    let result = result?;
    if !endpoint.holds()? {
        return Err(libc::EACCES);
    }
    Ok(result)
}

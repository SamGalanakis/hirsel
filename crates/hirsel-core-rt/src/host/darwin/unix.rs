//! Reaching a pinned AF_UNIX endpoint on Darwin. There is no path that names
//! a descriptor, so the call runs with this thread's working directory set to
//! the pinned parent (`pthread_fchdir_np`) and names the leaf; the leaf must
//! be the pinned socket's device and inode before and after the call.
use std::{
    ffi::CString,
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
    target: OwnedFd,
}

impl Endpoint {
    /// `target` is a leaf handle on the socket `name` in `parent`.
    pub(crate) fn new(parent: OwnedFd, name: &str, target: OwnedFd) -> Self {
        Self {
            parent,
            // The name came from a C string open, so it has no NUL.
            name: CString::new(name).unwrap_or_default(),
            target,
        }
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
        Ok((st.st_dev as u64, st.st_ino))
    }

    fn holds(&self) -> Result<bool, i32> {
        let stat = |fd, name: *const libc::c_char, flags| {
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: live descriptor, C name and output slot.
            let rc = unsafe {
                if name.is_null() {
                    libc::fstat(fd, st.as_mut_ptr())
                } else {
                    libc::fstatat(fd, name, st.as_mut_ptr(), flags)
                }
            };
            if rc < 0 {
                // SAFETY: this thread's errno.
                return Err(unsafe { *libc::__error() });
            }
            // SAFETY: the call succeeded.
            let st = unsafe { st.assume_init() };
            Ok((st.st_dev, st.st_ino))
        };
        Ok(stat(self.target.as_raw_fd(), std::ptr::null(), 0)?
            == stat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )?)
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

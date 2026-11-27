//! Probe the same descriptor-relative operations required by NET-12 and NET-13.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::host::native_unix as host;
use std::ffi::CStr;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs::File;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::{fd::AsRawFd, unix::net::UnixListener};

/// A pathname address leaves one byte for the terminator.
pub const MAX_PATH_BYTES: usize =
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

pub use crate::host::pinned;

pub fn open_beneath(
    root: BorrowedFd<'_>,
    path: &std::ffi::CStr,
    flags: i32,
) -> io::Result<OwnedFd> {
    super::open_beneath(root, path, flags)
}

/// Run a socket operation relative to an open directory. Only the temporary
/// thread changes working directory, and its directory goes away on return.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn in_directory<T: Send>(
    dir: BorrowedFd<'_>,
    operation: impl FnOnce() -> io::Result<T> + Send,
) -> io::Result<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .spawn_scoped(scope, move || {
                host::isolate_binding_thread(dir)?;
                operation()
            })?
            .join()
            .map_err(|_| io::Error::other("Unix socket helper stopped"))?
    })
}

/// Bind `socket` at the empty slot `leaf` of the pinned directory `dir`. A
/// slot that holds any node, a symbolic link included, gives `EADDRINUSE`, and
/// no link is followed. The calling thread's working directory becomes `dir`,
/// so the call runs on a thread made for it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn bind_in(socket: BorrowedFd<'_>, dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<()> {
    host::bind_in(socket, dir, leaf)
}

#[cfg(target_os = "ios")]
pub fn bind_in(_: BorrowedFd<'_>, _: BorrowedFd<'_>, _: &CStr) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

/// `bind` of `socket` at `name`, a path relative to this thread's working
/// directory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn bind_named(socket: BorrowedFd<'_>, name: &CStr) -> io::Result<()> {
    let bytes = name.to_bytes();
    if bytes.len() > MAX_PATH_BYTES {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    // SAFETY: sockaddr_un is an integer-only structure.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, byte) in address.sun_path.iter_mut().zip(bytes) {
        *to = *byte as libc::c_char;
    }
    let len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    loop {
        // SAFETY: a live socket and an address of `len` initialized bytes.
        let bound = unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&raw const address).cast(),
                len as libc::socklen_t,
            )
        };
        if bound == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn probe() -> io::Result<()> {
    use std::os::fd::AsFd;

    fn requires(operation: &str, error: io::Error) -> io::Error {
        io::Error::other(format!("Net Unix requires {operation}: {error}"))
    }
    let private = PrivateDir::new().map_err(|e| requires("a private probe directory", e))?;
    let root = File::open(&private.0).map_err(|e| requires("opening the probe directory", e))?;
    let parent =
        open_beneath(root.as_fd(), c".", host::DIRECTORY).map_err(|e| requires("openat2", e))?;
    let listener = in_directory(parent.as_fd(), || UnixListener::bind("socket"))
        .map_err(|e| requires("bind and listen in the private directory", e))?;
    let _client = host::connect_pinned(root.as_fd(), c"socket")
        .map_err(|e| requires("connecting to the pinned endpoint", e))?;
    drop(listener);
    Ok(())
}

/// A fresh mode-0700 directory under the temporary directory, removed with
/// its contents when dropped.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct PrivateDir(std::path::PathBuf);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PrivateDir {
    fn new() -> io::Result<Self> {
        use std::os::unix::ffi::OsStringExt;
        let mut template = std::env::temp_dir()
            .join("hirsel-unix-probe-XXXXXX")
            .into_os_string()
            .into_vec();
        template.push(0);
        // SAFETY: template is a writable NUL-terminated path ending in XXXXXX.
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        Ok(Self(std::ffi::OsString::from_vec(template).into()))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "ios")]
pub fn probe() -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

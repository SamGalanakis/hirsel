//! Probe the same descriptor-relative operations required by NET-12 and NET-13.
use crate::host::unix as host;
use std::fs::File;
use std::io;
use std::os::{
    fd::{BorrowedFd, OwnedFd},
    unix::net::UnixListener,
};

/// A pathname address leaves one byte for the terminator.
pub const MAX_PATH_BYTES: usize =
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

pub fn open_beneath(
    root: BorrowedFd<'_>,
    path: &std::ffi::CStr,
    flags: i32,
) -> io::Result<OwnedFd> {
    host::open_beneath(root, path, flags)
}

pub fn isolate_binding_thread(dir: BorrowedFd<'_>) -> io::Result<()> {
    host::isolate_binding_thread(dir)
}

pub fn probe() -> io::Result<()> {
    use std::os::fd::AsFd;

    fn requires(operation: &str, error: io::Error) -> io::Error {
        io::Error::other(format!("Net Unix requires {operation}: {error}"))
    }
    let private = PrivateDir::new().map_err(|e| requires("a private probe directory", e))?;
    let root = File::open(&private.0).map_err(|e| requires("opening the probe directory", e))?;
    let parent =
        open_beneath(root.as_fd(), c".", host::DIRECTORY).map_err(|e| requires("openat2", e))?;
    let binding = std::thread::Builder::new()
        .name("hirsel-unix-probe".into())
        .spawn(move || {
            isolate_binding_thread(parent.as_fd())
                .map_err(|e| requires("a private working directory", e))?;
            UnixListener::bind("socket").map_err(|e| requires("bind and listen", e))
        })
        .map_err(|e| requires("a bind helper thread", e))?;
    let listener = binding
        .join()
        .map_err(|_| io::Error::other("Unix bind helper stopped"))??;
    let _client = host::connect_pinned(root.as_fd(), c"socket")
        .map_err(|e| requires("connecting to the pinned endpoint", e))?;
    drop(listener);
    Ok(())
}

/// A fresh mode-0700 directory under the temporary directory, removed with
/// its contents when dropped.
struct PrivateDir(std::path::PathBuf);

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

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

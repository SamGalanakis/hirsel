//! Probe the same descriptor-relative operations required by NET-12 and NET-13.
use std::fs::File;
use std::io;
use std::os::{
    fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    linux::fs::MetadataExt as _,
    unix::net::{UnixListener, UnixStream},
};

/// A Linux pathname address leaves one byte for the terminator.
pub const MAX_PATH_BYTES: usize =
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

pub fn open_beneath(
    root: BorrowedFd<'_>,
    path: &std::ffi::CStr,
    flags: i32,
) -> io::Result<OwnedFd> {
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x08 | 0x04, // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS
    };
    // SAFETY: open_how and the C path are pinned for the resolved open.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of_val(&how),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel returned an owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

pub fn isolate_binding_thread(dir: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: the probe and the bind helper use only this thread's copied fs state.
    unsafe {
        if libc::unshare(libc::CLONE_FS) < 0 || libc::fchdir(dir.as_raw_fd()) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn probe() -> io::Result<()> {
    use std::os::fd::AsFd;

    fn requires(operation: &str, error: io::Error) -> io::Error {
        io::Error::other(format!("Net Unix requires {operation}: {error}"))
    }
    let private = PrivateDir::new().map_err(|e| requires("a private probe directory", e))?;
    let root = File::open(&private.0).map_err(|e| requires("opening the probe directory", e))?;
    let parent = open_beneath(root.as_fd(), c".", libc::O_PATH | libc::O_DIRECTORY)
        .map_err(|e| requires("openat2", e))?;
    let binding = std::thread::Builder::new()
        .name("hirsel-unix-probe".into())
        .spawn(move || {
            isolate_binding_thread(parent.as_fd())
                .map_err(|e| requires("unshare(CLONE_FS) and fchdir", e))?;
            UnixListener::bind("socket").map_err(|e| requires("bind and listen", e))
        })
        .map_err(|e| requires("a bind helper thread", e))?;
    let listener = binding
        .join()
        .map_err(|_| io::Error::other("Unix bind helper stopped"))??;
    let endpoint = open_beneath(root.as_fd(), c"socket", libc::O_PATH | libc::O_NOFOLLOW)
        .map_err(|e| requires("openat2 on the socket endpoint", e))?;
    if File::from(endpoint.try_clone()?).metadata()?.st_mode() & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::other("Unix probe did not pin a socket"));
    }
    let _client = UnixStream::connect(format!("/proc/self/fd/{}", endpoint.as_raw_fd()))
        .map_err(|e| requires("connect through /proc/self/fd", e))?;
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

//! Probe the same descriptor-relative operations required by NET-12 and NET-13.
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    os::unix::{
        fs::MetadataExt as _,
        net::{UnixListener, UnixStream},
    },
};

/// A Linux pathname address leaves one byte for the terminator.
pub const MAX_PATH_BYTES: usize =
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// macOS resolves beneath the root without symlinks through open flags.
#[cfg(target_os = "macos")]
pub fn open_beneath(
    root: BorrowedFd<'_>,
    path: &std::ffi::CStr,
    flags: i32,
) -> io::Result<OwnedFd> {
    const O_RESOLVE_BENEATH: i32 = 0x1000;
    const O_NOFOLLOW_ANY: i32 = 0x2000_0000;
    // SAFETY: the C path is pinned for the open.
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            path.as_ptr(),
            flags | libc::O_CLOEXEC | O_RESOLVE_BENEATH | O_NOFOLLOW_ANY,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel returned an owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
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

/// macOS gives a thread its own working directory with `pthread_fchdir_np`.
#[cfg(target_os = "macos")]
pub fn isolate_binding_thread(dir: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: only this thread's working directory changes.
    if unsafe { crate::sys::pthread_fchdir_np(dir.as_raw_fd()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn isolate_binding_thread(dir: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: the probe and the bind helper use only this thread's copied fs state.
    unsafe {
        if libc::unshare(libc::CLONE_FS) < 0 || libc::fchdir(dir.as_raw_fd()) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Run `f` with this thread's working directory at `dir`, so a relative
/// socket path resolves beneath it, then return the thread to the process's.
#[cfg(target_os = "macos")]
pub fn at_directory<T>(dir: BorrowedFd<'_>, f: impl FnOnce() -> T) -> io::Result<T> {
    isolate_binding_thread(dir)?;
    let result = f();
    // SAFETY: only this thread's working directory changes.
    unsafe { crate::sys::pthread_fchdir_np(-1) };
    Ok(result)
}

/// macOS binds and connects a relative path with a per-thread cwd.
#[cfg(target_os = "macos")]
pub fn probe() -> io::Result<()> {
    use std::os::fd::AsFd;
    use std::os::unix::net::{UnixListener, UnixStream};

    fn requires(operation: &str, error: io::Error) -> io::Error {
        io::Error::other(format!("Net Unix requires {operation}: {error}"))
    }
    let private = tempfile::tempdir().map_err(|e| requires("a private probe directory", e))?;
    let root = std::fs::File::open(private.path())
        .map_err(|e| requires("opening the probe directory", e))?;
    let parent = open_beneath(root.as_fd(), c".", libc::O_RDONLY | libc::O_DIRECTORY)
        .map_err(|e| requires("open beneath a directory", e))?;
    let listener = at_directory(parent.as_fd(), || UnixListener::bind("socket"))
        .map_err(|e| requires("pthread_fchdir_np", e))?
        .map_err(|e| requires("bind and listen", e))?;
    let _client = at_directory(parent.as_fd(), || UnixStream::connect("socket"))?
        .map_err(|e| requires("connect by a relative path", e))?;
    drop(listener);
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn probe() -> io::Result<()> {
    use std::os::fd::AsFd;

    fn requires(operation: &str, error: io::Error) -> io::Error {
        io::Error::other(format!("Net Unix requires {operation}: {error}"))
    }
    let private = tempfile::tempdir().map_err(|e| requires("a private probe directory", e))?;
    let root =
        File::open(private.path()).map_err(|e| requires("opening the probe directory", e))?;
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
    if File::from(endpoint.try_clone()?).metadata()?.mode() & libc::S_IFMT as u32 != libc::S_IFSOCK
    {
        return Err(io::Error::other("Unix probe did not pin a socket"));
    }
    let _client = UnixStream::connect(format!("/proc/self/fd/{}", endpoint.as_raw_fd()))
        .map_err(|e| requires("connect through /proc/self/fd", e))?;
    drop(listener);
    Ok(())
}

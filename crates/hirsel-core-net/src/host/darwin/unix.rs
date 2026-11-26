//! Confined opens and Unix socket naming on Darwin: `openat` with
//! `O_RESOLVE_BENEATH | O_NOFOLLOW_ANY` (a refused escape is ENOTCAPABLE,
//! reported as EXDEV, the error Linux's `RESOLVE_BENEATH` gives), and
//! a bind or connect by leaf name from a thread whose working directory is
//! the pinned parent (`pthread_fchdir_np`). Darwin cannot open a socket file,
//! so a connect checks the leaf's device and inode instead of pinning it.
//! Its bind follows a link at the final component, so a bind runs at a private
//! name and an exclusive rename moves the node into its slot.
use std::{
    ffi::{CStr, CString},
    io,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
};

/// Resolve every component beneath the starting directory (xnu `fcntl.h`).
pub(crate) const O_RESOLVE_BENEATH: i32 = 0x1000;
/// A directory handle that grants lookups but no reads.
pub(crate) const DIRECTORY: i32 = libc::O_SEARCH | libc::O_DIRECTORY;

unsafe extern "C" {
    fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
}

pub(crate) fn open_beneath(root: BorrowedFd<'_>, path: &CStr, flags: i32) -> io::Result<OwnedFd> {
    let flags = flags | libc::O_CLOEXEC | O_RESOLVE_BENEATH | libc::O_NOFOLLOW_ANY;
    // SAFETY: the C path is live for the call; a success transfers a descriptor.
    let fd = unsafe { libc::openat(root.as_raw_fd(), path.as_ptr(), flags, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(libc::ENOTCAPABLE) => io::Error::from_raw_os_error(libc::EXDEV),
            _ => error,
        });
    }
    // SAFETY: openat returned an owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Give this thread its own working directory, `dir`.
pub(crate) fn isolate_binding_thread(dir: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: changes only this thread's working directory.
    if unsafe { pthread_fchdir_np(dir.as_raw_fd()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bind `socket` at the empty slot `leaf` of `dir`, from a thread made for the
/// call: its working directory becomes `dir`. Darwin's bind follows a link at
/// the final component and creates the node at the link's target, so the bind
/// runs at a private random name, where no link can be waiting, and an
/// exclusive rename moves the node into the slot. A slot that holds any node, a
/// link included, gives EADDRINUSE: the private node is removed and nothing is
/// followed. The socket's own address stays the private name.
pub(crate) fn bind_in(socket: BorrowedFd<'_>, dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<()> {
    isolate_binding_thread(dir)?;
    let mut random = [0u8; 16];
    // SAFETY: getentropy fills the 16 live bytes.
    if unsafe { libc::getentropy(random.as_mut_ptr().cast(), random.len()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let private = format!(".hirsel-bind-{:032x}", u128::from_ne_bytes(random));
    let private = CString::new(private).map_err(|_| io::ErrorKind::InvalidInput)?;
    crate::unix::bind_named(socket, &private)?;
    // SAFETY: both names are live C strings, each one component of `dir`.
    let moved = unsafe {
        libc::renameatx_np(
            dir.as_raw_fd(),
            private.as_ptr(),
            dir.as_raw_fd(),
            leaf.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if moved == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // SAFETY: removes the node this call created; no link is followed.
    unsafe { libc::unlinkat(dir.as_raw_fd(), private.as_ptr(), 0) };
    Err(match error.raw_os_error() {
        Some(libc::EEXIST) => io::Error::from_raw_os_error(libc::EADDRINUSE),
        _ => error,
    })
}

/// The device and inode of the socket named `leaf` in `dir`, not following a link.
pub(crate) fn socket_identity(dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<(u64, u64)> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the directory and the C leaf are live; fstatat fills st on success.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            leaf.as_ptr(),
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
        return Err(io::Error::other("the endpoint is not a socket"));
    }
    Ok((st.st_dev as u64, st.st_ino))
}

/// Connect to the socket named `leaf` in `dir` from a thread working in `dir`,
/// after checking that the leaf is a socket and still the same one.
pub(crate) fn connect_pinned(dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<UnixStream> {
    let identity = socket_identity(dir, leaf)?;
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                isolate_binding_thread(dir)?;
                let stream = UnixStream::connect(std::ffi::OsStr::from_bytes(leaf.to_bytes()))?;
                if socket_identity(dir, leaf)? != identity {
                    return Err(io::Error::other("the endpoint changed during connect"));
                }
                Ok(stream)
            })
            .join()
            .map_err(|_| io::Error::other("Unix connect helper stopped"))?
    })
}

//! Confined opens and Unix socket naming on Darwin: `openat` with
//! `O_RESOLVE_BENEATH | O_NOFOLLOW_ANY` (a refused escape is ENOTCAPABLE,
//! reported as EXDEV, the error Linux's `RESOLVE_BENEATH` gives), and
//! a bind or connect by leaf name from a thread whose working directory is
//! the pinned parent (`pthread_fchdir_np`). Darwin cannot open a socket file,
//! and its bind, connect and sendto follow a link at the final component. So a
//! bind runs at a private name and an exclusive rename moves the node into its
//! slot, and a connect or send-to reaches the socket through a hard link in a
//! private directory nobody can change (`freeze`).
use std::{
    ffi::{CStr, CString},
    io,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
};

/// Resolve every component beneath the starting directory (xnu `fcntl.h`).
pub(crate) const O_RESOLVE_BENEATH: i32 = 0x1000;
/// A directory handle that grants lookups but no reads.
pub(crate) const DIRECTORY: i32 = libc::O_SEARCH | libc::O_DIRECTORY;

#[cfg(not(target_os = "ios"))]
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
    #[cfg(target_os = "ios")]
    {
        let _ = dir;
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    #[cfg(not(target_os = "ios"))]
    {
        // SAFETY: changes only this thread's working directory.
        if unsafe { pthread_fchdir_np(dir.as_raw_fd()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
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

/// The device and inode of the socket named `leaf` in `dir`, not following a
/// link. Any other node there, a link included, is EACCES.
pub fn socket_identity(dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<(u64, u64)> {
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
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok((st.st_dev as u64, st.st_ino))
}

/// A hard link to a pinned socket in a fresh private directory that only its
/// owner can enter and nobody can change: a connect or send-to names
/// [`Frozen::NAME`] from a thread whose working directory is [`Frozen::dir`],
/// so no rename in the confined directory can swap what the call reaches.
/// Drop removes the link and the directory.
pub struct Frozen<'p> {
    parent: BorrowedFd<'p>,
    name: CString,
    dir: OwnedFd,
}

impl Frozen<'_> {
    /// The link's name inside [`Frozen::dir`].
    pub const NAME: &'static CStr = c"s";

    pub fn dir(&self) -> BorrowedFd<'_> {
        self.dir.as_fd()
    }
}

impl Drop for Frozen<'_> {
    fn drop(&mut self) {
        // SAFETY: the directory and its link are this call's own nodes.
        unsafe {
            libc::fchmod(self.dir.as_raw_fd(), 0o700);
            libc::unlinkat(self.dir.as_raw_fd(), Self::NAME.as_ptr(), 0);
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            );
        }
    }
}

/// Freeze the socket `leaf` of `dir` whose device and inode are `identity`.
/// Darwin's connect and sendto follow a link at the final component and a
/// socket file cannot be opened, so the name the call uses must be one nobody
/// else can change: a hard link (`linkat` without a flag links a symbolic link
/// itself) in a directory this call creates in `dir`, made search-only (0100)
/// before the link is checked. A directory this process does not own fails
/// `fchmod`, so nobody else can have planted it. A leaf that is not the pinned
/// socket when it is linked gives EACCES.
pub fn freeze<'p>(
    dir: BorrowedFd<'p>,
    leaf: &CStr,
    identity: (u64, u64),
) -> io::Result<Frozen<'p>> {
    let mut random = [0u8; 16];
    // SAFETY: getentropy fills the 16 live bytes.
    if unsafe { libc::getentropy(random.as_mut_ptr().cast(), random.len()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let name = format!(".hirsel-connect-{:032x}", u128::from_ne_bytes(random));
    let name = CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: a live directory and a C name of one component.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: as above; a success transfers a descriptor.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        // SAFETY: removes the empty directory this call made, if it is still there.
        unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
        return Err(error);
    }
    let frozen = Frozen {
        parent: dir,
        name,
        // SAFETY: openat returned an owned descriptor.
        dir: unsafe { OwnedFd::from_raw_fd(fd) },
    };
    // SAFETY: both directories and both C names are live for the calls.
    unsafe {
        if libc::linkat(
            dir.as_raw_fd(),
            leaf.as_ptr(),
            frozen.dir.as_raw_fd(),
            Frozen::NAME.as_ptr(),
            0,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
        if libc::fchmod(frozen.dir.as_raw_fd(), 0o100) < 0 {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
    }
    if socket_identity(frozen.dir(), Frozen::NAME)? != identity {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(frozen)
}

/// Connect to the socket named `leaf` in `dir` through a frozen link, from a
/// thread made for the call.
pub(crate) fn connect_pinned(dir: BorrowedFd<'_>, leaf: &CStr) -> io::Result<UnixStream> {
    let frozen = freeze(dir, leaf, socket_identity(dir, leaf)?)?;
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                isolate_binding_thread(frozen.dir())?;
                UnixStream::connect(std::ffi::OsStr::from_bytes(Frozen::NAME.to_bytes()))
            })
            .join()
            .map_err(|_| io::Error::other("Unix connect helper stopped"))?
    })
}

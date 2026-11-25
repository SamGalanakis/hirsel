//! Confined opens on Darwin: `openat` with `O_RESOLVE_BENEATH` resolves every
//! component beneath the granted directory and refuses an escape with
//! `ENOTCAPABLE`; `O_NOFOLLOW_ANY` refuses every symlink on the way.
use std::{
    ffi::CStr,
    io,
    os::fd::{FromRawFd, OwnedFd, RawFd},
};

/// `O_RESOLVE_BENEATH` (macOS 15+), not yet exported by the libc crate.
const O_RESOLVE_BENEATH: i32 = 0x1000;

/// A directory handle that grants lookups but no reads (`O_SEARCH`).
pub(crate) const DIRECTORY_HANDLE: i32 = libc::O_SEARCH | libc::O_DIRECTORY;
/// A handle on a leaf or a symlink itself, never its target: event-only
/// access grants `fstat` without read permission or a blocking open.
pub(crate) const LEAF_HANDLE: i32 = libc::O_EVTONLY | libc::O_SYMLINK | libc::O_NONBLOCK;

/// What a confined open refuses besides leaving the directory.
#[derive(Clone, Copy)]
pub(crate) enum Refuse {
    /// Kernel-made links. Darwin has none (`/dev/fd` is not a link), so
    /// this refuses only escapes.
    MagicLinks,
    /// Every symlink.
    Symlinks,
}

/// One confined open: `openat` flags and the creation mode.
pub(crate) struct How {
    flags: i32,
    mode: libc::c_uint,
}

pub(crate) fn how(flags: i32, refuse: Refuse) -> How {
    How {
        flags: flags
            | libc::O_CLOEXEC
            | O_RESOLVE_BENEATH
            | match refuse {
                Refuse::MagicLinks => 0,
                Refuse::Symlinks => libc::O_NOFOLLOW_ANY,
            },
        mode: if flags & libc::O_CREAT != 0 { 0o600 } else { 0 },
    }
}

/// Open `path` beneath the directory `fd` as `how` describes.
pub(crate) fn open_beneath(fd: RawFd, path: &CStr, how: &How) -> io::Result<OwnedFd> {
    // SAFETY: openat reads the pinned path for the call only.
    let raw = unsafe { libc::openat(fd, path.as_ptr(), how.flags, how.mode) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat transfers a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn checked(rc: libc::c_int) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The type bits (`S_IFMT`) of `name` in `parent`, not following a final
/// link; `None` when nothing has that name.
pub(crate) fn leaf_mode(parent: RawFd, name: &CStr) -> io::Result<Option<libc::mode_t>> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the parent and name are live; the final link is not followed.
    let rc = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    match checked(rc) {
        // SAFETY: a successful fstatat initialized the output.
        Ok(()) => Ok(Some(unsafe { st.assume_init() }.st_mode & libc::S_IFMT)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `renameat` of one component to another, each in its parent.
pub(crate) fn rename_at(
    from: RawFd,
    from_name: &CStr,
    to: RawFd,
    to_name: &CStr,
) -> io::Result<()> {
    // SAFETY: pinned parents and NUL-terminated leaves.
    checked(unsafe { libc::renameat(from, from_name.as_ptr(), to, to_name.as_ptr()) })
}

/// `mkdirat` of one component in its parent.
pub(crate) fn mkdir_at(parent: RawFd, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
    // SAFETY: pinned parent and NUL-terminated leaf.
    checked(unsafe { libc::mkdirat(parent, name.as_ptr(), mode) })
}

/// `unlinkat` of one component in its parent.
pub(crate) fn unlink_at(parent: RawFd, name: &CStr, flags: i32) -> io::Result<()> {
    // SAFETY: pinned parent and NUL-terminated leaf.
    checked(unsafe { libc::unlinkat(parent, name.as_ptr(), flags) })
}

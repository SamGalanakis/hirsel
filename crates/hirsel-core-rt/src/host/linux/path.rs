//! Confined opens on Linux: `openat2` with `RESOLVE_BENEATH` resolves every
//! component beneath the granted directory, symlinks included. There is no
//! weaker fallback; a host without openat2 refuses with ENOSYS.
use std::{
    ffi::CStr,
    io,
    os::fd::{FromRawFd, OwnedFd, RawFd},
};

/// A directory handle that grants lookups but no reads (`O_PATH`).
pub(crate) const DIRECTORY_HANDLE: i32 = libc::O_PATH | libc::O_DIRECTORY;
/// A handle on a leaf or a symlink itself, never its target.
pub(crate) const LEAF_HANDLE: i32 = libc::O_PATH | libc::O_NOFOLLOW;

/// What a confined open refuses besides leaving the directory.
#[derive(Clone, Copy)]
pub(crate) enum Refuse {
    /// Kernel-made links (`/proc/self/fd/N`).
    MagicLinks,
    /// Every symlink.
    Symlinks,
}

/// The kernel's description of one confined open.
pub(crate) type How = libc::open_how;

pub(crate) fn how(flags: i32, refuse: Refuse) -> How {
    // SAFETY: open_how is an integer-only Linux UAPI structure.
    let mut how: How = unsafe { std::mem::zeroed() };
    how.flags = (flags | libc::O_CLOEXEC) as u64;
    how.mode = if flags & libc::O_CREAT != 0 { 0o600 } else { 0 };
    how.resolve = libc::RESOLVE_BENEATH
        | match refuse {
            Refuse::MagicLinks => libc::RESOLVE_NO_MAGICLINKS,
            Refuse::Symlinks => libc::RESOLVE_NO_SYMLINKS,
        };
    how
}

/// Open `path` beneath the directory `fd` as `how` describes.
pub(crate) fn open_beneath(fd: RawFd, path: &CStr, how: &How) -> io::Result<OwnedFd> {
    // SAFETY: openat2 reads the pinned path and open_how for the call only.
    let raw = unsafe { libc::syscall(libc::SYS_openat2, fd, path.as_ptr(), how, size_of::<How>()) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat2 transfers a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
}

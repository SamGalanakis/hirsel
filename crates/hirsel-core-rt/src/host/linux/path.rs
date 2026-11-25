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

/// Open `path` beneath the directory `fd` as `how` describes, through the
/// unit's file ring when its pool owns one.
pub(crate) fn open_beneath(fd: RawFd, path: &CStr, how: &How) -> io::Result<OwnedFd> {
    let raw = if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::OpenAt2::new(
            io_uring::types::Fd(fd),
            path.as_ptr(),
            (&raw const *how).cast(),
        )
        .build();
        // SAFETY: the provider holds fd, and path and how are live through the CQE.
        unsafe { crate::units::file_request(entry) }?
    } else {
        // SAFETY: openat2 reads the pinned path and open_how for the call only.
        let raw =
            unsafe { libc::syscall(libc::SYS_openat2, fd, path.as_ptr(), how, size_of::<How>()) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        raw as i32
    };
    // SAFETY: a successful open transfers a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn checked(rc: libc::c_int) -> io::Result<i32> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// The type bits (`S_IFMT`) of `name` in the confined directory `parent`, not
/// following a final link; `None` when nothing has that name. `name` is one
/// validated component, so the lookup needs no `resolve`: confinement is the
/// parent's (§3.3b RENAME row).
pub(crate) fn leaf_mode(parent: RawFd, name: &CStr) -> io::Result<Option<libc::mode_t>> {
    let mut st = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let done = if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::Statx::new(
            io_uring::types::Fd(parent),
            name.as_ptr(),
            st.as_mut_ptr().cast(),
        )
        .flags(libc::AT_SYMLINK_NOFOLLOW)
        .mask(libc::STATX_TYPE)
        .build();
        // SAFETY: the caller holds parent, and name and the output live through the CQE.
        unsafe { crate::units::file_request(entry) }.map(drop)
    } else {
        // SAFETY: statx reads the pinned name and initializes the pinned output.
        checked(unsafe {
            libc::statx(
                parent,
                name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_TYPE,
                st.as_mut_ptr(),
            )
        })
        .map(drop)
    };
    match done {
        // SAFETY: a successful statx initialized the output.
        Ok(()) => Ok(Some(
            libc::mode_t::from(unsafe { st.assume_init() }.stx_mode) & libc::S_IFMT,
        )),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `renameat` of one validated component to another, each in its confined parent.
pub(crate) fn rename_at(
    from: RawFd,
    from_name: &CStr,
    to: RawFd,
    to_name: &CStr,
) -> io::Result<()> {
    if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::RenameAt::new(
            io_uring::types::Fd(from),
            from_name.as_ptr(),
            io_uring::types::Fd(to),
            to_name.as_ptr(),
        )
        .build();
        // SAFETY: the caller holds both parents, and both names live through the CQE.
        return unsafe { crate::units::file_request(entry) }.map(drop);
    }
    // SAFETY: pinned parents and NUL-terminated leaves.
    checked(unsafe { libc::renameat(from, from_name.as_ptr(), to, to_name.as_ptr()) }).map(drop)
}

/// `mkdirat` of one validated component in its confined parent.
pub(crate) fn mkdir_at(parent: RawFd, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
    if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::MkDirAt::new(io_uring::types::Fd(parent), name.as_ptr())
            .mode(mode)
            .build();
        // SAFETY: the caller holds parent, and name lives through the CQE.
        return unsafe { crate::units::file_request(entry) }.map(drop);
    }
    // SAFETY: pinned parent and NUL-terminated leaf.
    checked(unsafe { libc::mkdirat(parent, name.as_ptr(), mode) }).map(drop)
}

/// `unlinkat` of one validated component in its confined parent. It never
/// follows a final link.
pub(crate) fn unlink_at(parent: RawFd, name: &CStr, flags: i32) -> io::Result<()> {
    if crate::units::uses_file_ring() {
        let entry = io_uring::opcode::UnlinkAt::new(io_uring::types::Fd(parent), name.as_ptr())
            .flags(flags)
            .build();
        // SAFETY: the caller holds parent, and name lives through the CQE.
        return unsafe { crate::units::file_request(entry) }.map(drop);
    }
    // SAFETY: pinned parent and NUL-terminated leaf.
    checked(unsafe { libc::unlinkat(parent, name.as_ptr(), flags) }).map(drop)
}

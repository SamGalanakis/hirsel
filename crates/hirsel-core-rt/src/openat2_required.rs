//! Executed precondition for the exact tests that require the OS Dir provider's
//! confined opens: openat2 with RESOLVE_BENEATH on Linux, openat with
//! O_RESOLVE_BENEATH on Darwin (macOS 26). The driver's tests include this file
//! by path, so it names no crate item.
use std::{
    ffi::{c_char, c_int},
    os::fd::{AsRawFd, FromRawFd},
};

/// A confined read-only open of `.` beneath `directory`, or -1 with errno.
#[cfg(target_os = "linux")]
fn open_beneath(directory: c_int) -> c_int {
    use std::ffi::{c_long, c_void};
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
    }
    let how = OpenHow {
        // O_RDONLY is zero on both supported Linux architectures. The existing
        // readable directory descriptor needs the same authority.
        flags: 0,
        mode: 0,
        resolve: 0x08 | 0x02,
    };
    // SAFETY: Linux x86-64/AArch64 syscall 437, pinned C pathname and open_how.
    unsafe {
        syscall(
            437 as c_long,
            directory,
            c".".as_ptr() as *const c_char,
            &how as *const OpenHow as *const c_void,
            std::mem::size_of::<OpenHow>(),
        ) as c_int
    }
}

/// A confined read-only open of `.` beneath `directory`, or -1 with errno.
#[cfg(target_os = "macos")]
fn open_beneath(directory: c_int) -> c_int {
    /// `O_RESOLVE_BENEATH` in the macOS SDK's `fcntl.h`; `O_RDONLY` is zero.
    const O_RESOLVE_BENEATH: c_int = 0x1000;
    unsafe extern "C" {
        fn openat(directory: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    }
    // SAFETY: a live directory descriptor and a pinned C pathname.
    unsafe { openat(directory, c".".as_ptr(), O_RESOLVE_BENEATH) }
}

pub(crate) fn require() {
    let directory =
        std::fs::File::open(".").expect("confined-open precondition: open test directory");
    let fd = open_beneath(directory.as_raw_fd());
    assert!(
        fd >= 0,
        "requires the host's confined open beneath a directory (openat2 with RESOLVE_BENEATH, or openat with O_RESOLVE_BENEATH): this host/sandbox returned {}; these tests must execute on a capable host, never skip or use a weaker open",
        std::io::Error::last_os_error()
    );
    // SAFETY: a successful open transfers exactly one new descriptor.
    drop(unsafe { std::fs::File::from_raw_fd(fd) });
}

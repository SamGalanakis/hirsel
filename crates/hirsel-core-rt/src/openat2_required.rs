//! Executed precondition for the exact tests that require the Linux OS Dir provider.
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_int, c_long, c_void};
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
}

/// macOS resolves beneath a directory with open flags; the same shim the
/// Dir provider uses must refuse nothing here.
#[cfg(not(target_os = "linux"))]
pub(crate) fn require() {
    use hirsel_core_net::sys;
    let directory = std::fs::File::open(".").expect("openat2 precondition: open test directory");
    // SAFETY: a pinned C pathname and a live directory descriptor.
    let fd = unsafe {
        sys::openat2(
            directory.as_raw_fd(),
            c".".as_ptr(),
            0,
            0,
            sys::RESOLVE_BENEATH | sys::RESOLVE_NO_MAGICLINKS,
        )
    };
    assert!(
        fd >= 0,
        "requires opening beneath a directory: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: a successful open transfers exactly one new descriptor.
    drop(unsafe { std::fs::File::from_raw_fd(fd) });
}

#[cfg(target_os = "linux")]
pub(crate) fn require() {
    let directory = std::fs::File::open(".").expect("openat2 precondition: open test directory");
    let how = OpenHow {
        // O_RDONLY is zero on both supported Linux architectures. The existing
        // readable directory descriptor above needs the same authority.
        flags: 0,
        mode: 0,
        resolve: 0x08 | 0x02,
    };
    // SAFETY: Linux x86-64/AArch64 syscall 437, pinned C pathname and open_how.
    let fd = unsafe {
        syscall(
            437 as c_long,
            directory.as_raw_fd() as c_int,
            c".".as_ptr() as *const c_char,
            &how as *const OpenHow as *const c_void,
            std::mem::size_of::<OpenHow>(),
        )
    };
    assert!(
        fd >= 0,
        "requires openat2 with RESOLVE_BENEATH: this host/sandbox returned {}; these tests must execute on an openat2-capable host, never skip or use a weaker open",
        std::io::Error::last_os_error()
    );
    // SAFETY: a successful syscall transfers exactly one new descriptor.
    drop(unsafe { std::fs::File::from_raw_fd(fd as c_int) });
}

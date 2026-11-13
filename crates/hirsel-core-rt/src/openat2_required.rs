//! Executed precondition for the exact tests that require the Linux OS Dir provider.
use std::{
    ffi::{c_char, c_int, c_long, c_void},
    os::fd::{AsRawFd, FromRawFd},
};
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
}
pub(crate) fn require() {
    let directory = std::fs::File::open(".").expect("openat2 precondition: open test directory");
    let how = OpenHow {
        flags: 0x20_0000 | 0x8_0000 | 0x1_0000,
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

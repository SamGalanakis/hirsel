//! iOS: no slab. Every image is prelinked into the host image (PACK-5).

use super::Access;
use std::io;

#[cfg(target_os = "macos")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;
#[cfg(all(target_os = "ios", not(target_abi = "sim")))]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Ios;
#[cfg(target_abi = "sim")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64IosSim;

fn none<T>() -> io::Result<T> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn page() -> usize {
    16 << 10
}

pub(crate) fn register() -> io::Result<()> {
    none()
}

pub(crate) fn reserve(_bytes: usize) -> io::Result<usize> {
    none()
}

pub(crate) fn release(_at: usize, _bytes: usize) {}

pub(crate) fn protect(_at: usize, _bytes: usize, _access: Access) -> io::Result<()> {
    none()
}

pub(crate) fn discard(_at: usize, _bytes: usize) {}

pub(crate) const KEEPS: bool = false;

/// How many mappings `[low, high)` is in, and how many of them allow no access. This
/// host does not count them.
pub(crate) fn mappings(_low: usize, _high: usize) -> Option<(usize, usize)> {
    None
}

/// Whether `write` can write a published text run, so a dead image's instructions on a
/// page a live image shares are filled with traps.
pub(crate) const FILLS: bool = false;

pub(crate) fn write(work: &mut dyn FnMut()) {
    work()
}

pub(crate) fn publish(_at: usize, _bytes: usize) -> io::Result<()> {
    none()
}

/// Whether this process can have a slab; the laws of one run only where it can.
#[cfg(test)]
pub(crate) fn slab() -> bool {
    false
}

#[cfg(test)]
pub(crate) const LIVE: &str = "";
#[cfg(test)]
pub(crate) const OPEN: &str = "";
#[cfg(test)]
pub(crate) const FAULT: i32 = libc::SIGBUS;

#[cfg(test)]
pub(crate) fn protection(_at: usize) -> String {
    String::new()
}

#[cfg(test)]
pub(crate) fn pc(_info: *mut libc::siginfo_t, _context: *mut libc::c_void) -> usize {
    0
}

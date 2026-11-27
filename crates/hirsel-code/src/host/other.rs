//! A host with no slab yet: no loader is created on it (PACK-4 `Unsupported`).

use super::Access;
use std::io;

#[cfg(target_os = "macos")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;
#[cfg(all(target_os = "ios", not(target_abi = "sim")))]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Ios;
#[cfg(all(target_os = "ios", target_abi = "sim"))]
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

pub(crate) fn publish(_at: usize, _bytes: usize) -> io::Result<()> {
    none()
}

/// Whether this host has a slab; the laws of one run only where it does.
#[cfg(test)]
pub(crate) const SLAB: bool = false;

#[cfg(test)]
pub(crate) fn protection(_at: usize) -> String {
    String::new()
}

//! Where the memory limit lives on each host, how a sanitizer's child is freed
//! of it, and the figures the limit's message reports. Every function here is
//! allocation-free: the allocator calls them.
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
pub(crate) use darwin::*;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;

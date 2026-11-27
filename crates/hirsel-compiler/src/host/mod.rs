//! What a worker process does to itself before it runs, per host.
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
pub(crate) use darwin::*;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;

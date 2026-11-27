//! What a worker process does to itself before it runs, per host.
use std::path::Path;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
pub(crate) use darwin::*;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;

/// Inputs to OS-level confinement for a `check_library` worker (`registry.md` §2).
pub(crate) struct ConfinementSpec<'a> {
    pub(crate) worker: &'a Path,
    pub(crate) scratch: &'a Path,
    pub(crate) cpu_seconds: u64,
    pub(crate) memory_bytes: u64,
    pub(crate) file_bytes: u64,
}

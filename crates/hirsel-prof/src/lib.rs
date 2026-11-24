//! Portable CPU profiles for Hirsel.

// macOS profiles take no memory samples: only the record decoding is used.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod memory;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod observation;
mod pprof;
mod profile;
mod query;
mod tail;

pub use pprof::export_pprof;
pub use profile::{Profile, ProfileWriter, RunMeta};
pub use query::{By, Group, Query, QueryResult, query};

/// Build-ID-bound source remarks in Hirsel's ELF debug product.
pub const CODEGEN_REMARK_SECTION: &str = ".debug_hirsel_remarks";

#[cfg(test)]
hirsel_guard::install!();

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::{
    CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource, attach, replay, run,
};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use macos::{
    CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource, attach, replay, run,
};
#[cfg(not(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
mod unsupported;
#[cfg(not(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
pub use unsupported::{
    CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource, attach, replay, run,
};

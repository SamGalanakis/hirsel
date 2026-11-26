//! Portable CPU profiles for Hirsel.

#[cfg(target_os = "linux")]
mod memory;
#[cfg(target_os = "linux")]
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

mod capture;
pub use capture::{CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource};

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::{attach, replay, run};
#[cfg(target_os = "macos")]
mod unsupported;
#[cfg(target_os = "macos")]
pub use unsupported::{attach, replay, run};

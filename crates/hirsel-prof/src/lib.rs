//! Portable CPU profiles for Hirsel.

mod pprof;
mod profile;
mod query;

pub use pprof::export_pprof;
pub use profile::{Profile, ProfileWriter, RunMeta};
pub use query::{By, Group, Query, QueryResult, query};

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
pub use linux::{CaptureOptions, Launch, SymbolSource, attach, run};

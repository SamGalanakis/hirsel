//! Portable CPU profiles for Hirsel.

mod compact;
mod image;
mod import;
mod memory;
mod observation;
mod pprof;
mod profile;
mod query;
mod record;
mod tail;
mod unwind;

pub use pprof::export_pprof;
pub use profile::{Profile, ProfileWriter, RunMeta};
pub use query::{By, Group, Query, QueryResult, query};

/// Build-ID-bound source remarks in Hirsel's ELF debug product.
pub const CODEGEN_REMARK_SECTION: &str = ".debug_hirsel_remarks";

#[cfg(test)]
hirsel_guard::install!();

mod capture;
pub use capture::{CaptureOptions, Launch, MemMode, ObservationOptions, SymbolSource, replay};

mod host;
pub use host::{attach, run};

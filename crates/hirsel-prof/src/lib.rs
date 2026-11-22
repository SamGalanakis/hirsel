//! Portable CPU profiles for Hirsel.

mod profile;

pub use profile::{Profile, ProfileWriter, RunMeta};

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
pub use linux::{CaptureOptions, Launch, SymbolSource, run};

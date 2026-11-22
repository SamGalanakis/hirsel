//! Portable CPU profiles for Hirsel.

mod profile;

pub use profile::{Profile, ProfileWriter, RunMeta};

#[cfg(test)]
hirsel_guard::install!();

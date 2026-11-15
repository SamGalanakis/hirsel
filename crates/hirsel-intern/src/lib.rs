//! Hardware instruction counters and session anchor assignment.

#[cfg(test)]
hirsel_guard::install!();

pub mod instructions;
mod salt;

pub use salt::Scramble;
#[cfg(feature = "id-salt")]
pub use salt::{salt_ids, under_salts, under_salts_with_env};

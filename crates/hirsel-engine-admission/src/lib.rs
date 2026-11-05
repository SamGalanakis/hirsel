//! Process query reservations and checked run admission ownership.
#[cfg(test)]
hirsel_guard::install!();

mod budget;
pub use budget::{MemoryReservation, MemoryReservationPool, ProcessBudget};
mod run;
pub use run::{admit_run, bind_run, callee_roots, entry_launch_kind, entry_roots, read_run_record};

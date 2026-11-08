//! Process query reservations and checked run admission ownership.
#[cfg(test)]
hirsel_guard::install!();

mod budget;
pub use budget::{MemoryReservation, ProcessBudget};
mod run;
pub use run::{admit_run, bind_run, callee_roots, entry_launch_kind, entry_roots, read_run_record};

/// One retained value's charge on the request footprint, released when the
/// value's last owner drops it.
pub struct RetainedArchiveLease(pub MemoryReservation);

pub fn arc_bytes<T: ?Sized>(value: &T) -> usize {
    std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::for_value(value))
        .expect("an allocated Arc has a valid layout")
        .0
        .pad_to_align()
        .size()
}

pub fn retained_arc_bytes<T: ?Sized>(value: &T) -> u64 {
    arc_bytes(value) as u64
}

#[cfg(feature = "test-alloc")]
pub mod test_alloc;

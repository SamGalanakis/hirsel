//! Process query reservations and checked run admission ownership.
#[cfg(test)]
hirsel_guard::install!();

mod budget;
pub use budget::{MemoryReservation, MemoryReservationPool, ProcessBudget};
mod run;
pub use run::{admit_run, bind_run, callee_roots, entry_launch_kind, entry_roots, read_run_record};

pub struct RetainedArchiveLease {
    pub _allowance: std::sync::Arc<MemoryReservation>,
    pub remaining: std::sync::Arc<std::sync::Mutex<u64>>,
    pub bytes: u64,
}
impl Drop for RetainedArchiveLease {
    fn drop(&mut self) {
        let mut remaining = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        *remaining += self.bytes;
    }
}

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

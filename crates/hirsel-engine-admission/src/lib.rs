//! Process query reservations and checked run admission ownership.
#[cfg(test)]
hirsel_guard::install!();

pub use hirsel_guard::admission::{MemoryReservation, ProcessBudget};
mod run;
pub use run::{
    admit_run, bind_run, callee_roots, entry_launch_kind, entry_roots, identified_entry,
    read_run_record,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_transfers_one_charge_and_each_owner_releases_its_part() {
        let budget = ProcessBudget::new(100);
        let mut outer = budget.reserve(100).unwrap();
        assert!(outer.split(101).is_none());
        assert_eq!(outer.bytes(), 100);
        assert_eq!(budget.reserved(), 100);
        let first = outer.split(30).unwrap();
        let mut second = outer.split(50).unwrap();
        let nested = second.split(40).unwrap();
        assert_eq!(
            outer.bytes() + first.bytes() + second.bytes() + nested.bytes(),
            100
        );
        assert_eq!(budget.reserved(), 100);
        assert!(budget.reserve(1).is_err());
        drop(outer);
        assert_eq!(budget.reserved(), 80);
        let replacement = budget.reserve(20).unwrap();
        drop(first);
        drop(second);
        drop(nested);
        assert_eq!(budget.reserved(), 20);
        drop(replacement);
        assert_eq!(budget.reserved(), 0);
    }
}

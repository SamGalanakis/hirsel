//! ABI fixtures for FORM v1b's ordinary producer code. Writers and compact
//! checks are emitted code; this runtime owns identity, storage and refunds.

use crate::{Capacity, Context, abi::*};
use hirsel_core_layout::charge;
use std::ptr::NonNull;

struct OpenRegion(*mut crate::region::Region);
impl OpenRegion {
    fn new(chunk: usize) -> Self {
        Self(hirsel_core_region_open(chunk))
    }
    fn allocate(
        &mut self,
        ctx: &Context,
        bytes: usize,
        charge: Option<u64>,
    ) -> Result<NonNull<u8>, Capacity> {
        // SAFETY: the fixture owns its region and borrows a live context; sizes
        // describe raw region storage, including any uncharged engine header.
        let result = unsafe {
            hirsel_core_region_alloc(
                ctx,
                self.0,
                bytes,
                8,
                charge.unwrap_or(u64::MAX),
                charge.is_none(),
            )
        };
        NonNull::new(result.pointer).ok_or(result.capacity)
    }
}
impl Drop for OpenRegion {
    fn drop(&mut self) {
        // SAFETY: the fixture ends every view before closing its unique region.
        unsafe {
            hirsel_core_region_close(self.0);
        }
    }
}

#[test]
fn destination_identity_needs_no_context_status_or_admission() {
    let operation: extern "C" fn() -> u64 = hirsel_core_dest_id;
    let ctx = Context::new(0);
    ctx.current.inject(1, true);
    let mut previous = operation();
    for _ in 0..1000 {
        let current = operation();
        assert_ne!(current, previous);
        previous = current;
    }
    assert_eq!(ctx.available(), 0);
    assert_eq!(ctx.current.attempts(), 0);
}

#[test]
fn destination_identity_is_unique_across_workers_and_released_storage() {
    let workers: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| (0..1000).map(|_| hirsel_core_dest_id()).collect::<Vec<_>>())
        })
        .collect();
    let mut ids: std::collections::BTreeSet<_> = workers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .collect();
    assert_eq!(ids.len(), 4000);
    for _ in 0..16 {
        let ctx = Context::new(8);
        let mut region = OpenRegion::new(128);
        let _allocation = region.allocate(&ctx, 24, Some(8)).unwrap();
        assert!(ids.insert(hirsel_core_dest_id()));
        drop(region);
        assert_eq!(ctx.available(), 8);
    }
}

#[test]
fn blob_header_is_uncharged_trim_keeps_payer_and_closed_copy_keeps_actual_width() {
    for chunk in [128, 65536] {
        let mut ctx = Context::new(128);
        let mut source = OpenRegion::new(chunk);
        let mut target = OpenRegion::new(chunk);
        ctx.limit_enter(64).unwrap();
        let blob = source.allocate(&ctx, 16 + 64, charge::region(64)).unwrap();
        assert_eq!(
            ctx.available(),
            0,
            "the 16-byte engine header is not charged"
        );
        // FORM sequences.core f0(3,64): Arr header 9 + three Null tags = 12.
        // SAFETY: the producer writes exactly the initialized blob header and
        // its completed 12-byte packed prefix, within the reserved 80 native bytes.
        unsafe {
            blob.as_ptr().cast::<u64>().write(12);
            blob.as_ptr().add(8).cast::<u64>().write(4);
            blob.as_ptr().add(16).write(1);
            blob.as_ptr()
                .add(17)
                .cast::<u32>()
                .write_unaligned(3u32.to_le());
            blob.as_ptr()
                .add(21)
                .cast::<u32>()
                .write_unaligned(3u32.to_le());
            blob.as_ptr().add(25).write_bytes(0, 3);
        }
        ctx.limit_exit();
        ctx.limit_enter(16).unwrap();
        // SAFETY: finish_trim retains round8(12), preserving every completed byte.
        unsafe {
            hirsel_core_region_lower_charge(
                source.0,
                (*source.0).entry,
                64,
                charge::region(12).unwrap(),
            );
        }
        assert_eq!(
            ctx.available(),
            16,
            "trim refunds the blob's original payer"
        );
        let closed = target.allocate(&ctx, 16 + 1, charge::region(1)).unwrap();
        // SAFETY: Null is a closed link-free one-byte subtree; copy exactly that
        // extent and retain its actual W=4, regardless of the child declaration.
        unsafe {
            closed.as_ptr().cast::<u64>().write(1);
            closed
                .as_ptr()
                .add(8)
                .cast::<u64>()
                .write(blob.as_ptr().add(8).cast::<u64>().read());
            std::ptr::copy_nonoverlapping(blob.as_ptr().add(25), closed.as_ptr().add(16), 1);
        }
        assert_eq!(ctx.available(), 8);
        ctx.limit_exit();
        assert_eq!(ctx.available(), 104); // retained source 16 and closed copy 8.
        drop(source);
        assert_eq!(ctx.available(), 120);
        // SAFETY: the target owns the copied bytes after the source region ends.
        unsafe {
            assert_eq!(closed.as_ptr().add(8).cast::<u64>().read(), 4);
            assert_eq!(closed.as_ptr().add(16).read(), 0);
        }
        drop(target);
        assert_eq!(ctx.available(), 128);
    }
}

#[test]
fn failed_stream_keeps_its_full_reservation_and_completed_bytes_do_not_move() {
    for chunk in [128, 65536] {
        let ctx = Context::new(128);
        let mut region = OpenRegion::new(chunk);
        // FORM f0(3,10): tag, reserved sequence header, one complete Null;
        // the next Null cannot fit. No finish or trim occurs on this path.
        let blob = region.allocate(&ctx, 16 + 10, charge::region(10)).unwrap();
        // SAFETY: initialize the completed child and metadata before exposing its view.
        unsafe {
            blob.as_ptr().add(8).cast::<u64>().write(4);
            blob.as_ptr().add(16).write(1);
            blob.as_ptr().add(25).write(0);
        }
        let child = blob.as_ptr().wrapping_add(25);
        let _later = region.allocate(&ctx, 16 + 64, charge::region(64)).unwrap();
        assert_eq!(
            ctx.available(),
            48,
            "failed build keeps round8(10), plus later 64"
        );
        assert_eq!(
            region.allocate(&ctx, 16 + 64, charge::region(64)).err(),
            Some(Capacity {
                requested: 64,
                available: 48
            })
        );
        // SAFETY: completed-child views stay live until region close, including
        // after a failed parent and after later chunks are allocated.
        unsafe {
            assert_eq!(child.read(), 0);
        }
        drop(region);
        assert_eq!(ctx.available(), 128);
    }
}

#[test]
fn compact_reservations_closed_copies_and_indexes_fail_at_each_and_all_later_admissions() {
    let mut runs = 0;
    for chunk in [128, 65536] {
        for later in [false, true] {
            for k in 1..=4 {
                let ctx = Context::new(128);
                ctx.current.inject(k, later);
                let mut region = OpenRegion::new(chunk);
                let mut retained = 0;
                if let Ok(_blob) = region.allocate(&ctx, 16 + 64, charge::region(64)) {
                    // SAFETY: this fixture's completed source uses twelve bytes.
                    unsafe {
                        hirsel_core_region_lower_charge(
                            region.0,
                            (*region.0).entry,
                            64,
                            charge::region(12).unwrap(),
                        );
                    }
                    retained += 16;
                    if region.allocate(&ctx, 16 + 1, charge::region(1)).is_ok() {
                        retained += 8;
                    }
                    // An index of the source is independent of whether copying
                    // its subtree succeeded: Nth-only may resume, all-later may not.
                    if region.allocate(&ctx, 48, charge::seq_index(3)).is_ok() {
                        retained += 48;
                    }
                }
                assert_eq!(ctx.available(), 128 - retained);
                drop(region);
                assert_eq!(ctx.available(), 128);
                runs += 1;
            }
        }
    }
    assert_eq!(runs, 16);
}

#[test]
fn index_charge_overflow_is_a_capacity_value_before_any_storage_is_written() {
    let ctx = Context::new(128);
    let mut region = OpenRegion::new(128);
    assert_eq!(charge::seq_index(3), Some(48));
    assert_eq!(
        region.allocate(&ctx, 48, charge::seq_index(u64::MAX)).err(),
        Some(Capacity {
            requested: u64::MAX,
            available: 128
        })
    );
    assert_eq!(ctx.available(), 128);
    drop(region);
    assert_eq!(ctx.available(), 128);
}

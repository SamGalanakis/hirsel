use crate::{Context, block::Block, region::Region};
use hirsel_core_layout::charge;

#[test]
fn physical_chunk_sizes_do_not_change_capacity_or_move_stable_bytes() {
    let mut ctx = Context::new(128);
    let mut run = || {
        let mut region = Region::open(&ctx);
        ctx.limit_enter(64).unwrap();
        let first = region.allocate(&ctx, 13, 64, charge::region(13)).unwrap();
        // SAFETY: first owns at least thirteen writable bytes until region close.
        unsafe {
            first.as_ptr().write_bytes(0x5a, 13);
        }
        ctx.limit_exit();
        let mut result = Vec::new();
        for _ in 0..10 {
            match region.allocate(&ctx, 17, 8, charge::region(17)) {
                Ok(_) => result.push(Ok(())),
                Err(c) => {
                    result.push(Err(c));
                    break;
                }
            }
        }
        // SAFETY: no allocation or hand-over moved the first region allocation.
        unsafe {
            assert_eq!(std::slice::from_raw_parts(first.as_ptr(), 13), &[0x5a; 13]);
        }
        region.close();
        assert_eq!(ctx.available(), 128);
        result
    };
    assert_eq!(run(), run());
}

#[test]
fn partial_refund_follows_allocation_payer_and_close_refunds_only_the_tail_balance() {
    let mut ctx = Context::new(8192);
    let mut region = Region::open(&ctx);
    ctx.limit_enter(4096).unwrap();
    let _p = region.allocate(&ctx, 4096, 8, Some(4096)).unwrap();
    let entry = region.entry;
    ctx.limit_exit();
    ctx.limit_enter(1024).unwrap();
    // SAFETY: the sole writer retains 80 logical bytes of its completed prefix.
    unsafe {
        region.lower_charge(entry, 4096, 80);
    }
    assert_eq!(ctx.available(), 1024);
    ctx.limit_exit();
    assert_eq!(ctx.available(), 8112);
    drop(region);
    assert_eq!(ctx.available(), 8192);
}

#[test]
fn failed_region_prefixes_remain_charged_until_close_at_every_admission() {
    let mut runs = 0;
    for warm in [false, true] {
        for later in [false, true] {
            for k in 1..=7 {
                let ctx = Context::new(128);
                if warm {
                    let mut previous = Region::open(&ctx);
                    previous.allocate(&ctx, 8, 8, Some(0)).unwrap();
                    previous.close();
                }
                ctx.current.inject(k, later);
                let mut region = Region::open(&ctx);
                let mut retained = 0;
                for _ in 0..6 {
                    if region.allocate(&ctx, 8, 8, Some(8)).is_err() {
                        break;
                    }
                    retained += 8;
                }
                assert_eq!(ctx.available(), 128 - retained);
                drop(region);
                assert_eq!(ctx.available(), 128);
                // No failure cleanup admission and no allocation record retained.
                let b = Block::new(&ctx, Some(0), 1, 8);
                drop(b);
                assert_eq!(ctx.current.records(), 1);
                runs += 1;
            }
        }
    }
    assert_eq!(runs, 28);
}

#[test]
fn explicitly_reserved_empty_spans_pay_the_header_and_finish_refunds_nothing() {
    let ctx = Context::new(64);
    let mut region = Region::open(&ctx);
    let _span = region.allocate(&ctx, 8, 8, charge::region(8)).unwrap();
    let _text = region.allocate(&ctx, 8, 8, charge::region(8)).unwrap();
    assert_eq!(ctx.available(), 48);
    drop(region);
    assert_eq!(ctx.available(), 64);
}

#[test]
fn region_c_abi_preserves_prefix_and_refunds_lowered_charge() {
    use crate::abi::*;
    // SAFETY: follows the C allocation ownership and sole-writer trim contract.
    unsafe {
        let ctx = hirsel_core_context_new(128);
        let region = hirsel_core_region_open(ctx);
        let allocation = hirsel_core_region_alloc(ctx, region, 64, 16, 64, false);
        allocation.pointer.write(73);
        hirsel_core_region_lower_charge(region, (*region).entry, 64, 8);
        assert_eq!(allocation.pointer.read(), 73);
        assert_eq!((*ctx).available(), 120);
        hirsel_core_region_close(region);
        assert_eq!((*ctx).available(), 128);
        hirsel_core_context_free(ctx);
    }
}

#[test]
fn shared_payer_entries_trim_each_builder_independently_across_spills_and_slot_pages() {
    let mut ctx = Context::new(8192);
    let mut region = Region::open(&ctx);
    let mut reservations = Vec::new();
    for depth in 0..80 {
        ctx.limit_enter(8192 - depth * 80).unwrap();
        region.allocate(&ctx, 32, 8, Some(32)).unwrap();
        let entry = region.entry;
        region.allocate(&ctx, 16, 8, Some(16)).unwrap();
        assert_eq!(region.entry, entry);
        reservations.push(entry);
    }
    for _ in 0..80 {
        ctx.limit_exit();
    }
    for entry in reservations {
        // SAFETY: distinct reservations share an entry; each refunds only its own tail.
        unsafe {
            region.lower_charge(entry, 32, 8);
            region.lower_charge(entry, 16, 0);
        }
    }
    assert_eq!(ctx.available(), 8192 - 80 * 8);
    drop(region);
    assert_eq!(ctx.available(), 8192);
}

#[test]
fn zero_charge_and_fully_trimmed_regions_outlive_their_payer_tables() {
    let ctx = Context::new(8);
    let mut region = Region::open(&ctx);
    let zero = region.allocate(&ctx, 1, 8, Some(0)).unwrap();
    let empty_entry = region.entry;
    let charged = region.allocate(&ctx, 8, 8, Some(8)).unwrap();
    let entry = region.entry;
    // SAFETY: both reservations are exclusively owned; all charged bytes are unused.
    unsafe {
        zero.as_ptr().write(42);
        charged.as_ptr().write(7);
        region.lower_charge(entry, 8, 0);
    }
    assert_eq!(ctx.available(), 8);
    drop(ctx);
    // SAFETY: a zero-size trim touches no dead payer; region chunks still own bytes.
    unsafe {
        region.lower_charge(empty_entry, 0, 0);
        region.lower_charge(entry, 0, 0);
        assert_eq!(zero.as_ptr().read(), 42);
        assert_eq!(charged.as_ptr().read(), 7);
    }
    let next = Context::new(8);
    region.allocate(&next, 8, 8, Some(8)).unwrap();
    drop(next);
    drop(region);
}

#[test]
fn demand_sized_chunks_bound_reserved_bytes_at_each_allocation() {
    use crate::region::GRANULE;

    let context = Context::new(u64::MAX);
    let mut region = Region::open(&context);
    let mut occupied = 0;
    let small_count = if cfg!(miri) { 64 } else { 2_000 };
    for (i, bytes) in std::iter::once(73)
        .chain(std::iter::repeat_n(24, small_count))
        .chain((1..64).map(|n| n * 73))
        .enumerate()
    {
        region.allocate(&context, bytes, 1, Some(0)).unwrap();
        occupied += bytes;
        if i == 0 {
            assert_eq!(region.physical_bytes(), GRANULE);
        }
        assert!(region.physical_bytes() <= 2 * occupied + GRANULE);
    }
    region.close();
}

#[test]
fn mixed_region_pools_pack_slots_at_their_own_size_and_alignment() {
    use crate::region::GRANULE;

    let context = Context::new(4096);
    let mut region = Region::open(&context);
    let mut positions = Vec::new();
    for _ in 0..3 {
        let small = region.allocate(&context, 24, 8, Some(24)).unwrap();
        let aligned = region.allocate(&context, 32, 32, Some(16)).unwrap();
        positions.push((small.as_ptr().addr(), aligned.as_ptr().addr()));
    }
    for (i, (small, aligned)) in positions.iter().enumerate() {
        assert_eq!(*small, positions[0].0 + i * 24);
        assert_eq!(*aligned % 32, 0);
    }
    assert_eq!(positions[2].1, positions[1].1 + 32);
    let occupied = 3 * (24 + 32);
    // The 24-byte pool uses the initial granule. The new 32-byte pool
    // needs one slot, then grows to 64 bytes for its next two slots.
    let end_slack = GRANULE - 3 * 24;
    assert_eq!(region.physical_bytes() - occupied, end_slack);
    assert_eq!(context.available(), 4096 - 3 * (24 + 16));
    region.close();
    assert_eq!(context.available(), 4096);
}

#[test]
fn closing_and_reopening_a_region_reuses_chunks_without_visiting_values() {
    use crate::region::{CHUNK_RELEASES, CHUNK_REQUESTS};
    let context = Context::new(u64::MAX);
    let large_len = if cfg!(miri) { 2_048 } else { 1_000_000 };
    for length in [1, large_len] {
        let before = CHUNK_REQUESTS.with(std::cell::Cell::get);
        let mut requests_after_first = 0;
        for iteration in 0..2 {
            let mut region = Region::open(&context);
            for value in 0..length {
                let pointer = region.allocate(&context, 8, 8, Some(8)).unwrap();
                // SAFETY: this aligned slot has at least eight writable bytes.
                unsafe { pointer.cast::<u64>().write(value) };
            }
            let releases = CHUNK_RELEASES.with(std::cell::Cell::get);
            region.close();
            assert_eq!(CHUNK_RELEASES.with(std::cell::Cell::get), releases);
            assert_eq!(context.available(), u64::MAX);
            let requests = CHUNK_REQUESTS.with(std::cell::Cell::get);
            if iteration == 0 {
                assert!(requests > before);
                requests_after_first = requests;
            } else {
                assert_eq!(requests, requests_after_first);
            }
        }
    }
}

#[test]
fn region_cache_retires_backing_after_two_smaller_uses() {
    use crate::region::{CHUNK_REQUESTS, GRANULE, cached_backing};

    const LARGE: usize = 8 << 20;
    let mut all_samples = Vec::new();
    for uses in [
        &[true, false, false][..],
        &[true, true, true],
        &[true, false, true, false, true],
    ] {
        let context = Context::new(LARGE as u64 + 8);
        let mut samples = Vec::new();
        let previous_requests = CHUNK_REQUESTS.with(std::cell::Cell::get);
        for &large in uses {
            let mut region = Region::open(&context);
            region.allocate(&context, 8, 8, Some(8)).unwrap();
            if large {
                let slot = region
                    .allocate(&context, LARGE, 8, Some(LARGE as u64))
                    .unwrap();
                // SAFETY: the reservation has LARGE writable bytes until close.
                unsafe { slot.as_ptr().write_bytes(1, LARGE) };
            }
            let active = region.physical_bytes();
            region.close();
            assert_eq!(context.available(), LARGE as u64 + 8);
            let (chunks, bytes) = cached_backing(&context);
            samples.push((chunks, bytes, bytes - active));
        }
        eprintln!("{uses:?}: cached (chunks, bytes, unused bytes) = {samples:?}");
        assert_eq!(samples[0], (2, GRANULE + LARGE, 0));
        assert_eq!(
            CHUNK_REQUESTS.with(std::cell::Cell::get) - previous_requests,
            2,
        );
        all_samples.push(samples);
    }
    assert_eq!(all_samples[0].last().copied(), Some((1, GRANULE, 0)));
}

#[test]
fn region_slot_opens_only_after_admission_and_refunds_the_handed_over_payer() {
    use crate::abi::*;
    // SAFETY: the slot belongs to this context, starts empty and holds its only Region.
    unsafe {
        let ctx = hirsel_core_context_new(128);
        let mut slot = std::ptr::null_mut();
        let refusal = hirsel_core_region_slot_alloc(ctx, &mut slot, 136, 8, 136, false);
        assert!(refusal.pointer.is_null());
        assert_eq!(refusal.capacity.requested, 136);
        assert_eq!(refusal.capacity.available, 128);
        assert!(slot.is_null());

        assert!(!hirsel_core_limit_enter(ctx, 64).pointer.is_null());
        let allocation = hirsel_core_region_slot_alloc(ctx, &mut slot, 24, 8, 24, false);
        assert!(!slot.is_null());
        assert!(!allocation.pointer.is_null());
        allocation.pointer.write(73);
        hirsel_core_limit_exit(ctx);
        assert_eq!((*ctx).available(), 104);
        assert_eq!(allocation.pointer.read(), 73);
        hirsel_core_region_close(slot);
        assert_eq!((*ctx).available(), 128);
        hirsel_core_context_free(ctx);
    }
}

use crate::{Context, block::Block, region::Region};
use hirsel_core_layout::charge;

#[test]
fn physical_chunk_sizes_do_not_change_capacity_or_move_stable_bytes() {
    let run = |chunk| {
        let mut ctx = Context::new(128);
        let mut region = Region::new(chunk);
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
        let physical = region.physical_bytes();
        drop(region);
        assert_eq!(ctx.available(), 128);
        (result, physical)
    };
    let a = run(128);
    let b = run(65536);
    assert_eq!(a.0, b.0);
    assert_ne!(a.1, b.1);
}

#[test]
fn partial_refund_follows_allocation_payer_and_close_refunds_only_the_tail_balance() {
    let mut ctx = Context::new(8192);
    let mut region = Region::new(8192);
    ctx.limit_enter(4096).unwrap();
    let p = region.allocate(&ctx, 4096, 8, Some(4096)).unwrap();
    ctx.limit_exit();
    ctx.limit_enter(1024).unwrap();
    // SAFETY: the sole writer retains 80 logical bytes of its completed prefix.
    unsafe {
        Region::lower_charge(p, 80);
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
    for chunk in [128, 65536] {
        for later in [false, true] {
            for k in 1..=7 {
                let ctx = Context::new(128);
                ctx.current.inject(k, later);
                let mut region = Region::new(chunk);
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
    let mut region = Region::new(256);
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
        let region = hirsel_core_region_open(256);
        let allocation = hirsel_core_region_alloc(ctx, region, 64, 16, 64, false);
        allocation.pointer.write(73);
        hirsel_core_region_lower_charge(std::ptr::NonNull::new(allocation.pointer).unwrap(), 8);
        assert_eq!(allocation.pointer.read(), 73);
        assert_eq!((*ctx).available(), 120);
        hirsel_core_region_close(region);
        assert_eq!((*ctx).available(), 128);
        hirsel_core_context_free(ctx);
    }
}

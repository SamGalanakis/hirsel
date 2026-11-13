use crate::{Capacity, Context, abi::*, block::*, limit::Funding};
use hirsel_core_layout::charge;

#[test]
fn limits_reserve_then_handover_survivors_and_reclaim_records() {
    let mut ctx = Context::new(128);
    let root = ctx.current.clone();
    ctx.limit_enter(96).unwrap();
    let owner = Block::new(&ctx, Some(24), 24, 8).unwrap();
    ctx.limit_enter(64).unwrap();
    let inner = Block::new(&ctx, Some(16), 16, 8).unwrap();
    ctx.limit_exit();
    assert_eq!(ctx.available(), 56);
    ctx.limit_exit();
    assert_eq!(ctx.available(), 88);
    drop(owner);
    drop(inner);
    assert_eq!(root.available(), 128);
    // Union by rank kept the inner representative. The root's two handles
    // retain their original record and its edge to that representative.
    assert_eq!(root.records(), 2);
}

#[test]
fn ended_parent_with_live_child_forwards_reservations_and_compresses() {
    let root = Funding::root(1024);
    let outer = root.reserve(800).unwrap();
    let inner = outer.reserve(600).unwrap();
    let leaf = inner.reserve(400).unwrap();
    leaf.admit(Some(100)).unwrap();
    outer.end();
    inner.end();
    leaf.end();
    assert_eq!(root.available(), 924);
    leaf.refund(100);
    assert_eq!(root.available(), 1024);
    drop((outer, inner, leaf));
    assert_eq!(root.records(), 1);
    for _ in 0..1000 {
        let r = root.reserve(1).unwrap();
        r.end();
    }
    assert_eq!(root.records(), 1);
}

#[test]
fn growth_uses_original_payer_and_keeps_old_storage_on_refusal() {
    let mut ctx = Context::new(64);
    let block = Block::new(&ctx, Some(8), 8, 8).unwrap();
    let other = Block::new(&ctx, Some(24), 24, 8).unwrap();
    ctx.limit_enter(32).unwrap();
    let error = block.replacement(&ctx, Some(16), 16, 8).err().unwrap();
    assert_eq!(
        error,
        Capacity {
            requested: 16,
            available: 0
        }
    );
    assert_eq!(ctx.available(), 32);
    ctx.limit_exit();
    let replacement = block.replacement(&ctx, Some(16), 16, 64).unwrap();
    assert_eq!(replacement.pointer().as_ptr() as usize % 64, 0);
    drop((block, other, replacement));
    assert_eq!(ctx.available(), 64);
}

#[test]
fn exact_charge_overflow_refuses_even_an_empty_maximum_limit() {
    let ctx = Context::new(u64::MAX);
    assert_eq!(
        Block::new(&ctx, charge::seq(u64::MAX, 8), 1, 8).err(),
        Some(Capacity {
            requested: u64::MAX,
            available: u64::MAX
        })
    );
    assert_eq!(ctx.available(), u64::MAX);
}

#[test]
fn own_clone_prefixes_fail_at_each_and_all_later_admissions() {
    let mut runs = 0;
    for later in [false, true] {
        for k in 1..=6 {
            let ctx = Context::new(256);
            ctx.current.inject(k, later);
            let mut prefix = Vec::new();
            for n in [8, 16, 24, 32, 40] {
                match Block::new(&ctx, Some(n), n as usize, 8) {
                    Ok(b) => prefix.push(b),
                    Err(_) => break,
                }
            }
            while prefix.pop().is_some() {}
            assert_eq!(ctx.available(), 256);
            assert_eq!(ctx.current.records(), 1);
            runs += 1;
        }
    }
    assert_eq!(runs, 12);
}

#[test]
fn c_abi_admission_growth_copy_free_and_handover() {
    // SAFETY: this fixture follows the C ownership/layout contracts, including
    // initialized nonoverlapping copies and exactly one refund per allocation.
    unsafe {
        let ctx = hirsel_core_context_new(128);
        assert!(!hirsel_core_limit_enter(ctx, 64).pointer.is_null());
        let first = hirsel_core_alloc(ctx, std::ptr::null_mut(), 8, 8, 8, false);
        first.pointer.write(42);
        hirsel_core_limit_exit(ctx);
        let second = hirsel_core_alloc(ctx, first.pointer, 16, 8, 16, false);
        std::ptr::copy_nonoverlapping(first.pointer, second.pointer, 1);
        assert_eq!(second.pointer.read(), 42);
        hirsel_core_free(std::ptr::NonNull::new(first.pointer).unwrap(), 8);
        hirsel_core_free(std::ptr::NonNull::new(second.pointer).unwrap(), 16);
        assert_eq!((*ctx).available(), 128);
        hirsel_core_context_free(ctx);
    }
}

#[test]
fn owned_failure_string_outlives_its_frame_and_context_without_admission() {
    for (capacity, reserved) in [(8, true), (0, true), (0, false)] {
        // SAFETY: this fixture transfers each buffer/cell once and inspects the
        // carrier only while its delivered cause chain owns it.
        unsafe {
            let mut ctx = Context::new(1000);
            assert!(!hirsel_core_task_start(&mut ctx).pointer.is_null());
            let root = ctx.current.clone();
            ctx.limit_enter(capacity).unwrap();
            let len = if capacity == 0 { 0 } else { 3 };
            let data = if reserved {
                let allocation =
                    hirsel_core_alloc(&ctx, std::ptr::null_mut(), capacity, 1, capacity, false);
                std::ptr::copy_nonoverlapping(b"hey".as_ptr(), allocation.pointer, len);
                allocation.pointer
            } else {
                std::ptr::null_mut()
            };
            ctx.limit_exit();
            ctx.current.inject(1, true);
            hirsel_core_raise_string(&mut ctx, 77, data, len as u64, capacity);
            assert_eq!(ctx.current.attempts(), 0);
            let chain = hirsel_core_causes_take(&mut ctx);
            drop(ctx);
            let cause = &*hirsel_core_cause_value(chain);
            assert_eq!((cause.kind, cause.site), (crate::causes::FAIL, 77));
            let message = &*cause.message.cast::<crate::causes::StringMessage>();
            assert_eq!((message.len, message.capacity), (len as u64, capacity));
            if len != 0 {
                assert_eq!(std::slice::from_raw_parts(message.data, len), b"hey");
            }
            assert_eq!(root.available(), 1000 - 32 - capacity);
            hirsel_core_causes_drop(chain);
            assert_eq!(root.available(), 1000);
            assert_eq!(root.attempts(), 0);
            assert_eq!(root.records(), 1);
        }
    }
}

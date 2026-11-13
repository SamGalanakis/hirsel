use crate::{
    Capacity, Context,
    abi::*,
    block::{Block, PLAIN},
    limit::Funding,
};
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
    // Symbolic full-range bookkeeping needs no physical MAX-sized host block.
    ctx.current.admit(Some(u64::MAX)).unwrap();
    assert_eq!(ctx.available(), 0);
    ctx.current.refund(u64::MAX);
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
        let first = hirsel_core_alloc(ctx, std::ptr::null_mut(), 8, 8, 8, false, PLAIN);
        first.pointer.write(42);
        hirsel_core_limit_exit(ctx);
        let second = hirsel_core_alloc(ctx, first.pointer, 16, 8, 16, false, PLAIN);
        std::ptr::copy_nonoverlapping(first.pointer, second.pointer, 1);
        assert_eq!(second.pointer.read(), 42);
        hirsel_core_free(std::ptr::NonNull::new(first.pointer).unwrap(), 8);
        hirsel_core_free(std::ptr::NonNull::new(second.pointer).unwrap(), 16);
        assert_eq!((*ctx).available(), 128);
        hirsel_core_context_free(ctx);
    }
}

#[test]
fn zero_charge_storage_pins_its_payer_after_the_context_ends() {
    let mut ctx = Context::new(0);
    ctx.limit_enter(0).unwrap();
    let block = Block::new(&ctx, Some(0), 8, 64).unwrap();
    ctx.limit_exit();
    drop(ctx);
    // SAFETY: even uncharged native storage remains owned and aligned until drop.
    unsafe {
        block.pointer().as_ptr().write(42);
    }
    drop(block);
}

#[test]
fn native_task_context_moves_workers_with_exclusive_execution_custody() {
    struct Task {
        context: *mut Context,
        block: std::ptr::NonNull<u8>,
    }
    // SAFETY: scheduler unbinds the previous worker before transferring the task
    // and its sole owned block; no thread retains execution or payload access.
    unsafe impl Send for Task {}
    impl Task {
        fn run(self) {
            // SAFETY: the fixture is the sole scheduler and follows native custody.
            unsafe {
                hirsel_core_task_bind(self.context);
                hirsel_core_free(self.block, 8);
                assert_eq!((*self.context).available(), 64);
                hirsel_core_context_free(self.context);
            }
        }
    }
    // SAFETY: one native context and one plain allocation are transferred once.
    unsafe {
        let context = hirsel_core_context_new(64);
        let result = hirsel_core_alloc(context, std::ptr::null_mut(), 8, 8, 8, false, PLAIN);
        let task = Task {
            context,
            block: std::ptr::NonNull::new(result.pointer).unwrap(),
        };
        hirsel_core_task_bind(std::ptr::null());
        std::thread::spawn(move || task.run()).join().unwrap();
    }
}

#[test]
fn frozen_prefixes_match_the_emitter_contract() {
    use crate::region::Region;
    assert_eq!(
        std::mem::offset_of!(Context, current),
        CONTEXT_CURRENT_LIMIT
    );
    assert_eq!(std::mem::offset_of!(Context, events), CONTEXT_EVENTS);
    assert_eq!(
        std::mem::offset_of!(crate::limit::LimitRecord, available),
        LIMIT_AVAILABLE
    );
    assert_eq!(
        std::mem::offset_of!(crate::limit::LimitRecord, outstanding),
        LIMIT_OUTSTANDING
    );
    assert_eq!(std::mem::offset_of!(Region, cursor), REGION_CURSOR);
    assert_eq!(std::mem::offset_of!(Region, end), REGION_END);
    assert_eq!(std::mem::offset_of!(Region, entry), REGION_ENTRY);
    assert_eq!(std::mem::offset_of!(Region, payer), REGION_PAYER);
    assert_eq!(std::mem::offset_of!(Region, charge), REGION_CHARGE);
    assert_eq!(
        crate::block::GRAPH_PREFIX_BYTES,
        crate::abi_layout::GRAPH_PREFIX_BYTES
    );
    use crate::causes::{Cause, Cell, StringMessage};
    assert_eq!(size_of::<Cause>(), CAUSE_BYTES);
    assert_eq!(std::mem::offset_of!(Cause, kind), CAUSE_KIND);
    assert_eq!(std::mem::offset_of!(Cause, message), CAUSE_MESSAGE);
    assert_eq!(
        CAUSE_MESSAGE + std::mem::offset_of!(StringMessage, data),
        CAUSE_MESSAGE_DATA
    );
    assert_eq!(
        CAUSE_MESSAGE + std::mem::offset_of!(StringMessage, len),
        CAUSE_MESSAGE_LENGTH
    );
    assert_eq!(
        CAUSE_MESSAGE + std::mem::offset_of!(StringMessage, capacity),
        CAUSE_MESSAGE_CAPACITY
    );
    assert_eq!(std::mem::offset_of!(Cause, site), CAUSE_SITE);
    assert_eq!(std::mem::offset_of!(Cell, cause), CAUSE_CELL_VALUE);
    assert_eq!(std::mem::offset_of!(Cell, next), CAUSE_CELL_NEXT);
    let ctx = Context::new(256);
    for alignment in [8, 16, 64, 512, 1024, 4096] {
        for graph in [false, true] {
            let block = if graph {
                Block::graph(&ctx, Some(128), 128, alignment)
            } else {
                Block::new(&ctx, Some(128), 128, alignment)
            }
            .unwrap();
            // SAFETY: the owner pins all 128 writable, aligned bytes.
            unsafe {
                block.pointer().as_ptr().write_bytes(0, 128);
                assert_eq!(
                    std::slice::from_raw_parts(block.pointer().as_ptr(), 128),
                    [0; 128]
                );
            }
            assert_eq!(block.pointer().as_ptr().addr() % alignment, 0);
            drop(block);
            assert_eq!(ctx.available(), 256);
        }
    }
    let mut region = Region::new(64);
    region.allocate(&ctx, 8, 8, Some(8)).unwrap();
    assert_eq!(ctx.available(), 248);
    drop(region);
    assert_eq!(ctx.available(), 256);
}

#[test]
fn emitted_own_sequence_refunds_current_foreign_and_handed_over_payers() {
    let mut ctx = Context::new(128);
    // SAFETY: fixture owns every returned byte block and passes its exact operands.
    unsafe {
        let outer = inline_own_alloc(&ctx, Some(16), 16, 64).unwrap();
        ctx.limit_enter(64).unwrap();
        let inner = inline_own_alloc(&ctx, Some(24), 24, 8).unwrap();
        inline_own_free(&ctx, outer, 16, 16, 64);
        assert_eq!(ctx.available(), 40);
        ctx.limit_exit();
        inline_own_free(&ctx, inner, 24, 24, 8);
        assert_eq!(ctx.available(), 128);
        let current = inline_own_alloc(&ctx, Some(8), 8, 8).unwrap();
        inline_own_free(&ctx, current, 8, 8, 8);
        assert_eq!(ctx.available(), 128);
    }
    // A survivor pins its original slot across exit/re-entry. Its refund must
    // reach the parent, not the newly entered current scope.
    ctx.limit_enter(64).unwrap();
    // SAFETY: this fixture owns the exact plain allocation and its payer word.
    let survivor = unsafe { inline_own_alloc(&ctx, Some(24), 24, 8).unwrap() };
    ctx.limit_exit();
    assert_eq!(ctx.available(), 104);
    ctx.limit_enter(32).unwrap();
    // SAFETY: same live block; the new scope cannot reuse a byte-retained slot.
    unsafe {
        let payer = *crate::block::funding(survivor);
        assert_ne!(payer.record_pointer(), ctx.limit_record());
        inline_own_free(&ctx, survivor, 24, 24, 8);
    }
    assert_eq!(ctx.available(), 32);
    ctx.limit_exit();
    assert_eq!(ctx.available(), 128);
    // The C refund hook implements the same test for emitted native frees.
    // SAFETY: we retain the block while settling, then release its physical base.
    unsafe {
        let block = inline_own_alloc(&ctx, Some(8), 8, 8).unwrap();
        let word = block
            .as_ptr()
            .sub(8)
            .cast::<*mut crate::limit::LimitRecord>()
            .read();
        hirsel_core_refund_in(&ctx, word, 8);
        hirsel_core_native_free(block.as_ptr().sub(8));
    }
    assert_eq!(ctx.available(), 128);
}

#[test]
fn owned_failure_string_outlives_its_frame_and_context_without_admission() {
    for (capacity, len) in [(8, 3), (0, 3), (0, 0)] {
        // SAFETY: this fixture transfers each buffer/cell once and inspects the
        // carrier only while its delivered cause chain owns it.
        unsafe {
            let mut ctx = Context::new(1000);
            assert!(!hirsel_core_task_start(&mut ctx).pointer.is_null());
            let root = ctx.current.clone();
            ctx.limit_enter(capacity).unwrap();
            let data = if capacity != 0 {
                let allocation = hirsel_core_alloc(
                    &ctx,
                    std::ptr::null_mut(),
                    capacity as usize,
                    1,
                    capacity,
                    false,
                    PLAIN,
                );
                std::ptr::copy_nonoverlapping(b"hey".as_ptr(), allocation.pointer, len);
                allocation.pointer
            } else {
                b"hey".as_ptr().cast_mut()
            };
            ctx.limit_exit();
            ctx.current.inject(1, true);
            hirsel_core_raise_string(&mut ctx, 77, data, len as u64, capacity);
            assert_eq!(ctx.current.attempts(), 0);
            let chain = hirsel_core_causes_take(&mut ctx);
            drop(ctx);
            let cause = &*hirsel_core_cause_value(chain);
            assert_eq!((cause.kind, cause.site), (crate::causes::FAIL, 77));
            let message = &cause.message;
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

#[test]
fn original_payer_growth_outlives_its_last_context_and_old_block() {
    let source = Context::new(48);
    let old = Block::new(&source, Some(8), 8, 8).unwrap();
    drop(source);
    let receiver = Context::new(0);
    let next = old.replacement(&receiver, Some(16), 16, 8).unwrap();
    drop(old);
    let last = next.replacement(&receiver, Some(24), 24, 8).unwrap();
    drop(next);
    drop(last);
    assert_eq!(receiver.available(), 0);
}

#[test]
fn current_payer_free_refunds_without_a_header_lookup() {
    let ctx = Context::new(64);
    for align in [8, 16, 64] {
        // SAFETY: no limit transition or handover occurs between this pair.
        unsafe {
            let block = inline_own_alloc(&ctx, Some(24), 24, align).unwrap();
            assert_eq!(ctx.available(), 40);
            inline_own_free_current(&ctx, block, 24, 24, align);
        }
        assert_eq!(ctx.available(), 64);
    }
    // SAFETY: zero charge still owns a payer pin; the helper settles it.
    unsafe {
        let block = inline_own_alloc(&ctx, Some(0), 1, 8).unwrap();
        inline_own_free_current(&ctx, block, 0, 1, 8);
    }
}

#[test]
fn static_capacity_zero_carriers_drop_and_grow_through_runtime_cleanup() {
    static LITERAL: [u8; 5] = *b"hello";
    let mut ctx = Context::new(1000);
    // SAFETY: static bytes are read only. The emitted growth sequence tests
    // capacity, selects current rather than Like, and never frees static data.
    unsafe {
        let mut message = crate::causes::StringMessage {
            data: LITERAL.as_ptr().cast_mut(),
            len: 5,
            capacity: 0,
        };
        assert_eq!(
            hirsel_core_compare_bytes(message.data, message.len, b"hello".as_ptr(), 5),
            0
        );
        let needed = message.len.checked_add(1).unwrap();
        assert!(needed > message.capacity);
        let before = ctx.available();
        let next = hirsel_core_alloc(
            &ctx,
            std::ptr::null_mut(),
            needed as usize,
            1,
            8,
            false,
            PLAIN,
        );
        assert!(!next.pointer.is_null());
        std::ptr::copy_nonoverlapping(message.data, next.pointer, message.len as usize);
        next.pointer.add(message.len as usize).write(b'X');
        message = crate::causes::StringMessage {
            data: next.pointer,
            len: needed,
            capacity: needed,
        };
        assert_eq!(
            hirsel_core_compare_bytes(message.data, message.len, LITERAL.as_ptr(), 5),
            1
        );
        assert_eq!(&LITERAL, b"hello");
        hirsel_core_free(std::ptr::NonNull::new_unchecked(message.data), 8);
        assert_eq!(ctx.available(), before);
        // Empty static data must not be mistaken for a prepaid zero-charge block.
        assert!(!hirsel_core_task_start(&mut ctx).pointer.is_null());
        let before = ctx.available();
        hirsel_core_raise_string(&mut ctx, 48, LITERAL.as_ptr().cast_mut(), 0, 0);
        let chain = hirsel_core_causes_take(&mut ctx);
        hirsel_core_causes_drop(chain);
        assert_eq!(ctx.available(), before + 32);
    }
}

#[test]
fn ordinary_native_words_and_balances_match_the_inline_contract() {
    use crate::abi_layout::*;
    let ctx = Context::new(4096);
    for (kind, prefix, tag) in [
        (
            crate::block::PLAIN,
            PLAIN_PREFIX_BYTES,
            OWN_ORDINARY_PLAIN_TAG,
        ),
        (
            crate::block::GRAPH,
            GRAPH_PREFIX_BYTES,
            OWN_ORDINARY_GRAPH_TAG,
        ),
    ] {
        for bytes in [0, 1, 128, 512] {
            let charge = hirsel_core_layout::charge::seq(bytes, 1).unwrap();
            // SAFETY: exact native layout and charge, exclusive cleaned payload.
            unsafe {
                let allocation = hirsel_core_alloc(
                    &ctx,
                    std::ptr::null_mut(),
                    bytes as usize,
                    8,
                    charge,
                    false,
                    kind,
                );
                assert!(!allocation.pointer.is_null());
                let raw = allocation
                    .pointer
                    .sub(OWN_PAYER_BACK)
                    .cast::<usize>()
                    .read();
                assert_eq!(raw ^ ctx.limit_record().addr(), tag);
                assert!(tag <= OWN_ORDINARY_MAX_TAG);
                assert_eq!(ctx.available(), 4096 - charge);
                if charge != 0 {
                    let record = &mut *ctx.limit_record();
                    record.available += charge;
                    record.outstanding -= charge;
                    hirsel_core_native_free(allocation.pointer.sub(prefix));
                } else {
                    hirsel_core_free_value(
                        std::ptr::NonNull::new_unchecked(allocation.pointer),
                        charge,
                    );
                }
                assert_eq!(ctx.available(), 4096);
            }
        }
    }
}

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
fn one_survivor_misses_the_empty_scope_fast_path_and_keeps_the_refund() {
    let mut ctx = Context::new(128);
    ctx.limit_enter(64).unwrap();
    let survivor = Block::new(&ctx, Some(8), 8, 8).unwrap();
    assert_eq!(ctx.available(), 56);
    ctx.limit_exit();
    assert_eq!(ctx.available(), 120);
    assert_eq!(ctx.current.scope_debug(), (2, 2, 1, 1, 8));
    drop(survivor);
    assert_eq!(ctx.available(), 128);
}

#[test]
fn empty_scope_fast_path_refunds_unused_bytes() {
    let mut ctx = Context::new(128);
    for _ in 0..64 {
        ctx.limit_enter(64).unwrap();
        assert_eq!(ctx.available(), 64);
        ctx.limit_exit();
        assert_eq!(ctx.available(), 128);
    }
    assert_eq!(ctx.current.records(), 1);
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
fn growth_uses_current_room_and_keeps_old_storage_on_refusal() {
    let mut ctx = Context::new(64);
    let block = Block::new(&ctx, Some(8), 8, 8).unwrap();
    let other = Block::new(&ctx, Some(24), 24, 8).unwrap();
    ctx.limit_enter(8).unwrap();
    let error = block.replacement(&ctx, Some(16), 16, 8).err().unwrap();
    assert_eq!(
        error,
        Capacity {
            requested: 16,
            available: 8
        }
    );
    assert_eq!(ctx.available(), 8);
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
        std::mem::offset_of!(crate::tasks::Task, arc_runtime),
        TASK_ARC_REPLAY
    );
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
    assert_eq!(CAUSE_KIND, CAUSE_MESSAGE_CAPACITY);
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
    let mut region = Region::open(&ctx);
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
        let payer = hirsel_core_block_payer(block);
        hirsel_core_refund_in(&ctx, payer, 8);
        crate::task_heap::release(&ctx, block);
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
            assert_eq!((cause.kind(), cause.site), (crate::causes::FAIL, 77));
            let message = cause.message().unwrap();
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
fn growth_refunds_a_retired_payer_and_keeps_the_receiver_live() {
    let source = Context::new(48);
    let old = Block::new(&source, Some(8), 8, 8).unwrap();
    drop(source);
    let receiver = Context::new(40);
    let next = old.replacement(&receiver, Some(16), 16, 8).unwrap();
    assert_eq!(receiver.available(), 24);
    drop(old);
    let last = next.replacement(&receiver, Some(24), 24, 8).unwrap();
    assert_eq!(receiver.available(), 0);
    drop(next);
    drop(last);
    assert_eq!(receiver.available(), 40);
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
fn page_funding_and_balances_match_the_inline_contract() {
    let ctx = Context::new(4096);
    for kind in [crate::block::PLAIN, crate::block::GRAPH] {
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
                let pointer = std::ptr::NonNull::new(allocation.pointer).unwrap();
                assert_eq!(hirsel_core_block_payer(pointer), ctx.limit_record());
                assert_eq!(crate::task_heap::kind(pointer), kind);
                assert_eq!(ctx.available(), 4096 - charge);
                hirsel_core_free_value(&ctx, pointer, charge);
                assert_eq!(ctx.available(), 4096);
            }
        }
    }
}

#[test]
fn leaf_buffer_growth_preserves_plain_kind_and_installs_current_payer() {
    let mut context = Context::new(128);
    let original = crate::block::Block::new(&context, Some(8), 8, 8).unwrap();
    // SAFETY: fixture owns eight fresh writable payload bytes.
    unsafe { original.pointer().as_ptr().write_bytes(17, 8) };
    context.limit_enter(24).unwrap();
    let grown = original.replacement(&context, Some(24), 24, 8).unwrap();
    // SAFETY: both allocations pin their page metadata.
    unsafe {
        assert_eq!(
            hirsel_core_block_payer(grown.pointer()),
            context.limit_record()
        );
        assert_eq!(crate::task_heap::kind(grown.pointer()), crate::block::PLAIN);
        assert_eq!(
            crate::task_heap::kind(original.pointer()),
            crate::block::PLAIN
        );
        std::ptr::copy_nonoverlapping(original.pointer().as_ptr(), grown.pointer().as_ptr(), 8);
        assert_eq!(grown.pointer().as_ptr().read(), 17);
    }
    assert_eq!(context.available(), 0);
    context.limit_exit();
    drop(original);
    assert_eq!(context.available(), 104);
    drop(grown);
    assert_eq!(context.available(), 128);
}

/// C readers (recorders, harnesses, provider shims) take event kinds from the
/// header; a kind added, renumbered or doubled on one side only would make them
/// read one kind as another (as TRY_RECEIVE once read as ARC_NEW).
#[test]
fn the_header_numbers_every_event_kind_as_the_table_does() {
    let header = include_str!("../include/hirsel_core_rt_host.h");
    let mut published: Vec<(&str, u8)> = header
        .split("HIRSEL_CORE_EVENT_")
        .skip(1)
        .map(|entry| {
            let (name, rest) = entry.split_once('=').expect("NAME=number");
            let digits = rest.trim_start();
            let end = digits
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(digits.len());
            (name, digits[..end].parse().expect("a kind number"))
        })
        .collect();
    let mut table = hirsel_core_layout::event::ALL.to_vec();
    published.sort();
    table.sort();
    assert_eq!(published, table);
}

fn reexecute_heap_test(name: &str, child_flag: &str) -> bool {
    if std::env::var_os(child_flag).is_some() {
        return false;
    }
    let output = crate::test_process::command()
        .args(["--exact", name])
        .env(child_flag, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child output: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    true
}

#[cfg(target_os = "linux")]
#[test]
fn shared_page_keeps_other_heaps_live_after_a_foreign_return() {
    if reexecute_heap_test(
        "tests::shared_page_keeps_other_heaps_live_after_a_foreign_return",
        "HIRSEL_SHARED_PAGE_TEST_CHILD",
    ) {
        return;
    }

    const LIMIT: u64 = 17;
    use crate::task_heap::{page_bytes, purge};
    let native_page = |block: &Block| block.pointer().addr().get() / page_bytes();
    let mut clients: Vec<(Context, Block)> = Vec::new();
    let (first, second) = loop {
        assert!(clients.len() <= 16, "no two heaps shared a page");
        let context = Context::new(LIMIT);
        let block = Block::new(&context, Some(LIMIT), 24, 8).unwrap();
        if let Some(index) = clients
            .iter()
            .position(|(_, other)| native_page(other) == native_page(&block))
        {
            break (clients.swap_remove(index), (context, block));
        }
        clients.push((context, block));
    };
    let (first, returned) = first;
    let (second, survivor) = second;
    let page = native_page(&survivor) * page_bytes();
    assert!(*survivor.payer() == second.payer());
    assert!(*returned.payer() == first.payer());
    // SAFETY: the survivor owns 24 writable bytes.
    unsafe { survivor.pointer().cast::<u64>().write(17) };
    let address = returned.pointer().as_ptr().expose_provenance();
    std::mem::forget(returned);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let pointer =
                std::ptr::NonNull::new(std::ptr::with_exposed_provenance_mut(address)).unwrap();
            // SAFETY: this thread takes the unique block and its exact charge.
            drop(unsafe { Block::from_raw(pointer, LIMIT) });
        });
    });
    first.current.drain();
    assert_eq!(first.available(), LIMIT);
    drop(first);
    purge();

    let mut flag = 0;
    // SAFETY: mincore checks the mapping without reading the survivor.
    let mapped = unsafe { libc::mincore(page as *mut _, page_bytes(), &mut flag) } == 0;
    if !mapped || flag & 1 == 0 {
        // A broken lease has invalidated this block and its owning heap.
        // Skip their destructors in this isolated child before reporting it.
        std::mem::forget((second, survivor));
        panic!("the shared page was discarded with a live block");
    }
    assert_eq!(second.available(), 0);
    // SAFETY: the neighbor's return and purge left the live block mapped.
    assert_eq!(unsafe { survivor.pointer().cast::<u64>().read() }, 17);
    drop(survivor);
    assert_eq!(second.available(), LIMIT);
}

fn pause_foreign_return(
    block: Block,
) -> (
    std::sync::Arc<std::sync::Barrier>,
    std::thread::JoinHandle<()>,
) {
    let charge = block.charge();
    let address = block.into_raw().as_ptr().expose_provenance();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let peer = gate.clone();
    let foreign = std::thread::spawn(move || {
        crate::task_heap::before_foreign_return(move || {
            peer.wait();
            peer.wait();
        });
        let pointer =
            std::ptr::NonNull::new(std::ptr::with_exposed_provenance_mut(address)).unwrap();
        // SAFETY: this thread owns the transferred slot and its exact charge.
        unsafe { hirsel_core_free(pointer, charge) };
    });
    gate.wait();
    (gate, foreign)
}

#[test]
fn migrated_context_keeps_the_slot_of_an_in_flight_foreign_return() {
    const LIMIT: u64 = 64;
    const CHARGE: u64 = 8;
    const BYTES: usize = 32 * 1024;
    struct Custody(*mut Context);
    // SAFETY: only the receiving worker uses the context after native unbind.
    unsafe impl Send for Custody {}
    impl Custody {
        unsafe fn resume(
            self,
            saved: usize,
            gate: std::sync::Arc<std::sync::Barrier>,
            foreign: std::thread::JoinHandle<()>,
        ) {
            // SAFETY: the original worker unbound the context before sending it.
            unsafe {
                hirsel_core_task_bind(self.0);
                let context = &*self.0;
                let refunded = context.available();
                let other = Block::new(context, Some(CHARGE), BYTES, 8).unwrap();
                let unpublished_was_reused = other.pointer().addr().get() == saved;
                gate.wait();
                foreign.join().unwrap();
                assert_eq!(refunded, LIMIT, "migration left the original refund behind");
                assert!(
                    !unpublished_was_reused,
                    "migration reused an unpublished slot"
                );
                let replacement = Block::new(context, Some(CHARGE), BYTES, 8).unwrap();
                assert_eq!(
                    replacement.pointer().addr().get(),
                    saved,
                    "the new owner lost the published return",
                );
                assert_eq!(context.available(), LIMIT - 2 * CHARGE);
                drop((other, replacement));
                assert_eq!(context.available(), LIMIT, "the charge was refunded twice");
                hirsel_core_task_bind(std::ptr::null());
                hirsel_core_context_free(self.0);
            }
        }
    }
    // SAFETY: exclusive native context custody moves once. The foreign return
    // receives only the live block and may continue while the context migrates.
    unsafe {
        let context = Custody(hirsel_core_context_new(LIMIT));
        let block = Block::new(&*context.0, Some(CHARGE), BYTES, 8).unwrap();
        let saved = block.pointer().addr().get();
        let (gate, foreign) = pause_foreign_return(block);
        hirsel_core_task_bind(std::ptr::null());
        std::thread::spawn(move || context.resume(saved, gate, foreign))
            .join()
            .unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn close_hands_an_in_flight_foreign_return_to_idle_purge() {
    if reexecute_heap_test(
        "tests::close_hands_an_in_flight_foreign_return_to_idle_purge",
        "HIRSEL_FOREIGN_CLOSE_TEST_CHILD",
    ) {
        return;
    }
    const LIMIT: u64 = 64;
    let context = Context::new(LIMIT);
    let block = Block::new(&context, Some(LIMIT), 128 * 1024, 8).unwrap();
    let address = block.pointer().addr().get();
    let mapped = || {
        let mut flag = 0;
        // SAFETY: mincore checks the mapping without accessing the payload.
        unsafe { libc::mincore(address as *mut _, crate::task_heap::page_bytes(), &mut flag) == 0 }
    };
    let (gate, foreign) = pause_foreign_return(block);
    context.current.drain();
    let refunded = context.available();
    drop(context);
    let mapped_while_publishing = mapped();
    gate.wait();
    foreign.join().unwrap();
    assert_eq!(
        refunded, LIMIT,
        "close saw the charge after physical publication"
    );
    assert!(
        mapped_while_publishing,
        "close unmapped the publisher's slot"
    );
    crate::task_heap::purge();
    assert!(!mapped(), "the return to a closed heap escaped reclamation");
}

#[test]
fn same_size_own_blocks_reuse_mappings_and_pack_small_slots() {
    // This law observes a cold mapping before measuring reuse. Run it apart
    // from other tests whose heaps can share a mapped slab range.
    if reexecute_heap_test(
        "tests::same_size_own_blocks_reuse_mappings_and_pack_small_slots",
        "HIRSEL_COLD_SLAB_TEST_CHILD",
    ) {
        return;
    }
    use crate::task_heap::MAP_REQUESTS;
    let mut context = Context::new(1_000_000);
    let before = MAP_REQUESTS.with(std::cell::Cell::get);
    let mut requests_after_first = 0;
    for iteration in 0..3 {
        let blocks: Vec<_> = (0..6_000)
            .map(|_| Block::new(&context, Some(24), 24, 8).unwrap())
            .collect();
        let first = blocks[0].pointer().addr().get();
        let second = blocks[1].pointer().addr().get();
        assert_eq!(second.abs_diff(first), 24);
        drop(blocks);
        assert_eq!(context.available(), 1_000_000);
        let requests = MAP_REQUESTS.with(std::cell::Cell::get);
        if iteration == 0 {
            assert!(requests > before);
            requests_after_first = requests;
        } else {
            assert_eq!(requests, requests_after_first);
        }
    }

    // Two payers have partial pages of the same class above 256 bytes.
    // The newest page belongs to the inner payer, even after forwarding.
    let outer = Block::new(&context, Some(24), 320, 8).unwrap();
    context.limit_enter(64).unwrap();
    let inner = Block::new(&context, Some(24), 320, 8).unwrap();
    context.limit_exit();
    let sibling = Block::new(&context, Some(24), 320, 8).unwrap();
    assert_eq!(
        sibling.pointer().addr().get() - outer.pointer().addr().get(),
        320
    );
    assert_eq!(context.available(), 1_000_000 - 72);
    drop((outer, inner, sibling));
    assert_eq!(context.available(), 1_000_000);

    let large = Block::new(&context, Some(24), 131_072, 131_072).unwrap();
    let address = large.pointer().addr();
    drop(large);
    let large = Block::new(&context, Some(24), 131_072, 131_072).unwrap();
    assert_eq!(large.pointer().addr(), address);
}

#[test]
fn cross_task_slots_return_to_the_allocating_heap_and_survive_its_exit() {
    use crate::task_heap::MAP_REQUESTS;
    let context = Context::new(4096);
    let block = Block::new(&context, Some(24), 24, 8).unwrap();
    let original = block.pointer().as_ptr().expose_provenance();
    let mut requests = MAP_REQUESTS.with(std::cell::Cell::get);
    // SAFETY: the child receives the only owning pointer and its exact charge.
    std::mem::forget(block);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let pointer =
                std::ptr::NonNull::new(std::ptr::with_exposed_provenance_mut(original)).unwrap();
            // SAFETY: this child now owns the slot and its original charge.
            drop(unsafe { Block::from_raw(pointer, 24) });
        });
    });
    context.current.drain();
    assert_eq!(context.available(), 4096);
    let block = Block::new(&context, Some(24), 24, 8).unwrap();
    assert_eq!(block.pointer().addr().get(), original);
    assert_eq!(MAP_REQUESTS.with(std::cell::Cell::get), requests);
    let survivor = Block::new(&context, Some(32), 131_072, 8192).unwrap();
    assert_eq!(survivor.pointer().addr().get() % 8192, 0);
    requests += 1;
    assert_eq!(MAP_REQUESTS.with(std::cell::Cell::get), requests);
    // SAFETY: the still-live large allocation has this aligned writable word.
    unsafe { survivor.pointer().cast::<u64>().write(17) };
    drop(context);
    // SAFETY: the transferred payload keeps its physical mapping alive.
    assert_eq!(unsafe { survivor.pointer().cast::<u64>().read() }, 17);
    drop((survivor, block));
}

/// Releasing most slots of a spread page from outside its task trims the
/// page: native pages without a live slot go back to the host, and the free
/// list is rebuilt below the highest live slot. A trim that returned a page
/// under a live slot, or threaded a live or straddling slot, would zero a
/// survivor or hand out a slot twice.
#[test]
fn trimmed_slab_pages_keep_survivors_and_hand_each_slot_out_once() {
    const WORDS: usize = 5;
    let context = Context::new(10_000_000);
    let page = crate::task_heap::page_bytes();
    let write = |block: &Block, value: u64| {
        for word in 0..WORDS {
            // SAFETY: a live 40-byte block has five aligned writable words.
            unsafe {
                block
                    .pointer()
                    .cast::<u64>()
                    .add(word)
                    .write(value + word as u64)
            };
        }
    };
    let read = |block: &Block, value: u64| {
        for word in 0..WORDS {
            // SAFETY: as above.
            assert_eq!(
                unsafe { block.pointer().cast::<u64>().add(word).read() },
                value + word as u64
            );
        }
    };
    let fill = |count: u64, base: u64| -> Vec<_> {
        (0..count)
            .map(|index| {
                let block = Block::new(&context, Some(24), 40, 8).unwrap();
                write(&block, base + 8 * index);
                block
            })
            .collect()
    };
    // Survivors straddle every third native-page boundary, so the pages
    // between them hold no live slot.
    let mut kept = Vec::new();
    for (index, block) in fill(6_000, 0).into_iter().enumerate() {
        let start = block.pointer().addr().get();
        let boundary = (start + 8 * WORDS - 1) / page;
        if boundary != start / page && boundary % 3 == 0 {
            kept.push((8 * index as u64, block));
        }
    }
    assert!(kept.len() > 10);
    let refill = fill(6_000, 1 << 32);
    for (value, block) in &kept {
        read(block, *value);
    }
    for (index, block) in refill.iter().enumerate() {
        read(block, (1 << 32) + 8 * index as u64);
    }
    let mut addresses: Vec<_> = kept
        .iter()
        .map(|(_, block)| block.pointer().addr().get())
        .chain(refill.iter().map(|block| block.pointer().addr().get()))
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    assert_eq!(addresses.len(), kept.len() + refill.len());
    drop((kept, refill));
    assert_eq!(context.available(), 10_000_000);
}

/// A task heap keeps an emptied page at its class queue head for the next
/// block. Closing the heap must unmap that page too, or every finished task
/// that freed its last small block leaks a slab.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn closed_task_heaps_unmap_their_emptied_pages() {
    if reexecute_heap_test(
        "tests::closed_task_heaps_unmap_their_emptied_pages",
        "CLOSED_TASK_HEAPS_TEST_CHILD",
    ) {
        return;
    }
    use crate::task_heap::MAP_REQUESTS;
    {
        let before = MAP_REQUESTS.with(std::cell::Cell::get);
        let context = Context::new(4096);
        let mut blocks: Vec<_> = (0..65)
            .map(|_| Block::new(&context, Some(24), 32 * 1024, 8).unwrap())
            .collect();
        let requests = MAP_REQUESTS.with(std::cell::Cell::get) - before;
        assert!(
            (1..=2).contains(&requests),
            "65 cold slabs made {requests} mapping requests"
        );
        assert_eq!(context.available(), 4096 - 65 * 24);

        // Leave one slot live in the first range while its neighbors return.
        let survivor = blocks.swap_remove(1);
        // SAFETY: the survivor exclusively owns this byte.
        unsafe { survivor.pointer().as_ptr().write(17) };
        drop(context);
        drop(blocks);
        // SAFETY: the transferred survivor still pins its slot and its range.
        assert_eq!(unsafe { survivor.pointer().as_ptr().read() }, 17);
        drop(survivor);
    }

    let mapped_kib = || {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmSize:"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap()
    };
    let task = || {
        let context = Context::new(64);
        drop(Block::new(&context, Some(24), 24, 8).unwrap());
        drop(Block::new(&context, Some(24), 200, 8).unwrap());
        assert_eq!(context.available(), 64);
    };
    task();
    let before = mapped_kib();
    for _ in 0..1000 {
        task();
    }
    // Leaking both 64 KiB slabs per task would map 128000 KiB more.
    let grown = mapped_kib() - before;
    assert!(grown < 8 * 1024, "{grown} KiB stayed mapped");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn repeated_sequence_growth_keeps_peak_rss_bounded() {
    const CHILD: &str = "REPEATED_SEQUENCE_GROWTH_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = crate::test_process::command()
            .args([
                "--exact",
                "tests::repeated_sequence_growth_keeps_peak_rss_bounded",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child output: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }

    let peak_kib = || {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap()
    };
    #[repr(C)]
    struct Carrier {
        data: *mut u8,
        len: u64,
        capacity: u64,
    }

    const START: usize = 16 * 1024;
    const END: usize = 128 * 1024;
    let context = Context::new((START + END) as u64);
    let document = || {
        let payer = context.payer();
        crate::block::admit_storage(payer, Some(START as u64)).unwrap();
        let heap = context.heap.get_or_init(std::sync::Arc::default);
        let initial = crate::task_heap::Heap::allocate(&context, payer, START, 8, PLAIN, true);
        let mut carrier = Carrier {
            data: initial.as_ptr(),
            len: 0,
            capacity: START as u64,
        };
        // SAFETY: the fixture owns the sequence backing and passes its exact
        // charge and layout. The initialized prefix is empty.
        unsafe {
            let grown = crate::growth::replace(
                &context,
                (&raw mut carrier).cast(),
                END,
                8,
                END as u64,
                END as u64,
                0,
                START as u64,
                false,
                PLAIN,
                true,
            )
            .unwrap();
            grown.as_ptr().write_bytes(0x5a, END);
            std::hint::black_box(grown);
            crate::block::free(grown, END as u64);
        }
        assert_eq!(context.available(), (START + END) as u64);
    };
    document();
    let first = peak_kib();
    for _ in 0..64 {
        document();
    }
    let repeated = peak_kib();
    assert!(
        repeated - first <= 16 * END / 1024,
        "peak RSS grew by {} document sizes",
        (repeated - first) * 1024 / END
    );
}

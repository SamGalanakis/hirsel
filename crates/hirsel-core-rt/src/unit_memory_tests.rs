use super::*;
use crate::units::{
    here,
    launch::launch_planned,
    plan::{HostEnvelope, ReactorChoice, UntiedPlan},
};

const MIB: usize = 1 << 20;

fn plan() -> UntiedPlan {
    UntiedPlan::derive(
        1,
        1,
        32 * MIB as u64,
        HostEnvelope::UNLIMITED,
        ReactorChoice::Readiness,
    )
    .unwrap()
}

fn run(plan: UntiedPlan, body: impl Fn() + Sync) {
    let pollers = hirsel_core_net::slot_readiness_pollers(1, here).unwrap();
    // SAFETY: the pool runs a Rust fixture with no capability operands.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    assert_eq!(
        launch_planned(
            plan,
            pollers,
            &config,
            &|_| {
                body();
                0
            },
            0
        )
        .unwrap(),
        0
    );
}

#[test]
fn shared_retention_cross_stock_eviction_and_idle_purge() {
    if crate::test_process::run_isolated(
        "units::memory::tests::shared_retention_cross_stock_eviction_and_idle_purge",
    ) {
        return;
    }
    let plan = plan();
    assert_eq!(plan.retained_bytes, 4 * MIB);
    run(plan, || {
        let memory = &super::super::pool().memory;
        let stack = memory.take_task_stack();
        #[inline(never)]
        fn grow(frames: usize) {
            let mut frame = [0u8; 16 * 1024];
            std::hint::black_box(&mut frame);
            if frames > 1 {
                grow(frames - 1);
            }
            std::hint::black_box(&frame);
        }
        // SAFETY: the unused task stack is ours. No reference leaves grow.
        unsafe {
            stack.call(|| grow(128));
        }
        assert!(stack.committed_bytes() >= 2 * MIB);
        let mut fibers = crate::fiber::Stock::default();
        fibers.recycle(stack);
        assert_eq!(
            memory.retention.lock().unwrap().used,
            crate::fiber::COMMIT + crate::task_heap::page_bytes()
        );

        for _ in 0..3 {
            memory.put_unit_stack(UnitStack::new(plan.unit_stack_bytes));
        }
        let slabs = (0..64)
            .map(|_| crate::task_heap::slabs::Slab::allocate(0).0)
            .collect::<Vec<_>>();
        for slab in slabs {
            memory.put_slab(slab);
        }
        assert_eq!(memory.retention.lock().unwrap().used, 4 * MIB);
        assert!(
            memory.cached_unit_stack().is_none(),
            "oldest unit stacks must give way to slabs"
        );
        assert_eq!(memory.retention.lock().unwrap().peak, 4 * MIB);

        // An emptied dedicated extent is shared stock as well.
        use crate::{Context, block, task_heap::Heap};
        let context = Context::with_unit_heap(MIB as u64);
        let bytes = 128 * 1024;
        let payer = context.payer();
        block::admit_storage(payer, Some(bytes as u64)).unwrap();
        let ptr = Heap::allocate(&context, payer, bytes, 8, block::PLAIN, true);
        // SAFETY: this context owns the live allocation.
        unsafe { block::free_in(&context, ptr, bytes as u64) };
        assert!(memory.has_retained());

        // This is later than the pool's 100 ms idle release. The one worker
        // is asleep with no runnable unit until this timer fires.
        super::super::park(Some(super::super::now() + 200_000_000));
        assert!(!memory.has_retained());
        assert!(!crate::task_heap::has_retaining());
    });
}

#[test]
fn reclaim_queue_byte_bound_and_sync_fallback() {
    let mut plan = plan();
    plan.reclaim_bytes = 2 * MIB;
    plan.reclaim_slots = 128;
    let memory = Memory::new(plan);
    for _ in 0..4 {
        let stack = UnitStack::new(MIB - crate::task_heap::page_bytes());
        assert_eq!(stack.len, MIB);
        memory.release(stack.len, Reclaim::UnitStack(stack));
    }
    let work = memory.work.lock().unwrap();
    assert_eq!(work.releases.len(), 2);
    assert_eq!(work.pending_bytes, 2 * MIB);
    assert_eq!(
        (work.sync_dispose_count, work.sync_dispose_bytes),
        (2, 2 * MIB)
    );
    drop(work);

    plan.reclaim_slots = 1;
    let memory = Memory::new(plan);
    for _ in 0..2 {
        let stack = UnitStack::new(MIB - crate::task_heap::page_bytes());
        memory.release(stack.len, Reclaim::UnitStack(stack));
    }
    let work = memory.work.lock().unwrap();
    assert_eq!((work.pending_slots, work.pending_bytes), (1, MIB));
    assert_eq!(work.sync_dispose_count, 1);
}

#[test]
fn ready_scratch_uses_byte_bounds() {
    if crate::test_process::run_isolated("units::memory::tests::ready_scratch_uses_byte_bounds") {
        return;
    }
    use super::super::{Ready, poll::push_arrival, running, take_xready};
    let plan = plan();
    assert_eq!(plan.ready_scratch_bytes, 4096);
    run(plan, || {
        let unit = running().unwrap();
        let mut delivered = Vec::new();
        for token in 0..1_000 {
            push_arrival(unit, token, Ready::READABLE);
        }
        take_xready(|token, _| delivered.push(token));
        assert_eq!(delivered, (0..1_000).collect::<Vec<_>>());
        push_arrival(unit, 1_001, Ready::READABLE);
        take_xready(|token, _| assert_eq!(token, 1_001));
        let entry = size_of::<(u64, Ready)>();
        // SAFETY: this is the running unit's spare.
        let spare = unsafe { &*unit.xspare.get() };
        assert!(spare.capacity() * entry <= 4096);
        assert!(unit.xready.lock().unwrap().capacity() * entry <= 4096);
        crate::tasks::check_ready_scratch();
    });
}

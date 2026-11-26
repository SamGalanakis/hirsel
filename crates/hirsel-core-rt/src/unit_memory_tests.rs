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

fn check_retention_accounting(memory: &Memory) -> usize {
    let worker_bytes: usize = memory.workers.iter().map(|w| w.lock().unwrap().bytes).sum();
    let shared_bytes = memory.retention.lock().unwrap().used;
    let total = memory.retained_used.load(Ordering::Acquire);
    assert_eq!(
        total,
        worker_bytes + shared_bytes,
        "every stocked stack byte must be accounted in retained_used"
    );
    assert!(
        total <= memory.plan.retained_bytes,
        "stocked bytes {total} exceeded cap {}",
        memory.plan.retained_bytes
    );
    total
}

#[test]
fn stack_pair_admission_charging_and_guard_pages() {
    if crate::test_process::run_isolated(
        "units::memory::tests::stack_pair_admission_charging_and_guard_pages",
    ) {
        return;
    }
    let mut plan = UntiedPlan::derive(
        1,
        2,
        64 * MIB as u64,
        HostEnvelope::UNLIMITED,
        ReactorChoice::Readiness,
    )
    .unwrap();
    plan.idle_release_ms = 20;
    // Only recycling contributes retained bytes in this fixture.
    plan.pair_refill_batch = 0;
    let pollers = hirsel_core_net::slot_readiness_pollers(2, here).unwrap();
    // SAFETY: Rust fixture with no capability operands.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    let status = launch_planned(
        plan,
        pollers,
        &config,
        &|_| {
            let pool = super::super::pool();
            let memory = &pool.memory;
            let page = crate::task_heap::page_bytes();
            let pair_bytes = memory.pair_retained_bytes();

            // The initial root unit already has both guarded stacks admitted
            // before dispatch, charged to itself and not to retention.
            let owner = super::super::running().unwrap();
            // SAFETY: this worker exclusively runs `owner`.
            let admitted = unsafe { &*owner.stack.get() };
            let unit_stack = admitted.unit.as_ref().expect("unit stack admitted");
            let root_stack = admitted.root.as_ref().expect("root task stack admitted");
            let (u_lo, u_hi) = unit_stack.guard_bounds();
            assert_eq!(u_hi - u_lo, page, "unit stack must keep its guard page");
            let (t_lo, t_hi, _) = root_stack.bounds();
            assert_eq!(
                t_hi - t_lo,
                page,
                "root task stack must keep its guard page"
            );
            assert_eq!(admitted.charged_bytes, pair_bytes);
            assert_eq!(memory.admitted_bytes.load(Ordering::Acquire), pair_bytes);
            assert_eq!(check_retention_accounting(memory), 0);

            // Spawn 8 helpers and hold them until all are admitted and running.
            let hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            for _ in 0..8 {
                let hold = hold.clone();
                let entered = entered.clone();
                assert!(
                    super::super::spawn_job(Box::new(move || {
                        let me = super::super::running().unwrap();
                        // SAFETY: this worker exclusively runs `me`.
                        let s = unsafe { &*me.stack.get() };
                        assert!(s.unit.is_some() && s.root.is_some());
                        assert_eq!(s.charged_bytes, pair_bytes);
                        if entered.fetch_add(1, Ordering::AcqRel) + 1 == 8 {
                            super::super::wake(owner);
                        }
                        while hold.load(Ordering::Acquire) {
                            super::super::park(Some(super::super::now() + 1_000_000));
                        }
                    }))
                    .is_ok()
                );
            }
            while entered.load(Ordering::Acquire) < 8 {
                super::super::park(Some(super::super::now() + 1_000_000));
            }
            // All 9 live units are charged for their admitted pairs; stocked pairs
            // remain accounted against retained_bytes.
            assert_eq!(
                memory.admitted_bytes.load(Ordering::Acquire),
                9 * pair_bytes
            );
            // No worker is moving pairs while all helpers hold their stacks.
            // Sample on the memory thread to serialize with idle purging.
            memory.map(move || check_retention_accounting(memory));

            hold.store(false, Ordering::Release);
            // After::Done recycles the stacks before retiring the live unit.
            while super::super::LIVE.load(Ordering::Acquire) != 1 {
                super::super::park(Some(super::super::now() + 1_000_000));
            }
            assert_eq!(memory.admitted_bytes.load(Ordering::Acquire), pair_bytes);
            memory.map(move || check_retention_accounting(memory));
            let peak = memory.retained_peak.load(Ordering::Acquire);
            assert!(peak > 0, "recycled pairs should enter worker/shared stock");
            assert!(peak <= memory.plan.retained_bytes);

            // Idle purge must drain per-worker stocks as well as shared retention.
            super::super::park(Some(super::super::now() + 200_000_000));
            assert_eq!(memory.map(move || check_retention_accounting(memory)), 0);
            assert!(!memory.has_retained());
            0
        },
        0,
    )
    .unwrap();
    assert_eq!(status, 0);
}

#[test]
fn batched_refill_coalescing_and_cross_unit_stack_scrub() {
    if crate::test_process::run_isolated(
        "units::memory::tests::batched_refill_coalescing_and_cross_unit_stack_scrub",
    ) {
        return;
    }
    let plan = UntiedPlan::derive(
        1,
        2,
        128 * MIB as u64,
        HostEnvelope::UNLIMITED,
        ReactorChoice::Readiness,
    )
    .unwrap();
    let pollers = hirsel_core_net::slot_readiness_pollers(2, here).unwrap();
    // SAFETY: Rust fixture with no capability operands.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    let status = launch_planned(
        plan,
        pollers,
        &config,
        &|_| {
            use crate::{
                Context,
                tasks::{SchedulerProvider, run_root},
            };
            const SENTINEL: u64 = 0xDEAD_BEEF_CAFE_BABE;

            unsafe extern "C" fn scribble_deep(
                _: *mut Context,
                out: *mut u8,
                input: *mut u8,
            ) -> u8 {
                // Grow the root task stack well past COMMIT, inspect deep slot,
                // then write SENTINEL when mode == 0.
                let mode = unsafe { *input };
                let (base, guard_end, top) = crate::fiber::guarded();
                assert!(guard_end > base && top > guard_end);
                let deep = std::ptr::with_exposed_provenance_mut::<u64>(top - 512 * 1024);
                // SAFETY: inside the active guarded stack reservation; fault
                // handler commits the page on touch.
                let seen = unsafe {
                    let v = deep.read_volatile();
                    if mode == 0 {
                        deep.write_volatile(SENTINEL);
                    }
                    v
                };
                unsafe { out.cast::<u64>().write(seen) };
                0
            }

            let pool = super::super::pool();
            let memory = &pool.memory;
            let owner = super::super::running().unwrap();
            let pair_bytes = memory.pair_retained_bytes();
            let committed_fresh = crate::fiber::commit_for_page(crate::task_heap::page_bytes())
                + crate::task_heap::page_bytes();

            // Wave 1: 16 cold helpers grow their root task stacks deep and exit.
            let done1 = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            for _ in 0..16 {
                let done1 = done1.clone();
                assert!(
                    super::super::spawn_job(Box::new(move || {
                        let mut ctx = Context::with_unit_heap(MIB as u64);
                        crate::causes::task_start(&mut ctx).unwrap();
                        let mut mode = 0u8;
                        let mut seen = u64::MAX;
                        // SAFETY: pinned local operands.
                        let failed = unsafe {
                            run_root(
                                &mut ctx,
                                scribble_deep,
                                &mut mode,
                                (&raw mut seen).cast(),
                                1,
                                8,
                                SchedulerProvider::default(),
                            )
                        };
                        assert_eq!(failed, 0);
                        assert_eq!(seen, 0);
                        // Root stack returned trimmed to its unit before After::Done.
                        let me = super::super::running().unwrap();
                        // SAFETY: exclusive unit access.
                        let s = unsafe { &*me.stack.get() };
                        assert_eq!(s.root.as_ref().unwrap().committed_bytes(), committed_fresh);
                        if done1.fetch_add(1, Ordering::AcqRel) + 1 == 16 {
                            super::super::wake(owner);
                        }
                    }))
                    .is_ok()
                );
            }
            while done1.load(Ordering::Acquire) < 16 {
                super::super::park(Some(super::super::now() + 1_000_000));
            }
            while memory.admitted_bytes.load(Ordering::Acquire) > pair_bytes {
                super::super::park(Some(super::super::now() + 1_000_000));
            }
            let peak_q = memory.work.lock().unwrap().trace.map_queue_peak;
            assert!(
                peak_q <= 16,
                "coalesced refill queue must stay bounded, saw peak={peak_q}"
            );
            check_retention_accounting(memory);

            // Wave 2: reuse stocked pairs across new units; deep stack memory
            // must read zero (trimmed/scrubbed) and reuse must hit worker stock.
            let done2 = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            for _ in 0..8 {
                let done2 = done2.clone();
                assert!(
                    super::super::spawn_job(Box::new(move || {
                        let mut ctx = Context::with_unit_heap(MIB as u64);
                        crate::causes::task_start(&mut ctx).unwrap();
                        let mut mode = 1u8;
                        let mut seen = u64::MAX;
                        // SAFETY: pinned local operands.
                        let failed = unsafe {
                            run_root(
                                &mut ctx,
                                scribble_deep,
                                &mut mode,
                                (&raw mut seen).cast(),
                                1,
                                8,
                                SchedulerProvider::default(),
                            )
                        };
                        assert_eq!(failed, 0);
                        assert_eq!(
                            seen, 0,
                            "reused root stack leaked grown pages from a prior unit"
                        );
                        if done2.fetch_add(1, Ordering::AcqRel) + 1 == 8 {
                            super::super::wake(owner);
                        }
                    }))
                    .is_ok()
                );
            }
            while done2.load(Ordering::Acquire) < 8 {
                super::super::park(Some(super::super::now() + 1_000_000));
            }
            while memory.admitted_bytes.load(Ordering::Acquire) > pair_bytes {
                super::super::park(Some(super::super::now() + 1_000_000));
            }
            let hits: usize = memory.workers.iter().map(|w| w.lock().unwrap().hits).sum();
            assert!(hits > 0, "wave 2 must hit per-worker stack-pair stock");
            check_retention_accounting(memory);
            0
        },
        0,
    )
    .unwrap();
    assert_eq!(status, 0);
}

#[test]
fn reclamation_budget_prioritizes_maps_without_starvation() {
    let mut plan = plan();
    plan.reclaim_bytes = 4 * MIB;
    plan.reclaim_slots = 4;
    let memory: &'static Memory = Box::leak(Box::new(Memory::new(plan)));
    let order = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    // Pre-fill reclamation queue to full budget BEFORE starting memory thread.
    for _ in 0..4 {
        let stack = UnitStack::new(MIB - crate::task_heap::page_bytes());
        memory.release(stack.len, Reclaim::UnitStack(stack));
    }
    // Queue 4 urgent map jobs while releases are already pending.
    let done = std::sync::Arc::new((Mutex::new(0usize), Condvar::new()));
    for i in 0..4u8 {
        let order = order.clone();
        let done = done.clone();
        memory.post(move || {
            order.lock().unwrap().push(i);
            let mut n = done.0.lock().unwrap();
            *n += 1;
            done.1.notify_all();
        });
    }
    let thread = memory.start(plan.memory_stack_bytes).unwrap();
    {
        let mut n = done.0.lock().unwrap();
        while *n < 4 {
            n = done.1.wait(n).unwrap();
        }
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    // Shut down memory thread and verify all queued releases drained cleanly.
    drop(thread);
    let work = memory.work.lock().unwrap();
    assert_eq!((work.pending_slots, work.pending_bytes), (0, 0));
    assert_eq!(work.trace.releases_disposed, 4);
}

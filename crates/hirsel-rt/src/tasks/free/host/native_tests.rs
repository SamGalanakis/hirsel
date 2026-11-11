//! Real exported native clock entries; no bound home adapter on record or replay.
use super::*;
use crate::io::{ClockResult, UnitResult};
use std::sync::atomic::{AtomicU32, Ordering};

struct Clock {
    real: crate::io_real::ConcurrentIo,
    parks: AtomicU32,
}
impl crate::io_concurrent::ConcurrentAdapter for Clock {
    fn keys(&self, request: &Request) -> KeySet {
        self.real.keys(request)
    }
    fn identity(&self, word: u64) -> Option<(crate::io_keys::Key, crate::io_keys::Kind)> {
        self.real.identity(word)
    }
    fn preflight(&self, request: &Request) -> Result<Option<Completion>, AdapterError> {
        self.real.preflight(request)
    }
    fn call(&self, request: Request) -> Result<Completion, AdapterError> {
        self.real.call(request)
    }
    fn call_at(&self, token: u64, request: Request) -> Result<Completion, AdapterError> {
        self.real.call_at(token, request)
    }
    fn finish(&self, token: u64) {
        self.real.finish(token);
    }
    fn parked(&self, token: u64) {
        self.parks.fetch_add(1, Ordering::Release);
        self.real.parked(token);
    }
    fn cancellable(&self, token: u64) -> bool {
        self.real.cancellable(token)
    }
    fn ready(&self) -> Result<crate::io::Ready, AdapterError> {
        self.real.ready()
    }
    fn readiness(&self) -> Option<crate::readiness::Readiness> {
        self.real.readiness()
    }
    fn wait_sources(&self) -> Result<crate::io::WaitSources, AdapterError> {
        self.real.wait_sources()
    }
}

unsafe fn clock_program(ctx: *mut TaskCtx, granted: bool) {
    let mut out = ClockResult {
        tag: 99,
        padding: 99,
        value: 99,
    };
    assert_eq!(
        unsafe { crate::io::hirsel_rt_v2_io_wall_clock(ctx, std::ptr::null_mut(), &mut out, &1) },
        STATUS_OK
    );
    assert_eq!(
        out,
        ClockResult {
            tag: 1,
            padding: 0,
            value: 0
        }
    );
    assert_eq!(
        unsafe {
            crate::io::hirsel_rt_v2_io_monotonic_clock(ctx, std::ptr::null_mut(), &mut out, &1)
        },
        STATUS_OK
    );
    assert_eq!(out.tag, u32::from(!granted));
    assert_eq!(out.padding, 0);
    if granted {
        let clock = out.value;
        assert_eq!(
            unsafe {
                crate::io::hirsel_rt_v2_clock_monotonic(ctx, std::ptr::null_mut(), &mut out, &clock)
            },
            STATUS_OK
        );
        assert_eq!(out.tag, 0);
        let deadline = out.value + 20_000_000;
        let mut slept = UnitResult {
            tag: 99,
            error: 99,
            code: 99,
        };
        assert_eq!(
            unsafe {
                crate::io::hirsel_rt_v2_clock_sleep_until(
                    ctx,
                    std::ptr::null_mut(),
                    &mut slept,
                    &clock,
                    &deadline,
                )
            },
            STATUS_OK
        );
        assert_eq!(
            slept,
            UnitResult {
                tag: 0,
                error: 0,
                code: 0
            }
        );
    } else {
        assert_eq!(out.value, 0);
    }
}

#[test]
fn native_clock_abi_records_concurrent_requests_and_replays_without_a_binding() {
    for width in [1, 8] {
        for durable in [false, true] {
            for granted in [false, true] {
                let mut real = crate::io_real::RealIo::grant(&std::env::temp_dir()).unwrap();
                real.discard_history();
                real.clock_grants(granted, false);
                let adapter = Arc::new(Clock {
                    real: real
                        .into_concurrent()
                        .unwrap_or_else(|_| panic!("Send clock")),
                    parks: Default::default(),
                });
                let mut admitted = Some(adapter.clone());
                let recording = super::super::record::Recording::new(b"native clock ABI", durable);
                let install = recording.clone();
                let mut run = crate::host::BodyRun::new(width, false);
                run.free = true;
                unsafe { run.use_physical_dispatch() };
                let outcome = run.run(
                    move |ctx| unsafe {
                        super::super::enable(ctx);
                        install.root(ctx);
                    },
                    move |ctx| unsafe {
                        {
                            let _role = role::Guard::new(ctx);
                            let _segment = role::Segment::new(ctx);
                            concurrent::Service::install(ctx, admitted.take().unwrap()).unwrap();
                            native::enable(ctx);
                        }
                        clock_program(ctx, granted);
                        STATUS_OK
                    },
                );
                assert_eq!(outcome.ended, Ok(STATUS_OK));
                assert_eq!(outcome.pending, 0);
                assert_eq!(Arc::strong_count(&adapter), 1);
                assert_eq!(adapter.parks.load(Ordering::Acquire) > 0, granted);
                drop(adapter);
                let cut = recording
                    .recorder
                    .stop("returned", None)
                    .unwrap()
                    .unwrap()
                    .0;
                let projection = hirsel_free::projection::Projection::new(
                    "returned",
                    &cut.progress,
                    &recording.trace(),
                );
                let bytes = Arc::try_unwrap(recording)
                    .ok()
                    .unwrap()
                    .recorder
                    .into_sink()
                    .bytes;
                for width in [1, 8] {
                    let replay = super::super::replay::Replay::new(&bytes).unwrap();
                    let install = replay.clone();
                    let mut run = crate::host::BodyRun::new(width, false);
                    run.free = true;
                    unsafe { run.use_physical_dispatch() };
                    let outcome = run.run(
                        move |ctx| unsafe {
                            install.root(ctx);
                            native::enable(ctx);
                        },
                        move |ctx| unsafe {
                            clock_program(ctx, granted);
                            let _role = role::Guard::new(ctx);
                            super::super::replay::start(ctx);
                            unreachable!("native clock replay beyond cut");
                        },
                    );
                    assert!(
                        matches!(outcome.ended, Err(Stop::Blocked(_))),
                        "{outcome:?}"
                    );
                    assert_eq!(outcome.pending, 0);
                    assert_eq!(replay.finish().unwrap(), projection);
                }
            }
        }
    }
}

#[test]
fn native_clock_replay_requires_mapped_metadata_and_refuses_other_requests() {
    let handles = handles::Handles::default();
    assert!(native::replay_keys(&handles, &Request::Monotonic { clock: 77 }).is_err());
    assert!(native::replay_keys(&handles, &Request::Close { handle: 77 }).is_err());
    assert!(native::replay_keys(&handles, &Request::MonotonicClock { io: 2 }).is_err());
    assert!(
        native::replay_keys(
            &handles,
            &Request::Print {
                stdout: 1,
                bytes: vec![]
            }
        )
        .is_err()
    );
    assert_eq!(
        native::replay_keys(&handles, &Request::MonotonicClock { io: 1 }).unwrap(),
        KeySet::new(vec![(crate::io_keys::ROOT, Mode::Shared)])
    );
}

/// Bug: a capability operation rebuilds per-call heap storage (footprints, positions,
/// pending rows) that a live `tasks.free@1` task could keep inline or reuse, so its cost
/// grows with what the runtime rebuilds rather than what the operation needs. Oracle:
/// the per-operation budget the capability-op-cost target sets, counted by the test
/// binary's allocator on the task's thread.
#[test]
fn live_authority_floor_allocation_budget() {
    const CALLS: u64 = 1_000;
    // The Requests claim the stop sweep arbitrates is fresh per call; the pending row
    // and result boxes reuse task-owned storage. Amortized table growth adds a few.
    const PER_CALL: u64 = 2;
    const GROWTH: u64 = 64;
    let mut real = crate::io_real::RealIo::grant(&std::env::temp_dir()).unwrap();
    real.discard_history();
    let grant: hirsel_run_schema::AuthorityGrant = serde_json::from_value(serde_json::json!({
        "provider": "authority.memory@1",
        "floor": "memory",
    }))
    .unwrap();
    real.set_authorities(
        &std::collections::BTreeMap::from([("main".to_owned(), grant)]),
        Some([0; 16]),
    )
    .unwrap();
    let adapter: Arc<dyn crate::io_concurrent::ConcurrentAdapter> = Arc::new(
        real.into_concurrent()
            .unwrap_or_else(|_| panic!("Send authority")),
    );
    let mut admitted = Some(adapter);
    let mut run = crate::host::BodyRun::new(1, false);
    run.free = true;
    unsafe { run.use_physical_dispatch() };
    let outcome = run.run(
        move |ctx| unsafe { super::super::enable(ctx) },
        move |ctx| unsafe {
            let authority = {
                let _role = role::Guard::new(ctx);
                {
                    let _segment = role::Segment::new(ctx);
                    concurrent::Service::install(ctx, admitted.take().unwrap()).unwrap();
                    native::enable(ctx);
                }
                let request = crate::authority::SeamRequest::Project {
                    io: 1,
                    name: "main".into(),
                };
                match crate::io::capability_call(ctx, Request::Seam(request)) {
                    Ok(Completion::Handle(Ok(handle))) => handle,
                    other => panic!("io.authority: {other:?}"),
                }
            };
            let floor = |n: u64| {
                for _ in 0..n {
                    let mut out = u64::MAX;
                    assert_eq!(
                        crate::authority::hirsel_rt_v2_authority_floor(
                            ctx,
                            std::ptr::null_mut(),
                            &mut out,
                            &authority,
                        ),
                        STATUS_OK
                    );
                    // The only authority, index 0: its floor's Level word is below 256.
                    assert!(out < 256, "{out}");
                }
            };
            floor(64);
            let before = crate::io_real::borrowed_net_tests::allocation_calls();
            floor(CALLS);
            let spent = crate::io_real::borrowed_net_tests::allocation_calls() - before;
            assert!(
                spent <= CALLS * PER_CALL + GROWTH,
                "{spent} allocations for {CALLS} authority.floor calls"
            );
            STATUS_OK
        },
    );
    assert_eq!(outcome.ended, Ok(STATUS_OK));
    assert_eq!(outcome.pending, 0);
}

/// Bug: reused slot storage carries a stopped operation's state into a delivery, or the
/// abandoned slot is donated or leaked instead of released with its task. Oracle: a stop
/// leaves the task for good (`stop::raise`), so the stopped call delivers nothing and the
/// context's drop releases its slot exactly once.
#[test]
fn a_stop_after_reused_slots_delivers_nothing_and_releases_its_slot() {
    let mut real = crate::io_real::RealIo::grant(&std::env::temp_dir()).unwrap();
    real.discard_history();
    let grant: hirsel_run_schema::AuthorityGrant = serde_json::from_value(serde_json::json!({
        "provider": "authority.memory@1",
        "floor": "memory",
    }))
    .unwrap();
    real.set_authorities(
        &std::collections::BTreeMap::from([("main".to_owned(), grant)]),
        Some([0; 16]),
    )
    .unwrap();
    let adapter: Arc<dyn crate::io_concurrent::ConcurrentAdapter> = Arc::new(
        real.into_concurrent()
            .unwrap_or_else(|_| panic!("Send authority")),
    );
    let mut admitted = Some(adapter.clone());
    let live = Arc::new(std::sync::Mutex::new(None));
    let seen = live.clone();
    let delivered = Arc::new(AtomicU32::new(0));
    let count = delivered.clone();
    let mut run = crate::host::BodyRun::new(1, false);
    run.free = true;
    unsafe { run.use_physical_dispatch() };
    let outcome = run.run(
        move |ctx| unsafe { super::super::enable(ctx) },
        move |ctx| unsafe {
            let authority = {
                let _role = role::Guard::new(ctx);
                {
                    let _segment = role::Segment::new(ctx);
                    concurrent::Service::install(ctx, admitted.take().unwrap()).unwrap();
                    native::enable(ctx);
                }
                *seen.lock().unwrap() = Some((*ctx).run_ref().free_io.get().unwrap().live.clone());
                let request = crate::authority::SeamRequest::Project {
                    io: 1,
                    name: "main".into(),
                };
                match crate::io::capability_call(ctx, Request::Seam(request)) {
                    Ok(Completion::Handle(Ok(handle))) => handle,
                    other => panic!("io.authority: {other:?}"),
                }
            };
            let mut out = u64::MAX;
            for _ in 0..3 {
                out = u64::MAX;
                assert_eq!(
                    crate::authority::hirsel_rt_v2_authority_floor(
                        ctx,
                        std::ptr::null_mut(),
                        &mut out,
                        &authority,
                    ),
                    STATUS_OK
                );
                assert!(out < 256);
                count.fetch_add(1, Ordering::Release);
            }
            assert!((*ctx).live_host.is_none() && (*ctx).native_host_result.is_none());
            (*ctx).run_ref().control.request_inside(Stop::Requested);
            let mut stopped = u64::MAX;
            crate::authority::hirsel_rt_v2_authority_floor(
                ctx,
                std::ptr::null_mut(),
                &mut stopped,
                &authority,
            );
            unreachable!("a stopped floor returned {stopped}");
        },
    );
    assert_eq!(outcome.ended, Err(Stop::Requested));
    assert_eq!(delivered.load(Ordering::Acquire), 3);
    let live = live.lock().unwrap().take().unwrap();
    // The stopped call's slot holds the live state; it went with its task's context.
    assert_eq!(
        Arc::strong_count(&live),
        1,
        "the stopped slot outlived its task"
    );
    assert_eq!(Arc::strong_count(&adapter), 1);
}

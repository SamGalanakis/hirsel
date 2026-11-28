//! Slice 2s laws: contained run stops, fault classification, signal chaining
//! with flags, guard rule (already-writable room), embed-safe pipes, and busy
//! pure-code peer worker teardown (EMB-3).
use crate::{
    Context,
    fiber::{self, RUNTIME_ROOM},
    process::{Config, MainResult},
    run_stop::{EndReason, Execution, run_contained_worker},
    steps::{self, EMBED_QUANTUM},
    tasks::SchedulerProvider,
};
use std::{
    os::unix::{io::AsRawFd, process::ExitStatusExt},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "embed_stop_signal_tests.rs"]
mod signal;

unsafe extern "C" fn silent_diag(_: *mut u8, _: *const u8, _: usize) {}

fn test_config(state: *mut u8) -> Config {
    Config {
        launch: ptr::null(),
        describe_output: None,
        root_limit: 256 << 10,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: ptr::null(),
        state,
        diagnostic: silent_diag,
        describe: None,
        scheduler: SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    }
}

#[inline(never)]
fn recurse_forever(depth: usize) -> usize {
    let mut buf = [0u8; 4096];
    std::hint::black_box(&mut buf);
    buf[0] = (depth & 0xff) as u8;
    std::hint::black_box(buf[0]) as usize + recurse_forever(depth + 1)
}

#[inline(never)]
fn recurse_bytes(target_bytes: usize, acc: usize) -> usize {
    let mut buf = [0u8; 4096];
    std::hint::black_box(&mut buf);
    if target_bytes <= 4096 {
        return acc + buf[0] as usize;
    }
    recurse_bytes(target_bytes - 4096, acc + 1)
}

fn spawn_child(test_name: &str, mode: &str) -> std::process::Output {
    crate::test_process::command()
        .args(["--ignored", "--exact", test_name, "--test-threads=1"])
        .env("HIRSEL_EMBED_STOP_MODE", mode)
        .output()
        .unwrap()
}

// Note: The hermetic grep law forbidding `process::stop` outside `process.rs`
// and `standalone_allocator.rs` lives in `tools/tests/core_embed_stop_laws.py`
// (target `//:core-embed-stop-laws`), where Buck2 mounts every source file.

// ---------------------------------------------------------------------------
// Law 2: Host survives a recoverable guard fault and starts generation 2.
// ---------------------------------------------------------------------------
#[test]
fn host_survives_recoverable_guard_fault_and_restarts() {
    unsafe extern "C" fn overflow_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        std::hint::black_box(recurse_forever(0));
        0
    }
    unsafe extern "C" fn normal_entry(
        _: *mut Context,
        cfg: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: test pins AtomicUsize in cfg.state.
        unsafe {
            let out = &*(*cfg).state.cast::<AtomicUsize>();
            out.store(42, Ordering::SeqCst);
        }
        0
    }

    fiber::reset_fault_counters();
    let gen1 = Execution::contained();
    let cfg1 = test_config(ptr::null_mut());
    // SAFETY: gen1 and cfg1 live until run_contained_worker returns.
    let res1 = unsafe { run_contained_worker(&gen1, &cfg1, overflow_entry) };
    match res1 {
        Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "Stack"),
        other => panic!("expected RunStop(Stack), got {other:?}"),
    }
    assert!(fiber::guard_hits() >= 1, "expected hard guard hit");
    assert_eq!(
        gen1.stop().map(|s| s.name().to_string()),
        Some("Stack".into())
    );

    let outcome = AtomicUsize::new(0);
    let gen2 = Execution::contained();
    let cfg2 = test_config((&raw const outcome).cast_mut().cast());
    // SAFETY: gen2, cfg2, and outcome live until run_contained_worker returns.
    let res2 = unsafe { run_contained_worker(&gen2, &cfg2, normal_entry) };
    assert_eq!(res2, Ok(0));
    assert_eq!(outcome.load(Ordering::SeqCst), 42);
    assert_eq!(gen2.end_reason(), None);
}

// ---------------------------------------------------------------------------
// Law 3: Native foreign stack overflow is NOT recovered.
// ---------------------------------------------------------------------------
#[test]
fn native_foreign_stack_overflow_is_not_recovered() {
    let dfl = spawn_child("embed_stop_tests::native_foreign_overflow_child", "sig_dfl");
    assert!(
        !dfl.status.success(),
        "SIG_DFL native overflow must terminate child: {dfl:?}"
    );
    assert_ne!(
        dfl.status.code(),
        Some(102),
        "native foreign overflow must never exit 102"
    );
    let sig = dfl.status.signal();
    assert!(
        sig == Some(libc::SIGSEGV) || sig == Some(libc::SIGBUS),
        "expected SIGSEGV/SIGBUS termination, got {dfl:?}"
    );

    let custom = spawn_child(
        "embed_stop_tests::native_foreign_overflow_child",
        "host_handler",
    );
    assert_eq!(
        custom.status.code(),
        Some(77),
        "custom host handler should receive native foreign overflow: {custom:?}"
    );
    assert_eq!(custom.stderr, b"HOST_CRASH_HANDLER\n");
}

#[test]
#[ignore = "isolated native foreign overflow child, invoked by named parent"]
fn native_foreign_overflow_child() {
    unsafe extern "C" fn host_crash_handler(
        _: libc::c_int,
        _: *mut libc::siginfo_t,
        _: *mut libc::c_void,
    ) {
        // SAFETY: async-signal-safe write + _exit.
        unsafe {
            libc::write(2, b"HOST_CRASH_HANDLER\n".as_ptr().cast(), 19);
            libc::_exit(77);
        }
    }
    unsafe extern "C" fn foreign_overflow_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: runs synchronous native foreign thunk on a Native-kind stack
        // with a valid zeroed Request (prof reads `(*request).site`).
        unsafe {
            let mut req: crate::provider::Request = std::mem::zeroed();
            crate::tasks::host_operation(ptr::null_mut(), &raw mut req, foreign_blow_stack);
        }
        0
    }
    unsafe extern "C" fn foreign_blow_stack(
        _: *mut u8,
        _: *mut crate::provider::Request,
    ) -> crate::provider::Outcome {
        std::hint::black_box(recurse_forever(0));
        crate::provider::Outcome::default()
    }

    let mode = std::env::var("HIRSEL_EMBED_STOP_MODE").unwrap();
    // SAFETY: isolated child sets its own SIGSEGV/SIGBUS disposition before arming.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&mut act.sa_mask);
        if mode == "host_handler" {
            act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            act.sa_sigaction = host_crash_handler as *const () as usize;
        } else {
            act.sa_flags = 0;
            act.sa_sigaction = libc::SIG_DFL;
        }
        assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
        #[cfg(target_vendor = "apple")]
        assert_eq!(libc::sigaction(libc::SIGBUS, &act, ptr::null_mut()), 0);
    }

    let generation = Execution::contained();
    let cfg = test_config(ptr::null_mut());
    // SAFETY: generation and cfg live for the call; native overflow must not return.
    let res = unsafe { run_contained_worker(&generation, &cfg, foreign_overflow_entry) };
    panic!("native foreign stack overflow was unexpectedly recovered: {res:?}");
}

// ---------------------------------------------------------------------------
// Law 4: Closed child pipe under SIG_DFL delivers Reset & preserves SIG_DFL.
// ---------------------------------------------------------------------------
#[test]
fn closed_child_pipe_under_sig_dfl_delivers_reset_and_preserves_disposition() {
    let out = spawn_child("embed_stop_tests::closed_pipe_sig_dfl_child", "run");
    assert!(
        out.status.success(),
        "closed child pipe under SIG_DFL failed: {out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "isolated SIGPIPE SIG_DFL child, invoked by named parent"]
fn closed_pipe_sig_dfl_child() {
    // SAFETY: isolated child explicitly installs SIG_DFL for SIGPIPE.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut act.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGPIPE, &act, ptr::null_mut()), 0);
    }

    // Spawn a real child with piped stdin that immediately closes stdin and
    // signals readiness on stdout.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exec 0<&-; echo READY; sleep 2"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut ready = [0u8; 6];
    std::io::Read::read_exact(&mut stdout, &mut ready).unwrap();
    assert_eq!(&ready, b"READY\n");
    let stdin = child.stdin.take().unwrap();
    let fd = stdin.as_raw_fd();

    #[cfg(target_vendor = "apple")]
    unsafe {
        assert_eq!(libc::fcntl(fd, 73 /* F_SETNOSIGPIPE */, 1), 0);
    }

    struct PipeState {
        fd: i32,
        err_code: i32,
    }
    unsafe extern "C" fn pipe_write_entry(
        _: *mut Context,
        cfg: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: PipeState lives until worker returns.
        unsafe {
            let st = &mut *(*cfg).state.cast::<PipeState>();
            let msg = b"hello closed pipe";
            let prefix = crate::provider::Buffer {
                data: msg.as_ptr().cast_mut(),
                len: msg.len(),
                capacity: msg.len(),
            };
            let empty = crate::provider::Buffer {
                data: ptr::null_mut(),
                len: 0,
                capacity: 0,
            };
            match crate::os_net::writev_pipe_nosigpipe(st.fd, &prefix, &empty) {
                Err(code) => st.err_code = code,
                Ok(_) => st.err_code = 0,
            }
        }
        0
    }

    let mut st = PipeState { fd, err_code: 0 };
    let generation = Execution::contained();
    let cfg = test_config((&raw mut st).cast());
    // SAFETY: st, generation, cfg live until run_contained_worker finishes.
    let res = unsafe { run_contained_worker(&generation, &cfg, pipe_write_entry) };
    assert_eq!(res, Ok(0));
    assert_eq!(st.err_code, libc::EPIPE, "expected EPIPE -> Reset");
    // Verify os_net::signal(5) represents Reset (tag 5, code 0).
    let outcome = crate::os_net::signal(5);
    assert_eq!(outcome.error.tag, 5);
    assert_eq!(outcome.error.code, 0);

    // Verify process-wide SIGPIPE disposition is still SIG_DFL.
    // SAFETY: querying current SIGPIPE sigaction.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(libc::SIGPIPE, ptr::null(), &mut current), 0);
        assert_eq!(
            current.sa_sigaction,
            libc::SIG_DFL,
            "contained run must never alter host SIGPIPE disposition"
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// Law 5: Host signal stack is untouched and distinct from the worker's.
// ---------------------------------------------------------------------------
#[test]
fn host_signal_stack_is_untouched_and_distinct_from_worker() {
    let mut host_buf = vec![0u8; 64 << 10];
    let mut saved_host: libc::stack_t = unsafe { std::mem::zeroed() };
    let custom = libc::stack_t {
        ss_sp: host_buf.as_mut_ptr().cast(),
        ss_flags: 0,
        ss_size: host_buf.len(),
    };
    // SAFETY: host_buf outlives the custom sigaltstack installation below.
    unsafe {
        assert_eq!(libc::sigaltstack(&custom, &mut saved_host), 0);
    }

    let worker_sp = AtomicUsize::new(0);
    unsafe extern "C" fn check_worker_ss(
        _: *mut Context,
        cfg: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: reading this worker thread's sigaltstack into cfg.state.
        unsafe {
            let mut ss: libc::stack_t = std::mem::zeroed();
            assert_eq!(libc::sigaltstack(ptr::null(), &mut ss), 0);
            let slot = &*(*cfg).state.cast::<AtomicUsize>();
            slot.store(ss.ss_sp.addr(), Ordering::SeqCst);
        }
        0
    }

    let generation = Execution::contained();
    let cfg = test_config((&raw const worker_sp).cast_mut().cast());
    // SAFETY: generation, cfg, worker_sp live until run_contained_worker finishes.
    let res = unsafe { run_contained_worker(&generation, &cfg, check_worker_ss) };
    assert_eq!(res, Ok(0));

    let mut after: libc::stack_t = unsafe { std::mem::zeroed() };
    // SAFETY: reading back host thread's sigaltstack and restoring previous.
    unsafe {
        assert_eq!(libc::sigaltstack(ptr::null(), &mut after), 0);
        let mut restore = saved_host;
        if (restore.ss_flags & libc::SS_DISABLE) != 0 {
            restore.ss_sp = ptr::null_mut();
            restore.ss_size = 0;
        }
        let _ = libc::sigaltstack(&restore, ptr::null_mut());
    }

    assert_eq!(after.ss_sp.addr(), host_buf.as_ptr().addr());
    assert_eq!(after.ss_size, host_buf.len());
    assert_eq!(after.ss_flags, 0);
    let wsp = worker_sp.load(Ordering::SeqCst);
    assert_ne!(wsp, 0, "worker must have an active alternate signal stack");
    assert_ne!(
        wsp,
        host_buf.as_ptr().addr(),
        "worker signal stack must not alias host signal stack"
    );
}

// ---------------------------------------------------------------------------
// Law 9: Returned stops release locks (`CountOverflow` + lock-held `HostMemory`).
// ---------------------------------------------------------------------------
#[test]
fn returned_stops_release_locks_and_allow_restart() {
    static PROVIDER_LOCK: Mutex<usize> = Mutex::new(0);

    unsafe extern "C" fn count_overflow_entry(
        context: *mut Context,
        config: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: seed a counted node to u64::MAX and trigger CountOverflow.
        unsafe {
            // The first caught failure installs the isolate's reserved cell.
            assert!(crate::isolate::hirsel_core_isolate_enter(context).entered);
            crate::causes::record(
                &mut *context,
                crate::causes::Cause::new(crate::causes::BOUNDS, 0),
            );
            let caught = crate::isolate::hirsel_core_isolate_catch(context);
            crate::abi::hirsel_core_causes_drop(caught.causes);
            let text = crate::abi::hirsel_core_alloc(
                context,
                ptr::null_mut(),
                8,
                8,
                8,
                false,
                crate::block::PLAIN,
            )
            .pointer;
            ptr::copy_nonoverlapping(b"retained".as_ptr(), text, 8);
            crate::abi::hirsel_core_raise_string(context, 0, text, 8, 8);
            let out = (*config).state.cast::<*mut crate::causes::Cell>();
            out.write(crate::abi::hirsel_core_causes_take(context));

            // Physical size differs from logical charge on emitted allocations.
            let other = crate::abi::hirsel_core_alloc(
                context,
                ptr::null_mut(),
                96,
                8,
                37,
                false,
                crate::block::PLAIN,
            );
            assert!(!other.pointer.is_null());
            let node = crate::block::Block::graph(&*context, Some(16), 16, 8)
                .unwrap()
                .into_raw();
            crate::counted::initialize(node, false);
            node.cast::<u64>().as_ptr().write(u64::MAX);
            crate::counted::share(node, false);
        }
        0
    }

    let gen1 = Execution::contained();
    let mut retained: *mut crate::causes::Cell = ptr::null_mut();
    let cfg = test_config((&raw mut retained).cast());
    match unsafe { run_contained_worker(&gen1, &cfg, count_overflow_entry) } {
        Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "CountOverflow"),
        other => panic!("expected CountOverflow, got {other:?}"),
    }

    drop(gen1);
    // SAFETY: the worker transferred the native cause chain before it stopped.
    unsafe {
        assert!(!retained.is_null());
        let cell = &*retained;
        assert!(cell.reserved);
        assert_eq!(
            cell.detached_message_bytes().as_deref(),
            Some(b"retained".as_slice())
        );
        crate::abi::hirsel_core_causes_drop(retained);
    }

    // Lock-held HostMemory: fallible allocation fails inside lock, drops guard
    // BEFORE raising, so generation 2 acquires the same lock without deadlock.
    unsafe extern "C" fn locked_host_memory_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        let mut guard = PROVIDER_LOCK.lock().unwrap();
        *guard += 1;
        // 1 EiB layout is valid for Layout::from_size_align on 64-bit, but
        // guaranteed refused by malloc/allocator -> Err(Stop("HostMemory")).
        let huge = std::alloc::Layout::from_size_align(1usize << 60, 8).unwrap();
        let res = crate::allocator::try_allocate_raw(huge);
        drop(guard);
        if let Err(stop) = res {
            crate::run_stop::raise(stop);
        }
        0
    }

    let gen2 = Execution::contained();
    match unsafe { run_contained_worker(&gen2, &cfg, locked_host_memory_entry) } {
        Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "HostMemory"),
        other => panic!("expected HostMemory, got {other:?}"),
    }

    // Generation 3 acquires the exact same lock and runs to completion.
    unsafe extern "C" fn reuse_lock_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        let mut guard = PROVIDER_LOCK.lock().unwrap();
        *guard += 10;
        0
    }
    let gen3 = Execution::contained();
    assert_eq!(
        unsafe { run_contained_worker(&gen3, &cfg, reuse_lock_entry) },
        Ok(0)
    );
    assert_eq!(*PROVIDER_LOCK.lock().unwrap(), 11);
}

// ---------------------------------------------------------------------------
// Law 10: Room check (already-writable room near guard & across growth edge).
// ---------------------------------------------------------------------------
#[test]
fn room_check_prevents_guard_hit_and_growth_fault_in_lock() {
    // (a) Near hard guard: fiber::room(RUNTIME_ROOM) raises Stack with 0 guard hits.
    #[inline(never)]
    fn recurse_near_guard(guard_end: usize) {
        let mut buf = [0u8; 4096];
        std::hint::black_box(&mut buf);
        let sp = (&raw const buf).addr();
        if sp.saturating_sub(guard_end) < RUNTIME_ROOM {
            // Must fail via room check without touching the hard guard page!
            if let Err(stop) = fiber::room(RUNTIME_ROOM) {
                crate::run_stop::raise(stop);
            }
            panic!("room check should have failed near hard guard");
        }
        recurse_near_guard(guard_end);
    }
    unsafe extern "C" fn near_guard_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        let (_, guard_end, _) = fiber::guarded();
        assert_ne!(guard_end, 0);
        recurse_near_guard(guard_end);
        0
    }

    fiber::reset_fault_counters();
    let gen1 = Execution::contained();
    let cfg = test_config(ptr::null_mut());
    match unsafe { run_contained_worker(&gen1, &cfg, near_guard_entry) } {
        Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "Stack"),
        other => panic!("expected Stack via room check, got {other:?}"),
    }
    assert_eq!(
        fiber::guard_hits(),
        0,
        "room check must stop before touching the hard guard page"
    );

    // (b) Across an uncommitted growth page: room() pre-commits BEFORE lock so
    //     zero growth faults occur while the lock is held.
    static LOCK: Mutex<()> = Mutex::new(());
    static FAULTS_IN_LOCK: AtomicUsize = AtomicUsize::new(usize::MAX);

    #[inline(never)]
    fn use_stack_under_lock() {
        let mut buf = [0u8; 8192];
        std::hint::black_box(&mut buf);
        buf[0] = 1;
        buf[8191] = 2;
        std::hint::black_box(buf[0] + buf[8191]);
    }

    #[inline(never)]
    fn approach_growth_edge() {
        let mut buf = [0u8; 4096];
        std::hint::black_box(&mut buf);
        let sp = (&raw const buf).addr();
        let low = fiber::current_low_water();
        assert_ne!(low, 0);
        if sp.saturating_sub(low) < 8192 {
            // We are within 8 KiB of the uncommitted growth boundary.
            // Pre-commit via fiber::room BEFORE acquiring the lock!
            fiber::room(RUNTIME_ROOM).unwrap();
            let _g = LOCK.lock().unwrap();
            let before = fiber::growth_faults();
            use_stack_under_lock();
            let after = fiber::growth_faults();
            FAULTS_IN_LOCK.store(after - before, Ordering::SeqCst);
            return;
        }
        approach_growth_edge();
    }
    unsafe extern "C" fn growth_edge_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        approach_growth_edge();
        0
    }

    let gen2 = Execution::contained();
    assert_eq!(
        unsafe { run_contained_worker(&gen2, &cfg, growth_edge_entry) },
        Ok(0)
    );
    assert_eq!(
        FAULTS_IN_LOCK.load(Ordering::SeqCst),
        0,
        "fiber::room must pre-commit before lock so zero growth faults occur under lock"
    );
}

// ---------------------------------------------------------------------------
// Law 11: Busy pure-code peer worker exits on both run stop and host stop.
// ---------------------------------------------------------------------------
#[test]
fn busy_pure_code_peer_worker_exits_on_run_stop_and_host_stop() {
    struct LoopState {
        reloads: AtomicUsize,
        running: AtomicBool,
    }
    unsafe extern "C" fn busy_pure_loop_entry(
        context: *mut Context,
        cfg: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: LoopState pinned until worker joins; context is this task's Context.
        unsafe {
            let st = &*(*cfg).state.cast::<LoopState>();
            assert!((*context).step_bounds.is_empty());
            assert_eq!((*context).steps, EMBED_QUANTUM);
            loop {
                // Emulate emitted `Inst::Step` decrementing to 0 and hitting cold check.
                (*context).steps = 0;
                let failed = steps::hirsel_core_step_out(context, 0);
                assert_eq!(failed, 0);
                assert_eq!((*context).steps, EMBED_QUANTUM);
                let n = st.reloads.fetch_add(1, Ordering::AcqRel) + 1;
                if n == 3 {
                    st.running.store(true, Ordering::Release);
                }
            }
        }
    }
    unsafe extern "C" fn trigger_stack_stop_entry(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        std::hint::black_box(recurse_forever(0));
        0
    }

    // Subcase (a): peer worker raises a run stop (Stack).
    {
        let generation = Arc::new(Execution::contained());
        let st = Arc::new(LoopState {
            reloads: AtomicUsize::new(0),
            running: AtomicBool::new(false),
        });
        let gen_a = generation.clone();
        let st_a = st.clone();
        let busy = std::thread::spawn(move || {
            let cfg = test_config(Arc::as_ptr(&st_a).cast_mut().cast());
            unsafe { run_contained_worker(&gen_a, &cfg, busy_pure_loop_entry) }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !st.running.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "busy worker never entered loop");
            std::thread::sleep(Duration::from_millis(1));
        }
        let cfg_b = test_config(ptr::null_mut());
        let res_b = unsafe { run_contained_worker(&generation, &cfg_b, trigger_stack_stop_entry) };
        match res_b {
            Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "Stack"),
            other => panic!("expected Stack stop, got {other:?}"),
        }
        let res_a = busy.join().unwrap();
        match res_a {
            Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "Stack"),
            other => panic!("busy peer should exit with Stack, got {other:?}"),
        }
        assert!(st.reloads.load(Ordering::Acquire) >= 3);
    }

    // Subcase (b): host requests stop while busy peer is in pure loop.
    {
        let generation = Arc::new(Execution::contained());
        let st = Arc::new(LoopState {
            reloads: AtomicUsize::new(0),
            running: AtomicBool::new(false),
        });
        let gen_a = generation.clone();
        let st_a = st.clone();
        let busy = std::thread::spawn(move || {
            let cfg = test_config(Arc::as_ptr(&st_a).cast_mut().cast());
            unsafe { run_contained_worker(&gen_a, &cfg, busy_pure_loop_entry) }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !st.running.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "busy worker never entered loop");
            std::thread::sleep(Duration::from_millis(1));
        }
        generation.request_host_stop();
        let res_a = busy.join().unwrap();
        assert_eq!(res_a, Err(EndReason::HostStop));
        assert_eq!(generation.end_reason(), Some(EndReason::HostStop));
    }
}

#[test]
fn stopped_worker_leaves_peer_pin_with_generation() {
    unsafe extern "C" fn stop(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        crate::run_stop::raise(crate::run_stop::Stop::from_static(b"PeerStop"));
    }
    unsafe fn release(data: *const ()) {
        unsafe { drop(Arc::from_raw(data.cast::<()>())) };
    }
    let observed = Arc::new(());
    let generation = Execution::contained();
    let scope = unsafe { crate::run_stop::ExecutionScope::enter(&generation) };
    let pin = unsafe { crate::code::CodePin::from_raw(Arc::into_raw(observed.clone()), release) };
    generation.register_pin(pin);
    let cfg = test_config(ptr::null_mut());
    let end = unsafe { run_contained_worker(&generation, &cfg, stop) };
    assert!(matches!(end, Err(EndReason::RunStop(_))));
    assert_eq!(Arc::strong_count(&observed), 2);
    drop(scope);
    drop(generation);
    assert_eq!(Arc::strong_count(&observed), 1);
}

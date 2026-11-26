//! Rust fixture for the C `-Wl,--wrap=exit,--wrap=_exit,--wrap=mprotect,--wrap=munmap,--wrap=malloc`
//! fault harness (Slice 2s laws). Links against the embed-slot `hirsel-core-rt`
//! (`features = []`).
use hirsel_core_rt::{
    Context,
    allocator::try_allocate_raw,
    block::Block,
    channel::{hirsel_core_channel_new, hirsel_core_receive},
    counted,
    fiber::{self, RUNTIME_ROOM},
    isa,
    process::{Config, MainResult},
    run_stop::{EndReason, GenerationEnd, run_contained_worker},
    steps::{self, EMBED_QUANTUM},
    tasks::{SchedulerProvider, ValueLayout},
};
use std::{
    alloc::Layout,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn wrap_arm_fail_next_mprotect(stack_base: usize, stack_len: usize);
    fn wrap_disarm_fail_mprotect();
    fn wrap_arm_fail_next_malloc();
    fn wrap_disarm_fail_malloc();
    fn wrap_reset_munmap_log();
    fn wrap_saw_whole_munmap(base: usize, len: usize) -> i32;
}

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

unsafe fn write_name(dst: *mut u8, cap: usize, name: &str) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let bytes = name.as_bytes();
    let n = bytes.len().min(cap - 1);
    // SAFETY: caller provides writable [dst, dst + cap).
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
        *dst.add(n) = 0;
    }
}

#[inline(never)]
#[allow(unconditional_recursion)]
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

unsafe extern "C" fn ok_with_growth_entry(
    _: *mut Context,
    cfg: *const Config,
    _: *mut MainResult,
) -> u8 {
    let v = std::hint::black_box(recurse_bytes(160 << 10, 1));
    // SAFETY: caller pins cfg and AtomicUsize in cfg.state.
    unsafe {
        if !(*cfg).state.is_null() {
            (*(*cfg).state.cast::<AtomicUsize>()).store(v, Ordering::SeqCst);
        }
    }
    0
}

fn verify_clean_restart() -> bool {
    let witness = AtomicUsize::new(0);
    let generation = GenerationEnd::contained();
    let cfg = test_config((&raw const witness).cast_mut().cast());
    let res = unsafe { run_contained_worker(&generation, &cfg, ok_with_growth_entry) };
    res == Ok(0) && witness.load(Ordering::SeqCst) > 0 && generation.end_reason().is_none()
}

/// Denied x86-64 CPU selection via ISA seam -> returns "UnsupportedCpu" without exit().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_isa_denied(name_buf: *mut u8, cap: usize) -> i32 {
    match isa::init_embedded_with(|| None) {
        Err(name) => {
            unsafe { write_name(name_buf, cap, name) };
            1
        }
        Ok(_) => 0,
    }
}

/// Recoverable guard fault -> "Stack" + clean generation 2 restart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_guard_fault(name_buf: *mut u8, cap: usize) -> i32 {
    unsafe extern "C" fn blow(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        std::hint::black_box(recurse_forever(0));
        0
    }
    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    match unsafe { run_contained_worker(&generation, &cfg, blow) } {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            if verify_clean_restart() { 1 } else { -2 }
        }
        _ => -1,
    }
}

/// Refused stack growth during a Hirsel growth fault -> fail-closed "HostMemory",
/// whole-region munmap observed, clean restart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_refused_growth_fault(name_buf: *mut u8, cap: usize) -> i32 {
    static STACK_BASE: AtomicUsize = AtomicUsize::new(0);
    static STACK_LEN: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn grow_under_enomem(
        _: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        let (base, _, top) = fiber::guarded();
        let len = top - base;
        STACK_BASE.store(base, Ordering::SeqCst);
        STACK_LEN.store(len, Ordering::SeqCst);
        unsafe {
            wrap_reset_munmap_log();
            wrap_arm_fail_next_mprotect(base, len);
        }
        std::hint::black_box(recurse_bytes(160 << 10, 0));
        0
    }

    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    let res = unsafe { run_contained_worker(&generation, &cfg, grow_under_enomem) };
    unsafe { wrap_disarm_fail_mprotect() };
    let base = STACK_BASE.load(Ordering::SeqCst);
    let len = STACK_LEN.load(Ordering::SeqCst);
    match res {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            if base == 0 || len == 0 || unsafe { wrap_saw_whole_munmap(base, len) } == 0 {
                return -3;
            }
            if verify_clean_restart() { 1 } else { -2 }
        }
        _ => -1,
    }
}

/// Refused stack growth during `fiber::room` pre-commit before a locked op ->
/// fail-closed "HostMemory", lock never stranded, whole-region munmap observed,
/// clean restart reusing the lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_refused_growth_room(name_buf: *mut u8, cap: usize) -> i32 {
    static STACK_BASE: AtomicUsize = AtomicUsize::new(0);
    static STACK_LEN: AtomicUsize = AtomicUsize::new(0);
    static ROOM_LOCK: Mutex<usize> = Mutex::new(0);

    #[inline(never)]
    fn approach_and_room_fail() {
        let mut buf = [0u8; 4096];
        std::hint::black_box(&mut buf);
        let sp = (&raw const buf).addr();
        let low = fiber::current_low_water();
        if sp.saturating_sub(low) < 8192 {
            let (base, _, top) = fiber::guarded();
            let len = top - base;
            STACK_BASE.store(base, Ordering::SeqCst);
            STACK_LEN.store(len, Ordering::SeqCst);
            unsafe {
                wrap_reset_munmap_log();
                wrap_arm_fail_next_mprotect(base, len);
            }
            if let Err(stop) = fiber::room(RUNTIME_ROOM) {
                hirsel_core_rt::run_stop::raise(stop);
            }
            let mut g = ROOM_LOCK.lock().unwrap();
            *g += 1;
            return;
        }
        approach_and_room_fail();
    }

    unsafe extern "C" fn room_entry(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        approach_and_room_fail();
        0
    }
    unsafe extern "C" fn reuse_lock(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        let mut g = ROOM_LOCK.lock().unwrap();
        *g += 10;
        0
    }

    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    let res = unsafe { run_contained_worker(&generation, &cfg, room_entry) };
    unsafe { wrap_disarm_fail_mprotect() };
    let base = STACK_BASE.load(Ordering::SeqCst);
    let len = STACK_LEN.load(Ordering::SeqCst);
    match res {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            if base == 0 || len == 0 || unsafe { wrap_saw_whole_munmap(base, len) } == 0 {
                return -3;
            }
            let gen2 = GenerationEnd::contained();
            if unsafe { run_contained_worker(&gen2, &cfg, reuse_lock) } != Ok(0) {
                return -4;
            }
            if *ROOM_LOCK.lock().unwrap() != 10 {
                return -5;
            }
            1
        }
        _ => -1,
    }
}

/// `--wrap=malloc` refusal inside a lock-held allocation releases the lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_lock_held_malloc_refusal(
    name_buf: *mut u8,
    cap: usize,
) -> i32 {
    static ALLOC_LOCK: Mutex<usize> = Mutex::new(0);

    unsafe extern "C" fn locked_alloc(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        let mut g = ALLOC_LOCK.lock().unwrap();
        *g += 1;
        unsafe { wrap_arm_fail_next_malloc() };
        let res = try_allocate_raw(Layout::from_size_align(64, 8).unwrap());
        unsafe { wrap_disarm_fail_malloc() };
        drop(g);
        if let Err(stop) = res {
            hirsel_core_rt::run_stop::raise(stop);
        }
        0
    }
    unsafe extern "C" fn reuse_lock(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        let mut g = ALLOC_LOCK.lock().unwrap();
        *g += 10;
        0
    }

    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    let res = unsafe { run_contained_worker(&generation, &cfg, locked_alloc) };
    unsafe { wrap_disarm_fail_malloc() };
    match res {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            let gen2 = GenerationEnd::contained();
            if unsafe { run_contained_worker(&gen2, &cfg, reuse_lock) } != Ok(0) {
                return -4;
            }
            if *ALLOC_LOCK.lock().unwrap() != 11 {
                return -5;
            }
            1
        }
        _ => -1,
    }
}

/// `CountOverflow` stop + clean restart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_count_overflow(name_buf: *mut u8, cap: usize) -> i32 {
    unsafe extern "C" fn overflow(
        context: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        unsafe {
            let node = Block::graph(&*context, Some(16), 16, 8).unwrap().into_raw();
            counted::initialize(node, false);
            node.cast::<u64>().as_ptr().write(u64::MAX);
            counted::share(node, false);
        }
        0
    }
    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    match unsafe { run_contained_worker(&generation, &cfg, overflow) } {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            if verify_clean_restart() { 1 } else { -2 }
        }
        _ => -1,
    }
}

/// `Deadlock` stop + clean restart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_deadlock(name_buf: *mut u8, cap: usize) -> i32 {
    unsafe extern "C" fn deadlock(
        context: *mut Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        unsafe {
            let layout = ValueLayout {
                bytes: 8,
                align: 8,
                logical: 0,
                clean: None,
            };
            let pair = hirsel_core_channel_new(context, 1, layout, 1);
            let mut out = 0u64;
            // Receive on empty channel while holding sender on this sole task -> Deadlock.
            hirsel_core_receive(context, pair.receiver, (&raw mut out).cast(), 1);
        }
        0
    }
    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    match unsafe { run_contained_worker(&generation, &cfg, deadlock) } {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            if verify_clean_restart() { 1 } else { -2 }
        }
        _ => -1,
    }
}

/// Host stop (`request_host_stop`) stops a busy pure-code worker cleanly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_host_stop() -> i32 {
    unsafe extern "C" fn busy(context: *mut Context, cfg: *const Config, _: *mut MainResult) -> u8 {
        unsafe {
            let flag = &*(*cfg).state.cast::<AtomicBool>();
            let mut n = 0usize;
            loop {
                (*context).steps = 0;
                steps::hirsel_core_step_out(context, 0);
                if (*context).steps != EMBED_QUANTUM {
                    return 9;
                }
                n += 1;
                if n == 3 {
                    flag.store(true, Ordering::Release);
                }
            }
        }
    }
    let generation = Arc::new(GenerationEnd::contained());
    let running = Arc::new(AtomicBool::new(false));
    let gen_w = generation.clone();
    let run_w = running.clone();
    let handle = std::thread::spawn(move || {
        let cfg = test_config(Arc::as_ptr(&run_w).cast_mut().cast());
        unsafe { run_contained_worker(&gen_w, &cfg, busy) }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !running.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return -2;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    generation.request_host_stop();
    if handle.join().unwrap() != Err(EndReason::HostStop) {
        return -1;
    }
    if verify_clean_restart() { 1 } else { -3 }
}

/// Replaced fault handler refusal -> "FaultHandlerReplaced" without exit() or reinstall.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_fixture_replaced_handler(name_buf: *mut u8, cap: usize) -> i32 {
    unsafe extern "C" fn host_dummy(_: libc::c_int) {}
    unsafe extern "C" fn ok_entry(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        0
    }
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = host_dummy as *const () as usize;
        libc::sigemptyset(&mut act.sa_mask);
        if libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()) != 0 {
            return -2;
        }
    }
    let generation = GenerationEnd::contained();
    let cfg = test_config(ptr::null_mut());
    match unsafe { run_contained_worker(&generation, &cfg, ok_entry) } {
        Err(EndReason::RunStop(stop)) => {
            unsafe { write_name(name_buf, cap, stop.name()) };
            let mut cur: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(libc::SIGSEGV, ptr::null(), &mut cur) } != 0 {
                return -3;
            }
            if cur.sa_sigaction != host_dummy as *const () as usize {
                return -4;
            }
            1
        }
        _ => -1,
    }
}

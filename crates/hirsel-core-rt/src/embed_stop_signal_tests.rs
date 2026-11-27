//! Signal chaining, publication race, and replaced-handler laws for Slice 2s.
use super::{recurse_bytes, test_config};
use crate::{
    Context,
    fiber::{self, ArmError},
    process::{Config, MainResult},
    run_stop::{EndReason, Execution, run_contained_worker},
};
use std::{
    os::unix::process::ExitStatusExt,
    ptr,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

fn spawn_child(test_name: &str, mode: &str) -> std::process::Output {
    crate::test_process::command()
        .args(["--ignored", "--exact", test_name, "--test-threads=1"])
        .env("HIRSEL_EMBED_STOP_MODE", mode)
        .output()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Law 6: Publication race — every concurrent fault reaches host handler.
// ---------------------------------------------------------------------------
static PROBE_ADDR: AtomicUsize = AtomicUsize::new(0);
static PROBE_LEN: AtomicUsize = AtomicUsize::new(0);
static HOST_SEGV_HITS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn probe_host_handler(
    _: libc::c_int,
    info: *mut libc::siginfo_t,
    _: *mut libc::c_void,
) {
    // SAFETY: kernel supplies live siginfo_t; unprotecting our test probe page
    // is async-signal-safe (`mprotect`).
    unsafe {
        let addr = (*info).si_addr().addr();
        let base = PROBE_ADDR.load(Ordering::Acquire);
        let len = PROBE_LEN.load(Ordering::Acquire);
        if base != 0 && addr >= base && addr < base + len {
            HOST_SEGV_HITS.fetch_add(1, Ordering::AcqRel);
            libc::mprotect(
                ptr::with_exposed_provenance_mut(base),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
            );
            return;
        }
        libc::_exit(99);
    }
}

fn alloc_probe_page() -> (*mut u8, usize) {
    let page = fiber::try_page().unwrap();
    // SAFETY: anonymous private probe page.
    let raw = unsafe {
        libc::mmap(
            ptr::null_mut(),
            page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(raw, libc::MAP_FAILED);
    PROBE_ADDR.store(raw.addr(), Ordering::Release);
    PROBE_LEN.store(page, Ordering::Release);
    HOST_SEGV_HITS.store(0, Ordering::Release);
    (raw.cast(), page)
}

#[test]
fn publication_race_delivers_every_fault_to_host_handler() {
    let out = spawn_child("embed_stop_tests::signal::publication_race_child", "run");
    assert!(
        out.status.success(),
        "publication race failed: {out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "isolated publication race child, invoked by named parent"]
fn publication_race_child() {
    let (probe, page) = alloc_probe_page();
    // SAFETY: install host handler before arm_embedded.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_flags = libc::SA_SIGINFO;
        act.sa_sigaction = probe_host_handler as *const () as usize;
        libc::sigemptyset(&mut act.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
        #[cfg(target_vendor = "apple")]
        assert_eq!(libc::sigaction(libc::SIGBUS, &act, ptr::null_mut()), 0);
    }

    const ITERS: usize = 300;
    let started = Arc::new(AtomicBool::new(false));
    let started_bg = started.clone();
    let probe_addr = probe.addr();
    let faulter = std::thread::spawn(move || {
        let p = ptr::with_exposed_provenance_mut::<u8>(probe_addr);
        for i in 0..ITERS {
            // SAFETY: re-protect probe page and read to trigger real hardware fault.
            unsafe {
                assert_eq!(libc::mprotect(p.cast(), page, libc::PROT_NONE), 0);
                if i == 5 {
                    started_bg.store(true, Ordering::Release);
                }
                std::hint::black_box(p.read_volatile());
            }
        }
    });

    while !started.load(Ordering::Acquire) {
        std::hint::spin_loop();
    }
    fiber::arm_embedded().unwrap();
    faulter.join().unwrap();
    assert_eq!(HOST_SEGV_HITS.load(Ordering::Acquire), ITERS);
}

// ---------------------------------------------------------------------------
// Law 7: Unrelated faults and software signals chain with flags.
// ---------------------------------------------------------------------------
#[test]
fn unrelated_faults_and_software_signals_chain_with_flags() {
    // (a) Host thread fault while running AND after stop reaches host handler.
    let out = spawn_child(
        "embed_stop_tests::signal::chaining_child",
        "host_thread_during_and_after",
    );
    assert!(
        out.status.success(),
        "host_thread_during_and_after failed: {out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (b) SIG_DFL + synchronous non-Hirsel hardware fault -> SIGSEGV, not 102.
    let out = spawn_child(
        "embed_stop_tests::signal::chaining_child",
        "sig_dfl_hardware",
    );
    assert_ne!(out.status.code(), Some(102));
    assert!(
        out.status.signal() == Some(libc::SIGSEGV) || out.status.signal() == Some(libc::SIGBUS),
        "expected SIGSEGV/SIGBUS on SIG_DFL hardware fault: {out:?}"
    );

    // (c) SIG_DFL + software-raised SIGSEGV -> SIGSEGV, not 102 and never dropped.
    let out = spawn_child(
        "embed_stop_tests::signal::chaining_child",
        "sig_dfl_software",
    );
    assert_ne!(out.status.code(), Some(102));
    assert_eq!(
        out.status.signal(),
        Some(libc::SIGSEGV),
        "expected SIGSEGV on SIG_DFL software signal: {out:?}"
    );

    // (d) SIG_IGN + software-raised SIGSEGV -> returns cleanly & Hirsel still grows stacks.
    let out = spawn_child(
        "embed_stop_tests::signal::chaining_child",
        "sig_ign_software",
    );
    assert!(
        out.status.success(),
        "sig_ign_software failed: {out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (e) Custom handler with sa_mask(SIGUSR1) + SA_RESETHAND:
    //     first signal runs handler with SIGUSR1 blocked (restored after),
    //     Hirsel stack growth still works after, second signal dies with SIGSEGV.
    let out = spawn_child(
        "embed_stop_tests::signal::chaining_child",
        "sa_mask_and_resethand",
    );
    assert_eq!(
        out.status.signal(),
        Some(libc::SIGSEGV),
        "second signal after SA_RESETHAND must die with SIGSEGV: {out:?}"
    );
    assert_eq!(
        out.stderr, b"PHASE1_OK\n",
        "phase 1 checks before second signal failed: {out:?}"
    );
}

static MASK_SEEN_BLOCKED: AtomicUsize = AtomicUsize::new(0);
static RESETHAND_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn resethand_mask_handler(
    _: libc::c_int,
    _: *mut libc::siginfo_t,
    _: *mut libc::c_void,
) {
    // SAFETY: pthread_sigmask and sigismember are async-signal-safe.
    unsafe {
        let mut current: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_SETMASK, ptr::null(), &mut current);
        if libc::sigismember(&current, libc::SIGUSR1) == 1 {
            MASK_SEEN_BLOCKED.store(1, Ordering::SeqCst);
        }
        RESETHAND_CALLS.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe extern "C" fn grow_stack_entry(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
    // Touch past initial 64 KiB COMMIT to force a real stack growth fault.
    std::hint::black_box(recurse_bytes(160 << 10, 0));
    0
}

#[test]
#[ignore = "isolated signal chaining child, invoked by named parent"]
fn chaining_child() {
    let mode = std::env::var("HIRSEL_EMBED_STOP_MODE").unwrap();
    match mode.as_str() {
        "host_thread_during_and_after" => {
            let (probe, page) = alloc_probe_page();
            // SAFETY: install probe host handler before arming.
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_flags = libc::SA_SIGINFO;
                act.sa_sigaction = probe_host_handler as *const () as usize;
                libc::sigemptyset(&mut act.sa_mask);
                assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
                #[cfg(target_vendor = "apple")]
                assert_eq!(libc::sigaction(libc::SIGBUS, &act, ptr::null_mut()), 0);
            }
            struct Sync {
                entered: Barrier,
                release: Barrier,
            }
            unsafe extern "C" fn hold_entry(
                _: *mut Context,
                cfg: *const Config,
                _: *mut MainResult,
            ) -> u8 {
                // SAFETY: Sync lives until worker thread joins.
                unsafe {
                    let s = &*(*cfg).state.cast::<Sync>();
                    s.entered.wait();
                    s.release.wait();
                }
                0
            }
            let sync = Arc::new(Sync {
                entered: Barrier::new(2),
                release: Barrier::new(2),
            });
            let sync_w = sync.clone();
            let generation = Arc::new(Execution::contained());
            let gen_w = generation.clone();
            let worker = std::thread::spawn(move || {
                let cfg = test_config(Arc::as_ptr(&sync_w).cast_mut().cast());
                // SAFETY: gen_w and sync_w stay pinned until return.
                unsafe { run_contained_worker(&gen_w, &cfg, hold_entry) }
            });
            sync.entered.wait();
            // Fault on host thread WHILE contained worker is running.
            unsafe {
                assert_eq!(libc::mprotect(probe.cast(), page, libc::PROT_NONE), 0);
                std::hint::black_box(probe.read_volatile());
            }
            assert_eq!(HOST_SEGV_HITS.load(Ordering::Acquire), 1);
            sync.release.wait();
            assert_eq!(worker.join().unwrap(), Ok(0));
            // Fault on host thread AFTER contained worker stopped.
            unsafe {
                assert_eq!(libc::mprotect(probe.cast(), page, libc::PROT_NONE), 0);
                std::hint::black_box(probe.read_volatile());
            }
            assert_eq!(HOST_SEGV_HITS.load(Ordering::Acquire), 2);
        }
        "sig_dfl_hardware" => {
            let (probe, page) = alloc_probe_page();
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut act.sa_mask);
                assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
                #[cfg(target_vendor = "apple")]
                assert_eq!(libc::sigaction(libc::SIGBUS, &act, ptr::null_mut()), 0);
                fiber::arm_embedded().unwrap();
                assert_eq!(libc::mprotect(probe.cast(), page, libc::PROT_NONE), 0);
                std::hint::black_box(probe.read_volatile());
            }
            panic!("SIG_DFL hardware fault returned");
        }
        "sig_dfl_software" => {
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut act.sa_mask);
                assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
                fiber::arm_embedded().unwrap();
                libc::raise(libc::SIGSEGV);
            }
            panic!("SIG_DFL software SIGSEGV was silently dropped");
        }
        "sig_ign_software" => {
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = libc::SIG_IGN;
                libc::sigemptyset(&mut act.sa_mask);
                assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
                fiber::arm_embedded().unwrap();
                assert_eq!(libc::raise(libc::SIGSEGV), 0);
            }
            let generation = Execution::contained();
            let cfg = test_config(ptr::null_mut());
            // SAFETY: stack growth must still succeed after SIG_IGN software signal.
            let res = unsafe { run_contained_worker(&generation, &cfg, grow_stack_entry) };
            assert_eq!(res, Ok(0));
        }
        "sa_mask_and_resethand" => {
            unsafe {
                // Ensure SIGUSR1 starts unblocked.
                let mut unblk: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut unblk);
                libc::sigaddset(&mut unblk, libc::SIGUSR1);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblk, ptr::null_mut());

                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_flags = libc::SA_SIGINFO | libc::SA_RESETHAND;
                act.sa_sigaction = resethand_mask_handler as *const () as usize;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaddset(&mut act.sa_mask, libc::SIGUSR1);
                assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);

                fiber::arm_embedded().unwrap();
                assert_eq!(libc::raise(libc::SIGSEGV), 0);

                assert_eq!(RESETHAND_CALLS.load(Ordering::SeqCst), 1);
                assert_eq!(
                    MASK_SEEN_BLOCKED.load(Ordering::SeqCst),
                    1,
                    "sa_mask signal SIGUSR1 was not blocked inside chained handler"
                );
                // Verify SIGUSR1 was restored (unblocked) after return.
                let mut after_mask: libc::sigset_t = std::mem::zeroed();
                libc::pthread_sigmask(libc::SIG_SETMASK, ptr::null(), &mut after_mask);
                assert_eq!(libc::sigismember(&after_mask, libc::SIGUSR1), 0);
            }
            // Verify Hirsel's handler stayed installed and still grows fiber stacks!
            let generation = Execution::contained();
            let cfg = test_config(ptr::null_mut());
            let res = unsafe { run_contained_worker(&generation, &cfg, grow_stack_entry) };
            assert_eq!(res, Ok(0));

            unsafe {
                libc::write(2, b"PHASE1_OK\n".as_ptr().cast(), 10);
                // Second signal must fall back to SIG_DFL and kill the child.
                libc::raise(libc::SIGSEGV);
            }
            panic!("second SIGSEGV after SA_RESETHAND returned");
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Law 8: Replaced handler is refused without reinstalling.
// ---------------------------------------------------------------------------
#[test]
fn replaced_fault_handler_is_refused_without_reinstalling() {
    let out = spawn_child("embed_stop_tests::signal::replaced_handler_child", "run");
    assert!(
        out.status.success(),
        "replaced_handler_child failed: {out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "isolated replaced handler child, invoked by named parent"]
fn replaced_handler_child() {
    unsafe extern "C" fn host_replacement(_: libc::c_int) {}
    unsafe extern "C" fn ok_entry(_: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        0
    }
    let gen1 = Execution::contained();
    let cfg = test_config(ptr::null_mut());
    assert_eq!(
        unsafe { run_contained_worker(&gen1, &cfg, ok_entry) },
        Ok(0)
    );

    // Host replaces SIGSEGV action between generations.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = host_replacement as *const () as usize;
        libc::sigemptyset(&mut act.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGSEGV, &act, ptr::null_mut()), 0);
    }

    assert_eq!(fiber::arm_embedded(), Err(ArmError::FaultHandlerReplaced));
    let gen2 = Execution::contained();
    match unsafe { run_contained_worker(&gen2, &cfg, ok_entry) } {
        Err(EndReason::RunStop(stop)) => assert_eq!(stop.name(), "FaultHandlerReplaced"),
        other => panic!("expected FaultHandlerReplaced, got {other:?}"),
    }

    // Host's action must be untouched (never reinstalled).
    unsafe {
        let mut cur: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(libc::SIGSEGV, ptr::null(), &mut cur), 0);
        assert_eq!(cur.sa_sigaction, host_replacement as *const () as usize);
    }
}

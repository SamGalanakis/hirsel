//! Platform-specific completion checks for the shared-page law.
use super::*;

#[cfg(target_os = "linux")]
pub(super) fn check(loader: &Loader, replacement: Code<Leaf>) {
    assert_eq!(loader.mappings(), Some((2, 0)));
    host::check_alias_lines();
    let bytes = image(8).encode();
    let expected = loader.installed();
    let at = replacement.text();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let insn = |code, jf, k| libc::sock_filter { code, jt: 0, jf, k };
                let filter = [
                    insn((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0, 0),
                    insn(
                        (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                        1,
                        libc::SYS_membarrier as u32,
                    ),
                    insn(
                        (libc::BPF_RET | libc::BPF_K) as u16,
                        0,
                        libc::SECCOMP_RET_ERRNO | libc::EIO as u32,
                    ),
                    insn(
                        (libc::BPF_RET | libc::BPF_K) as u16,
                        0,
                        libc::SECCOMP_RET_ALLOW,
                    ),
                ];
                let program = libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_ptr().cast_mut(),
                };
                // SAFETY: the kernel validates and copies this thread-local filter.
                unsafe {
                    assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
                    assert_eq!(
                        libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
                        0
                    );
                }
                // The live replacement still runs but a fresh publication fails.
                assert_eq!(load(loader, &bytes).err(), Some(Refusal::Unsynchronized));
                assert_eq!(call(&replacement, 1), 9);
                drop(replacement);
                assert_eq!(loader.reclaim(), Err(Refusal::Unsynchronized));
                // The dead text resolves to nothing, and while this thread's host
                // refuses to synchronize it, its cohort keeps its run and its bytes.
                assert_eq!(loader.resolve(at), None);
                assert!(loader.installed() >= expected);
                assert!(loader.unrevoked() >= expected - 24);
            })
            .join()
            .unwrap();
    });
    // The refusal was that thread's. This one's reclaim synchronizes the withheld
    // text, its cohorts are recycled, and the pool loads again.
    assert_eq!(loader.reclaim(), Ok(()));
    assert_eq!((loader.unrevoked(), loader.installed()), (0, 0));
    assert_eq!(call(&load(loader, &bytes).unwrap(), 1), 10);
}

#[cfg(target_os = "macos")]
pub(super) fn check(loader: &Loader, replacement: Code<Leaf>) {
    // A thread publishes while it is inside a call of loaded code: no thread holds
    // anything a publication waits for.
    let bytes = image(8).encode();
    let fresh = replacement.with_entry(|entry| {
        // SAFETY: `replacement` is live through the call.
        assert_eq!(unsafe { entry(1) }, 9);
        load(loader, &bytes)
    });
    assert_eq!(call(&fresh.unwrap(), 1), 10);
    drop(replacement);
    assert_eq!(loader.reclaim(), Ok(()));
    assert_eq!(loader.installed(), 0);
}

/// PACK-5: a publication stops a thread that is running user code, and one that exits
/// meanwhile does not refuse it.
#[cfg(target_os = "macos")]
#[test]
fn a_publication_stops_running_threads_and_outlasts_exiting_ones() {
    use std::os::unix::thread::JoinHandleExt;
    use std::sync::atomic::AtomicBool;
    let running = Arc::new(AtomicBool::new(true));
    let spin = running.clone();
    let spinner = std::thread::spawn(move || {
        while spin.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
    });
    // SAFETY: the handle's thread is live.
    let port = unsafe { libc::pthread_mach_thread_np(spinner.as_pthread_t()) };
    assert_eq!(host::stopped(port), 0);
    let churn: Vec<_> = (0..2)
        .map(|_| {
            let churn = running.clone();
            std::thread::spawn(move || {
                while churn.load(Ordering::Acquire) {
                    std::thread::spawn(|| ()).join().unwrap();
                }
            })
        })
        .collect();
    for _ in 0..2000 {
        host::synchronize().unwrap();
    }
    running.store(false, Ordering::Release);
    spinner.join().unwrap();
    churn.into_iter().for_each(|thread| thread.join().unwrap());
}

#[cfg(target_os = "ios")]
pub(super) fn check(_: &Loader, _: Code<Leaf>) {
    unreachable!("iOS has no slab")
}

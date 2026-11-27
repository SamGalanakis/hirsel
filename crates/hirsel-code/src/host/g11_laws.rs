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
                assert_eq!(load(loader, &bytes).err(), Some(Refusal::Unsupported));
                assert_eq!(call(&replacement, 1), 9);
                drop(replacement);
                assert_eq!(loader.reclaim(), Err(Refusal::Unsupported));
            })
            .join()
            .unwrap();
    });
    assert_eq!(loader.resolve(at), None);
    assert!(loader.installed() >= expected);
    assert!(loader.unrevoked() >= expected - 24);
    assert_eq!(load(loader, &bytes).err(), Some(Refusal::Unsupported));
}

#[cfg(target_os = "macos")]
pub(super) fn check(loader: &Loader, replacement: Code<Leaf>) {
    assert!(matches!(Loader::new(config()), Err(Refusal::Unsupported)));
    let (entered, active) = std::sync::mpsc::channel();
    let (release, hold) = std::sync::mpsc::channel();
    let (invalidated, waiting) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let neighbor = scope.spawn(move || {
            Loader::on_executor(|| {
                entered.send(()).unwrap();
                hold.recv().unwrap();
            })
        });
        active.recv().unwrap();
        drop(replacement);
        let reclaim = scope.spawn(move || {
            host::BEFORE_ACK.set(Some(invalidated));
            loader.reclaim()
        });
        waiting.recv().unwrap();
        assert!(!reclaim.is_finished());
        release.send(()).unwrap();
        neighbor.join().unwrap();
        assert_eq!(reclaim.join().unwrap(), Ok(()));
    });
    assert_eq!(loader.installed(), 0);
}

#[cfg(target_os = "ios")]
pub(super) fn check(_: &Loader, _: Code<Leaf>) {
    unreachable!("iOS has no slab")
}

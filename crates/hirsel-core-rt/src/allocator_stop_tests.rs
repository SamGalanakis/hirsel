//! Isolated checks for allocator stops and checked diagnostics.
#[test]
#[ignore = "isolated native refusal, invoked by the named parent"]
fn native_refusal_child() {
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            // SAFETY: static marker. A run stop must skip this destructor.
            unsafe {
                libc::write(2, b"CLEANUP\n".as_ptr().cast(), 8);
            }
        }
    }
    let _cleanup = Cleanup;
    let ordinary = std::env::var_os("HIRSEL_TEST_ORDINARY_NATIVE_ALLOC").is_some();
    let context = ordinary.then(|| crate::Context::new(8));
    let denied = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: lower only this isolated child's limits. Native Linux refuses the
    // 64MiB request. Pinned qemu-user accepts guest RLIMIT_AS=0 but leaves the
    // guest limit infinite, and Darwin refuses to set it, so ARM instead requests
    // a valid 1EiB Layout beyond its address space. Both exercise a real
    // allocator null, never a test hook.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &denied) }, 0);
    #[cfg(not(target_arch = "aarch64"))]
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_AS, &denied) }, 0);
    #[cfg(target_arch = "aarch64")]
    let bytes = 1usize << 60;
    #[cfg(not(target_arch = "aarch64"))]
    let bytes = 64 << 20;
    if let Some(context) = context {
        // The language limit admits eight logical bytes. The physical mapping
        // then stops the run without returning a pointer or running cleanup.
        std::hint::black_box(crate::block::Block::new(&context, Some(8), bytes, 8));
    } else {
        std::hint::black_box(super::allocate_raw(
            std::alloc::Layout::from_size_align(bytes, 8).unwrap(),
        ));
    }
    panic!("allocator refusal returned");
}
#[test]
fn fail15_native_allocator_refusal_is_a_reported_run_stop_without_cleanup() {
    for ordinary in [false, true] {
        let mut child = crate::test_process::command();
        child.args([
            "--ignored",
            "--exact",
            "allocator::stop_tests::native_refusal_child",
            "--test-threads=1",
        ]);
        if ordinary {
            child.env("HIRSEL_TEST_ORDINARY_NATIVE_ALLOC", "1");
        } else {
            child.env_remove("HIRSEL_TEST_ORDINARY_NATIVE_ALLOC");
        }
        let output = child.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(102),
            "{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stderr, b"run stop: HostMemory\n");
    }
}

#[test]
#[ignore = "isolated checked allocator diagnostic, invoked by the named parent"]
fn checked_allocator_child() {
    let pointer = super::allocate_raw(std::alloc::Layout::new::<u64>());
    if std::env::var_os("HIRSEL_TEST_CHECKED_LEAK").is_some() {
        return;
    }
    // SAFETY: the second call deliberately exercises checked mode's non-live
    // address diagnostic. The first call transferred the live allocation.
    unsafe {
        super::deallocate_raw(pointer);
        super::deallocate_raw(pointer);
    }
    panic!("checked free of a non-live address returned");
}

#[test]
fn checked_allocator_reports_the_original_site() {
    const CHILD: &str = "allocator::stop_tests::checked_allocator_child";
    for leak in [false, true] {
        let mut child = crate::test_process::command();
        child
            .args(["--ignored", "--exact", CHILD, "--test-threads=1"])
            .env("HIRSEL_ALLOC", "checked")
            .env_remove("HIRSEL_TEST_CHECKED_LEAK");
        if leak {
            child.env("HIRSEL_TEST_CHECKED_LEAK", "1");
        }
        let output = child.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = if leak {
            "checked allocator: leaked 8 bytes"
        } else {
            "checked allocator: free of non-live block"
        };
        assert_eq!(output.status.success(), leak, "{stderr}");
        assert!(stderr.contains(message), "{stderr}");
        assert!(stderr.contains("allocated at:"), "{stderr}");
        assert!(stderr.contains("checked_allocator_child"), "{stderr}");
    }
}

//! FAIL-15: native allocation failure is a named run stop, never Rust abort.
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
    let denied = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: lower only this isolated child's virtual-address limit. The64MiB
    // request is below the guard budget and commits no memory: malloc returns null.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &denied) }, 0);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_AS, &denied) }, 0);
    if ordinary {
        // SAFETY: valid positive native size, ordinary alignment; this physical
        // refusal ends the run, so there is no returned payload to publish.
        std::hint::black_box(unsafe { crate::abi::hirsel_core_native_alloc_ordinary(64 << 20) });
    } else {
        std::hint::black_box(super::allocate(
            std::alloc::Layout::from_size_align(64 << 20, 8).unwrap(),
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

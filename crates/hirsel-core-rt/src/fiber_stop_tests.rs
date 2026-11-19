#[test]
#[ignore = "isolated startup stop, invoked by the named parent"]
fn startup_stop_child() {
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            // SAFETY: static witness; stops must bypass this destructor.
            unsafe { libc::write(2, b"CLEANUP\n".as_ptr().cast(), 8) };
        }
    }
    let _cleanup = Cleanup;
    match std::env::var("HIRSEL_STARTUP_STOP").unwrap().as_str() {
        "negative page" => {
            std::hint::black_box(super::page_bytes(-1));
        }
        "zero page" => {
            std::hint::black_box(super::page_bytes(0));
        }
        "missing action" => {
            // SAFETY: initialized siginfo/context for direct handler testing;
            // no action has been installed in this isolated child.
            unsafe {
                let mut info = std::mem::zeroed();
                super::fault(libc::SIGSEGV, &mut info, std::ptr::null_mut());
            }
        }
        _ => unreachable!(),
    }
    panic!("startup stop returned");
}

#[test]
fn host_startup_stops_have_exact_names_and_skip_cleanup() {
    assert_eq!(super::page_bytes(4096), 4096);
    for (mode, expected) in [
        ("negative page", b"run stop: PageSize\n".as_slice()),
        ("zero page", b"run stop: PageSize\n".as_slice()),
        ("missing action", b"run stop: Signal\n".as_slice()),
    ] {
        let output = crate::test_process::command()
            .args([
                "--ignored",
                "--exact",
                "fiber::stop_tests::startup_stop_child",
                "--test-threads=1",
            ])
            .env("HIRSEL_STARTUP_STOP", mode)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(102), "{mode}: {output:?}");
        assert_eq!(output.stderr, expected, "{mode}");
    }
}

/// A touch deep in a stack's reservation commits it; recycling a stack that grew
/// drops those pages (the reused stack reads zero there) and lets it grow again.
/// Without commit-on-touch the first write is an unrelated fault; a trim that
/// unmaps but keeps the low-water word makes the second touch one.
#[test]
fn grown_stack_trims_on_recycle_and_grows_again() {
    super::arm();
    let mut stock = super::Stock::default();
    let touch = |stack: &super::Stack, value: Option<u64>| {
        super::guard(stack.bounds());
        // SAFETY: half way down the current stack's reservation, above its guard page.
        let deep = unsafe { stack.base.as_ptr().add(stack.len / 2).cast::<u64>() };
        // SAFETY: the fault handler commits the page before the access resumes.
        let seen = unsafe {
            let seen = deep.read_volatile();
            if let Some(value) = value {
                deep.write_volatile(value);
            }
            seen
        };
        super::guard((0, 0));
        (deep.addr(), seen)
    };
    let stack = stock.take();
    let (first, _) = touch(&stack, Some(7));
    stock.recycle(stack);
    let stack = stock.take();
    let (again, seen) = touch(&stack, None);
    assert_eq!(again, first, "the recycled stack is reused");
    assert_eq!(seen, 0, "the trim dropped the grown page");
}

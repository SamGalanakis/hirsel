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
        "refused growth" => {
            super::arm();
            let stack = super::Stack::new();
            super::guard(stack.bounds());
            let none = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: lower only this isolated child's limits. A zero data limit
            // refuses the commit-on-touch mprotect of private writable pages.
            unsafe {
                assert_eq!(libc::setrlimit(libc::RLIMIT_CORE, &none), 0);
                assert_eq!(libc::setrlimit(libc::RLIMIT_DATA, &none), 0);
            }
            // SAFETY: half way down the current stack's reservation, above its
            // guard page; the fault handler either commits it or stops the run.
            unsafe {
                stack
                    .base
                    .as_ptr()
                    .add(stack.len / 2)
                    .cast::<u64>()
                    .write_volatile(7);
            }
        }
        _ => unreachable!(),
    }
    panic!("startup stop returned");
}

#[test]
fn host_stops_have_exact_names_and_skip_cleanup() {
    assert_eq!(super::page_bytes(4096), 4096);
    let mut modes = vec![
        ("negative page", b"run stop: PageSize\n".as_slice()),
        ("zero page", b"run stop: PageSize\n".as_slice()),
        ("missing action", b"run stop: Signal\n".as_slice()),
    ];
    // A host refusing a stack's growth is HostMemory (FAIL-15), not a fault.
    // qemu-user and Darwin ignore the data limit, so x86-64 Linux runs that case.
    match crate::host::HOST_TARGET {
        hirsel_core_layout::Target::X86_64Linux => {
            modes.push(("refused growth", b"run stop: HostMemory\n".as_slice()));
        }
        hirsel_core_layout::Target::Aarch64Linux
        | hirsel_core_layout::Target::Aarch64Darwin
        | hirsel_core_layout::Target::Aarch64Ios
        | hirsel_core_layout::Target::Aarch64IosSim => {}
    }
    for (mode, expected) in modes {
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

/// Law: recycling never unmaps, and a recycled stack comes back as the same
/// guarded reservation with only its first `COMMIT` writable, whether the
/// retention allowance kept it or it waits as surplus; releasing the surplus
/// leaves the allowance holding exactly the kept stacks.
///
/// A touch deep in a stack's reservation commits it, so recycling a stack that
/// grew must drop those pages (the reused stack reads zero there) and let it
/// grow again. Without commit-on-touch the first write is an unrelated fault; a
/// trim that unmaps but keeps the low-water word makes the second touch one; a
/// recycle that trims only the stacks it keeps hands a grown surplus stack, with
/// the last task's data, to the next spawn; a surplus stack that keeps or leaks
/// a retention share leaves the allowance unbalanced.
#[test]
fn recycled_stacks_come_back_trimmed_whether_kept_or_surplus() {
    super::arm();
    let page = super::page();
    let one = page + super::COMMIT;
    let budget: &'static _ = Box::leak(Box::new(crate::retention::Budget::with_headroom(one)));
    let mut stock = super::Stock::with_budget(budget);
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
        super::guard((0, 0, 0));
        seen
    };
    let shape = |stack: &super::Stack| {
        let (base, guard_end, _) = stack.bounds();
        // SAFETY: an idle stack's top word is always writable.
        let low = unsafe { *super::low_word(stack.top()) };
        (stack.len, guard_end - base, stack.top() - low)
    };
    let fresh = (page + super::RESERVE, page, super::COMMIT);
    let mut mapped: Vec<usize> = Vec::new();
    for round in 0..2 {
        let stacks: Vec<_> = (0..3).map(|_| stock.take()).collect();
        assert_eq!(
            budget.used(),
            0,
            "round {round}: taking releases each share"
        );
        let mut bases: Vec<usize> = stacks.iter().map(|stack| stack.bounds().0).collect();
        bases.sort_unstable();
        if round == 0 {
            mapped = bases;
        } else {
            assert_eq!(
                bases, mapped,
                "the kept and surplus stacks are reused, none mapped"
            );
        }
        for stack in &stacks {
            assert_eq!(shape(stack), fresh, "round {round}: size, guard and commit");
            assert_eq!(
                touch(stack, Some(7)),
                0,
                "round {round}: no earlier task's data"
            );
        }
        for stack in stacks {
            stock.recycle(stack);
        }
        assert_eq!((stock.kept.len(), stock.surplus.len()), (1, 2));
        assert_eq!(budget.used(), one, "only the kept stack holds a share");
    }
    stock.release();
    assert_eq!((stock.kept.len(), stock.surplus.len()), (1, 0));
    assert_eq!(
        budget.used(),
        one,
        "releasing the surplus keeps the kept share"
    );
    let stack = stock.take();
    assert!(mapped.contains(&stack.bounds().0));
    assert_eq!(touch(&stack, None), 0, "the kept stack was trimmed too");
    drop(stack);
    drop(stock);
    assert_eq!(budget.used(), 0);
}

#[test]
fn network_idle_returns_cached_stack_pages_and_keeps_the_reservations() {
    let page = super::page();
    if page == super::COMMIT {
        return;
    }
    let reservation = page + super::COMMIT;
    let budget = Box::leak(Box::new(crate::retention::Budget::with_headroom(
        2 * reservation,
    )));
    let mut stock = super::Stock::with_budget(budget);
    let stack = stock.take();
    let other = stock.take();
    // SAFETY: the test owns the two idle stacks and these pages are writable.
    let bottom = unsafe { stack.base.as_ptr().add(stack.len - super::COMMIT) };
    let other_bottom = unsafe { other.base.as_ptr().add(other.len - super::COMMIT) };
    unsafe {
        bottom.write_volatile(7);
        other_bottom.write_volatile(9);
    }
    stock.recycle(stack);
    stock.recycle(other);
    assert!(stock.has_warm_kept());
    stock.cool_kept();
    assert!(!stock.has_warm_kept());
    assert_eq!(budget.used(), 2 * reservation);

    let other = stock.take();
    let stack = stock.take();
    assert_eq!(stack.top() - super::COMMIT, bottom.addr());
    assert_eq!(other.top() - super::COMMIT, other_bottom.addr());
    // SAFETY: take retained the mappings. A discarded private anonymous page
    // reads zero on its next access, and the header page is intact.
    unsafe {
        assert_eq!(bottom.read_volatile(), 0);
        assert_eq!(other_bottom.read_volatile(), 0);
        assert_eq!(super::low_word(stack.top()).read(), bottom.addr());
        bottom.write_volatile(11);
    }
    stock.recycle(stack);
    assert!(stock.has_warm_kept());
    stock.cool_kept();
    let stack = stock.take();
    assert_eq!(stack.top() - super::COMMIT, bottom.addr());
    // SAFETY: the retained mapping still owns this writable page.
    assert_eq!(unsafe { bottom.read_volatile() }, 0);
}

#[test]
fn discarding_below_a_parked_stack_keeps_its_live_frames() {
    if super::page() == super::COMMIT {
        return;
    }
    struct Parked {
        saved: *mut u8,
        caller: *mut u8,
        bottom: *mut u8,
        result: u64,
    }
    unsafe extern "C" fn body(first: *mut u8, _: *mut u8) -> ! {
        let state = first.cast::<Parked>();
        let live = 23_u64;
        // SAFETY: the pinned state and both stacks outlive these switches.
        // The bottom byte is scratch. The local remains live across the park.
        unsafe {
            (*state).bottom.write_volatile(9);
            hirsel_stack::switch(&raw mut (*state).saved, (*state).caller);
            (*state).result = (&raw const live).read_volatile();
            hirsel_stack::switch(&raw mut (*state).saved, (*state).caller);
        }
        unreachable!()
    }

    let stack = super::Stack::new();
    // SAFETY: this address lies in the initial writable stack extent.
    let bottom = unsafe { stack.base.as_ptr().add(stack.len - super::COMMIT) };
    let mut state = Parked {
        saved: std::ptr::null_mut(),
        caller: std::ptr::null_mut(),
        bottom,
        result: 0,
    };
    // SAFETY: state lives until both switches return. Only one stack runs at
    // a time. The first return owns a suspended frame above its saved SP.
    unsafe {
        let first = stack.prepare(body, (&raw mut state).cast(), std::ptr::null_mut());
        hirsel_stack::switch(&raw mut state.caller, first);
        assert_eq!(bottom.read_volatile(), 9);
        stack.discard_below(state.saved.addr());
        assert_eq!(bottom.read_volatile(), 0);
        hirsel_stack::switch(&raw mut state.caller, state.saved);
    }
    assert_eq!(state.result, 23);
}

/// Law: a call onto another stack from anywhere in a task's stack returns,
/// even when the switch's register save is the first touch below the caller's
/// writable part. That save must grow the caller's stack like any other touch,
/// so the guard still names the caller while the switch writes there.
///
/// The child starts a body at each 16-byte offset over 16 KiB above a fresh
/// stack's commit edge and calls onto a second stack from it, so one offset
/// puts the switch's save just below the edge. A guard that named the callee
/// first sends that fault to the default handler: the child dies of SIGSEGV
/// (a native law whose task called a foreign function 64 KiB deep did).
#[test]
fn a_call_from_a_task_at_its_commit_edge_grows_the_caller() {
    let output = crate::test_process::command()
        .args([
            "--ignored",
            "--exact",
            "fiber::stop_tests::commit_edge_call_child",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[ignore = "isolated commit-edge calls, invoked by the named parent"]
fn commit_edge_call_child() {
    struct Edge {
        callee: *const super::Stack,
        caller: *mut u8,
        result: u64,
    }
    unsafe extern "C" fn body(first: *mut u8, _: *mut u8) -> ! {
        let edge = first.cast::<Edge>();
        // SAFETY: the parent pins the state and both stacks until it resumes;
        // the callee stack is idle and the call does not switch.
        unsafe {
            (*edge).result = (*(*edge).callee).call(|| std::hint::black_box(7_u64));
            let mut discard = std::ptr::null_mut();
            hirsel_stack::switch(&mut discard, (*edge).caller);
        }
        unreachable!()
    }
    super::arm();
    for offset in (0..16 << 10).step_by(16) {
        let caller = super::Stack::new();
        let callee = super::Stack::new();
        let mut edge = Edge {
            callee: &callee,
            caller: std::ptr::null_mut(),
            result: 0,
        };
        let words = hirsel_stack::initial_frame(body, (&raw mut edge).cast(), std::ptr::null_mut());
        let edge_address = caller.top() - super::COMMIT;
        // SAFETY: the frame lies in the caller's writable part, 16-aligned.
        // Only one stack runs at a time, and the body switches back once.
        unsafe {
            let start = std::ptr::with_exposed_provenance_mut::<u8>(edge_address + offset);
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), start, size_of_val(&words));
            super::guard(caller.bounds());
            hirsel_stack::switch(&raw mut edge.caller, start);
        }
        super::guard((0, 0, 0));
        assert_eq!(edge.result, 7, "offset {offset}");
    }
}

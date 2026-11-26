use crate::{
    Context,
    abi_host::*,
    abi_tasks::{hirsel_core_cancel, hirsel_core_join, hirsel_core_spawn},
    entry::Grant,
    process::*,
    provider::*,
};
use std::{
    fs::File,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

#[derive(Clone, Copy)]
enum Scenario {
    Exercise,
    Unavailable,
    Echo(u64),
    SmallAllocation,
    Temp(usize),
    Bounds(usize),
    Cancel,
    SleepCancel,
    SleepMeasure(u64),
    SleepAlreadyReached,
}
struct State {
    os: Option<Box<crate::os::OsProvider>>,
    directory: u64,
    scenario: Scenario,
    issued: Vec<u8>,
    delivered: Vec<u8>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    diagnostics: Vec<u8>,
    observed: Vec<u64>,
    created: bool,
    made: bool,
    close_next_read: bool,
}
unsafe fn path<'a>(r: &Request) -> &'a [u8] {
    if r.path_len == 0 {
        &[]
    } else {
        // SAFETY: fixture only borrows a request's pinned path during issue.
        unsafe { std::slice::from_raw_parts(r.path, r.path_len) }
    }
}
unsafe fn input<'a>(b: Buffer) -> &'a [u8] {
    if b.len == 0 {
        &[]
    } else {
        // SAFETY: caller pins exactly the readable extent, capacity may be zero.
        unsafe { std::slice::from_raw_parts(b.data, b.len) }
    }
}
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: issue exclusively borrows fixture state; no callback re-enters it.
    unsafe {
        let s = &mut *state.cast::<State>();
        let r = &mut *request;
        s.issued.push(r.operation);
        if let Some(os) = &mut s.os {
            if r.operation == READ && s.close_next_read {
                s.close_next_read = false;
                // External host closure makes the still-owned designation Closed.
                assert_eq!(libc::close(os.file_descriptor(r.handle)), 0);
            }
            let provider = os.provider();
            return (provider.issue)(provider.state, r, 0).outcome;
        }
        let mut out = Outcome::default();
        if matches!(s.scenario, Scenario::Unavailable) && r.operation == OPEN {
            return Outcome {
                error: IoError::from_errno(libc::ENOSYS),
                ..out
            };
        }
        match r.operation {
            OPEN => {
                assert_eq!(r.handle, 7);
                out = match path(r) {
                    b"missing" => Outcome {
                        error: IoError::from_errno(libc::ENOENT),
                        ..out
                    },
                    b"denied" => Outcome {
                        error: IoError::from_errno(libc::EACCES),
                        ..out
                    },
                    b"log" if r.mode == 3 && s.created => Outcome {
                        error: IoError::from_errno(libc::EEXIST),
                        ..out
                    },
                    b"log" => {
                        s.created = true;
                        Outcome { value: 71, ..out }
                    }
                    _ => panic!("unexpected model path"),
                };
            }
            READ => {
                assert_eq!(r.handle, 71);
                if s.close_next_read {
                    s.close_next_read = false;
                    out.error = IoError::from_errno(libc::EBADF);
                } else if r.buffer.capacity > r.buffer.len {
                    ptr::copy_nonoverlapping(b"hi".as_ptr(), r.buffer.data.add(r.buffer.len), 2);
                    r.buffer.len += 2;
                    out.value = 2;
                }
            }
            WRITE => {
                assert_eq!(r.handle, 71);
                assert_eq!(input(r.buffer), b"hi");
                out.written = 2;
            }
            CLOSE => {
                assert_eq!(r.handle, 71);
            }
            TRY_LOCK => {
                assert_eq!(r.handle, 71);
                out.value = 1;
            }
            UNLOCK => {
                assert_eq!(r.handle, 71);
            }
            MKDIR => {
                assert_eq!((r.handle, path(r)), (8, b"x".as_slice()));
                if s.made {
                    out.error = IoError::from_errno(libc::EEXIST);
                }
                s.made = true;
            }
            MKDTEMP => {
                assert_eq!(r.handle, 7);
                ptr::copy_nonoverlapping(b"t0".as_ptr(), r.buffer.data.add(r.buffer.len), 2);
                r.buffer.len += 2;
                out.value = 8;
            }
            REMOVE => {
                assert_eq!((r.handle, path(r)), (8, b"x".as_slice()));
            }
            REMOVE_TREE => {
                assert_eq!((r.handle, path(r)), (7, b"t0".as_slice()));
            }
            WALL_MS => out.value = 37,
            MONO_NS => out.value = 41,
            SLEEP_UNTIL => out.value = 0,
            STDOUT => {
                s.stdout.extend_from_slice(input(r.buffer));
                out.written = r.buffer.len as u64;
            }
            STDERR => {
                s.stderr.extend_from_slice(input(r.buffer));
                out.written = r.buffer.len as u64;
            }
            _ => unreachable!(),
        }
        out
    }
}
unsafe extern "C" fn wait(state: *mut u8, request: *mut Request, token: u64) -> Wait {
    unsafe {
        (*state.cast::<State>()).os.as_mut().map_or(
            Wait {
                kind: WAIT_READY,
                deadline_ns: 0,
            },
            |os| {
                let provider = os.provider();
                (provider.wait)(provider.state, request, token)
            },
        )
    }
}
unsafe extern "C" fn step_issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
    Step {
        kind: STEP_DONE,
        outcome: issue(s, r),
        deadline_ns: 0,
    }
}
unsafe extern "C" fn deliver(state: *mut u8, r: *mut Request, out: Outcome) -> Outcome {
    // SAFETY: recording delivery borrows only callback metadata, not carrier ownership.
    unsafe {
        (*state.cast::<State>()).delivered.push((*r).operation);
    }
    out
}
unsafe extern "C" fn diagnostic(state: *mut u8, p: *const u8, n: usize) {
    // SAFETY: run pins its diagnostic bytes through this callback.
    unsafe {
        (*state.cast::<State>())
            .diagnostics
            .extend_from_slice(std::slice::from_raw_parts(p, n));
    }
}
fn literal(bytes: &[u8]) -> Buffer {
    Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: 0,
    }
}
unsafe fn call(
    ctx: *mut Context,
    op: u8,
    handle: u64,
    operand: Option<&mut Buffer>,
    mode: u8,
) -> Outcome {
    // SAFETY: each native fixture call supplies that row's checked handle/carrier.
    unsafe {
        hirsel_core_host_op(
            ctx,
            op,
            handle,
            operand.map_or(ptr::null_mut(), |b| b),
            mode,
            123,
        )
    }
}
unsafe fn echo(ctx: *mut Context, index: u64) -> (u64, u64) {
    // SAFETY: Args/Stdout are the run's declared handles; returned carrier is read-only.
    unsafe {
        let len = hirsel_core_args_len(ctx, 5);
        let arg = hirsel_core_args_get(ctx, 5, index);
        let written = if arg.is_null() {
            0
        } else {
            assert_eq!((*arg).capacity, 0);
            assert_eq!(arg, hirsel_core_args_get(ctx, 5, index));
            let out = hirsel_core_host_op(ctx, STDOUT, 3, arg.cast_mut(), 0, 123);
            assert_eq!((out.status, out.error), (0, IoError::OK));
            out.written
        };
        (len, written)
    }
}
unsafe fn files(ctx: *mut Context, state: *mut State, small: bool) -> u8 {
    // SAFETY: native equivalent of providers.core f1, including explicit own cleanup.
    unsafe {
        let mut log = literal(b"log");
        let f = call(ctx, OPEN, 1, Some(&mut log), 3);
        assert_eq!((f.status, f.error), (0, IoError::OK));
        let write = call(ctx, WRITE, f.value, Some(&mut literal(b"hi")), 0);
        assert_eq!(write.error, IoError::OK);
        assert_eq!(call(ctx, CLOSE, f.value, None, 0).error, IoError::OK);
        let exists = call(ctx, OPEN, 1, Some(&mut log), 3);
        assert_eq!((exists.error.tag, exists.error.code), (3, 0));
        let g = call(ctx, OPEN, 1, Some(&mut log), 0);
        assert_eq!(g.error, IoError::OK);
        if small {
            (&mut *ctx).limit_enter(4).unwrap();
        }
        let before = (*ctx).available();
        let block = crate::abi::hirsel_core_alloc(
            ctx,
            ptr::null_mut(),
            8,
            1,
            8,
            false,
            crate::block::PLAIN,
        );
        if small {
            assert!(block.pointer.is_null());
            assert_eq!((*ctx).available(), before);
            crate::abi::hirsel_core_raise(
                ctx,
                crate::causes::Cause::new(crate::causes::UNWRAP, 123),
            );
            (&mut *ctx).limit_exit();
            return 1;
        }
        assert!(!block.pointer.is_null());
        let mut buf = Buffer {
            data: block.pointer,
            len: 0,
            capacity: 8,
        };
        let read = call(ctx, READ, g.value, Some(&mut buf), 0);
        assert_eq!((read.error, read.value, buf.len), (IoError::OK, 2, 2));
        let byte = *buf.data.add(1);
        assert_eq!(call(ctx, CLOSE, g.value, None, 0).error, IoError::OK);
        crate::abi::hirsel_core_free(ptr::NonNull::new(buf.data).unwrap(), 8);
        assert_eq!((*ctx).available(), before);
        (*state).observed.extend_from_slice(&[
            exists.error.tag as u64,
            read.value,
            byte as u64,
            write.written,
        ]);
        0
    }
}
unsafe fn temp(ctx: *mut Context, state: *mut State, capacity: usize) -> u8 {
    // SAFETY: native equivalent of providers.core f2, freeing the buffer on both edges.
    unsafe {
        let charge = (capacity as u64 + 7) & !7;
        let before = (*ctx).available();
        let block = crate::abi::hirsel_core_alloc(
            ctx,
            ptr::null_mut(),
            capacity,
            1,
            charge,
            false,
            crate::block::PLAIN,
        );
        assert!(!block.pointer.is_null());
        let mut name = Buffer {
            data: block.pointer,
            len: 0,
            capacity,
        };
        let dir = call(ctx, MKDTEMP, 1, Some(&mut name), 0);
        if capacity < 32 {
            assert_eq!(dir.status, 1);
            crate::abi::hirsel_core_free(ptr::NonNull::new(name.data).unwrap(), charge);
            assert_eq!((*ctx).available(), before);
            return 1;
        }
        assert_eq!((dir.status, dir.error), (0, IoError::OK));
        let mut x = literal(b"x");
        assert_eq!(
            call(ctx, MKDIR, dir.value, Some(&mut x), 0).error,
            IoError::OK
        );
        let exists = call(ctx, MKDIR, dir.value, Some(&mut x), 0);
        assert_eq!((exists.error.tag, exists.error.code), (3, 0));
        (*state)
            .observed
            .extend_from_slice(&[name.len as u64, exists.error.tag as u64]);
        assert_eq!(
            call(ctx, REMOVE, dir.value, Some(&mut x), 0).error,
            IoError::OK
        );
        assert_eq!(
            call(ctx, REMOVE_TREE, 1, Some(&mut name), 0).error,
            IoError::OK
        );
        crate::abi::hirsel_core_free(ptr::NonNull::new(name.data).unwrap(), charge);
        assert_eq!((*ctx).available(), before);
        0
    }
}
unsafe extern "C" fn entry(ctx: *mut Context, cfg: *const Config, _: *mut MainResult) -> u8 {
    // SAFETY: state is pinned for this native run; raw accesses end before callbacks.
    unsafe {
        let state = (*cfg).state.cast::<State>();
        match (*state).scenario {
            Scenario::Echo(index) => {
                let (n, w) = echo(ctx, index);
                (*state).observed.extend_from_slice(&[n, w]);
                0
            }
            Scenario::SmallAllocation => files(ctx, state, true),
            Scenario::Temp(capacity) => temp(ctx, state, capacity),
            Scenario::Bounds(capacity) => {
                if capacity == 31 {
                    return temp(ctx, state, 31);
                }
                let mut static_name = literal(b"static");
                if capacity == usize::MAX {
                    static_name.len = usize::MAX;
                }
                let out = call(ctx, MKDTEMP, 1, Some(&mut static_name), 0);
                assert_eq!(out.status, 1);
                assert_eq!(static_name.capacity, 0);
                1
            }
            Scenario::Cancel => {
                (*ctx)
                    .cancel_requested
                    .store(true, std::sync::atomic::Ordering::Release);
                assert_eq!(call(ctx, STDOUT, 3, Some(&mut literal(b"no")), 0).status, 1);
                assert_eq!(call(ctx, WALL_MS, 2, None, 0).status, 0);
                assert_eq!(call(ctx, MONO_NS, 2, None, 0).status, 0);
                1
            }
            Scenario::SleepCancel => {
                (*ctx)
                    .cancel_requested
                    .store(true, std::sync::atomic::Ordering::Release);
                let out = hirsel_core_host_sleep_until(ctx, 2, 3, 123);
                assert_eq!(out.status, 1);
                1
            }
            Scenario::SleepMeasure(milliseconds) => {
                let before_mono = call(ctx, MONO_NS, 2, None, 120).value;
                let before_wall = call(ctx, WALL_MS, 2, None, 121).value;
                let deadline = before_mono.saturating_add(milliseconds * 1_000_000);
                let out = hirsel_core_host_sleep_until(ctx, 2, deadline, 122);
                assert_eq!(out.status, 0);
                let after_mono = call(ctx, MONO_NS, 2, None, 123).value;
                let after_wall = call(ctx, WALL_MS, 2, None, 124).value;
                (*state).observed.extend_from_slice(&[
                    before_mono,
                    after_mono,
                    before_wall,
                    after_wall,
                ]);
                0
            }
            Scenario::SleepAlreadyReached => {
                let before = call(ctx, MONO_NS, 2, None, 126).value;
                let out = hirsel_core_host_sleep_until(ctx, 2, before, 127);
                assert_eq!(out.status, 0);
                (*state)
                    .observed
                    .push(call(ctx, MONO_NS, 2, None, 128).value);
                0
            }
            Scenario::Unavailable => files(ctx, state, false),
            Scenario::Exercise => {
                assert_eq!(echo(ctx, 1), (2, 5));
                assert!(hirsel_core_args_get(ctx, 5, 2).is_null());
                assert!(hirsel_core_args_get(ctx, 5, u64::MAX).is_null());
                assert_eq!(files(ctx, state, false), 0);
                let file = call(ctx, OPEN, 1, Some(&mut literal(b"log")), 0).value;
                assert_eq!(call(ctx, TRY_LOCK, file, None, 0).value, 1);
                assert_eq!(call(ctx, UNLOCK, file, None, 0).error, IoError::OK);
                let mut borrowed = literal(b"unchanged");
                let pointer = borrowed.data;
                assert_eq!(call(ctx, READ, file, Some(&mut borrowed), 0).value, 0);
                assert_eq!(
                    (borrowed.data, borrowed.len, borrowed.capacity),
                    (pointer, 9, 0)
                );
                (*state).close_next_read = true;
                let mut bytes = [0u8; 8];
                let mut buf = Buffer {
                    data: bytes.as_mut_ptr(),
                    len: 0,
                    capacity: 8,
                };
                let closed = call(ctx, READ, file, Some(&mut buf), 0);
                assert_eq!((closed.error.tag, closed.error.code, buf.len), (4, 0, 0));
                let _ = call(ctx, CLOSE, file, None, 0);
                assert_eq!(
                    call(ctx, OPEN, 1, Some(&mut literal(b"missing")), 0)
                        .error
                        .tag,
                    1
                );
                assert_eq!(
                    call(ctx, OPEN, 1, Some(&mut literal(b"denied")), 0)
                        .error
                        .tag,
                    2
                );
                assert_eq!(temp(ctx, state, 40), 0);
                let wall = call(ctx, WALL_MS, 2, None, 0).value;
                let mono = call(ctx, MONO_NS, 2, None, 0).value;
                if (*state).os.is_some() {
                    assert!(wall > 1_700_000_000_000);
                    assert!(call(ctx, MONO_NS, 2, None, 0).value >= mono);
                } else {
                    assert_eq!((wall, mono), (37, 41));
                }
                let err = call(ctx, STDERR, 4, Some(&mut literal(b"error")), 0);
                assert_eq!((err.error, err.written), (IoError::OK, 5));
                0
            }
        }
    }
}
fn fixture(scripted: bool, scenario: Scenario, args: &[&[u8]]) -> (u8, Box<State>) {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut os = Box::new(crate::os::OsProvider::new(
        File::create(root.join("stdout")).unwrap().into(),
        File::create(root.join("stderr")).unwrap().into(),
    ));
    let directory = os.grant(root).unwrap();
    // A real EACCES, independent of path confinement.
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(root.join("denied"), b"secret").unwrap();
    std::fs::set_permissions(root.join("denied"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut state = Box::new(State {
        os: (!scripted).then_some(os),
        directory: if scripted { 7 } else { directory },
        scenario,
        issued: vec![],
        delivered: vec![],
        stdout: vec![],
        stderr: vec![],
        diagnostics: vec![],
        observed: vec![],
        created: false,
        made: false,
        close_next_read: false,
    });
    let provider = Provider {
        state: (&mut *state as *mut State).cast(),
        wait: wait,
        issue: step_issue,
        resume: crate::provider::immediate::resume,
        abort: crate::provider::immediate::abort,
        deliver,
    };
    let grants = [
        Grant {
            kind: crate::entry::DIR,
            provider: &provider,
            handle: state.directory,
            terminal: false,
        },
        Grant {
            kind: crate::entry::CLOCK,
            provider: &provider,
            handle: 0,
            terminal: false,
        },
        Grant {
            kind: crate::entry::STDOUT,
            provider: &provider,
            handle: 0,
            terminal: false,
        },
        Grant {
            kind: crate::entry::STDERR,
            provider: &provider,
            handle: 0,
            terminal: false,
        },
        Grant {
            kind: crate::entry::ARGS,
            provider: &provider,
            handle: 0,
            terminal: false,
        },
    ];
    let carriers = args.iter().map(|a| literal(a)).collect::<Vec<_>>();
    let config = Config {
        launch: std::ptr::null(),
        describe_output: None,
        root_limit: 4096,
        args: carriers.as_ptr(),
        args_len: carriers.len(),
        grants: ptr::null(),
        grants_len: 0,
        provider: &provider,
        state: provider.state,
        diagnostic,
        describe: None,
        scheduler: Default::default(),
        stop_fd: 0,
        capabilities: grants.as_ptr(),
        capabilities_len: grants.len(),
        close_grants: None,
    };
    // SAFETY: all run-owned operands and callback state outlive the synchronous run.
    let status = unsafe { hirsel_core_run(&config, entry) };
    assert_eq!(state.issued, state.delivered, "issue/delivery order");
    if !scripted {
        state.stdout = std::fs::read(root.join("stdout")).unwrap();
        state.stderr = std::fs::read(root.join("stderr")).unwrap();
    }
    (status, state)
}
#[test]
fn every_host_op_routes_to_the_host_and_records_delivery() {
    crate::openat2_required::require();
    assert_child_passes("host_ops_tests::real_host_operations_child");
}
#[test]
fn live_sleep_until_waits_on_host_clock_and_advances_wall() {
    let started = std::time::Instant::now();
    let output = child_case("host_ops_tests::live_sleep_until_50ms_child");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(30),
        "{elapsed:?}"
    );
    assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
}
#[test]
#[ignore = "real monotonic wait fixture, invoked by parent"]
fn live_sleep_until_50ms_child() {
    crate::openat2_required::require();
    let (status, state) = fixture(false, Scenario::SleepMeasure(50), &[]);
    assert_eq!(status, 0);
    assert_eq!(state.observed.len(), 4);
    assert!(state.observed[1] >= state.observed[0] + 40_000_000);
    assert!(state.observed[3] > state.observed[2]);
}
#[test]
fn replay_sleep_until_does_not_wait() {
    let started = std::time::Instant::now();
    let (status, state) = fixture(true, Scenario::SleepMeasure(50), &[]);
    assert_eq!(status, 0);
    assert_eq!(state.observed, [41, 41, 37, 37]);
    assert!(started.elapsed() < std::time::Duration::from_millis(20));
}
#[test]
fn already_reached_sleep_until_returns_at_once() {
    crate::openat2_required::require();
    let started = std::time::Instant::now();
    let (status, state) = fixture(false, Scenario::SleepAlreadyReached, &[]);
    assert_eq!(status, 0);
    assert_eq!(state.observed.len(), 1);
    assert!(started.elapsed() < std::time::Duration::from_millis(20));
}
#[test]
#[ignore = "isolated ffi assertion fixture, invoked by parent"]
fn real_host_operations_child() {
    crate::openat2_required::require();
    let (status, state) = fixture(false, Scenario::Exercise, &[b"prog", b"hello"]);
    assert_eq!(status, 0, "{}", String::from_utf8_lossy(&state.diagnostics));
    for op in 0..14 {
        assert!(state.issued.contains(&op), "operation {op}");
    }
    assert_eq!(
        (state.stdout.as_slice(), state.stderr.as_slice()),
        (b"hello".as_slice(), b"error".as_slice())
    );
    assert_eq!(&state.observed[..4], [3, 2, 105, 2]);
    assert!((1..32).contains(&state.observed[4]));
    assert_eq!(state.observed[5], 3);
}
#[test]
fn providers_sample_hand_derived_outcomes_match_the_native_abi() {
    let (status, state) = fixture(true, Scenario::Exercise, &[b"prog", b"hello"]);
    assert_eq!(status, 0);
    assert_eq!(state.observed, [3, 2, 105, 2, 2, 3]);
    assert_eq!(
        (state.stdout.as_slice(), state.stderr.as_slice()),
        (b"hello".as_slice(), b"error".as_slice())
    );
    for op in 0..14 {
        assert!(state.issued.contains(&op), "operation {op}");
    }
    for (args, index, expected) in [
        (&[b"prog".as_slice(), b"hello".as_slice()][..], 2, [2, 0]),
        (&[][..], 0, [0, 0]),
    ] {
        let (status, state) = fixture(true, Scenario::Echo(index), args);
        assert_eq!(status, 0);
        assert_eq!(state.observed, expected);
        assert!(state.stdout.is_empty());
    }
    let (status, state) = fixture(true, Scenario::SmallAllocation, &[]);
    assert_eq!(status, 101);
    assert_eq!(state.diagnostics, b"Unwrap at 123\n");
    let (status, state) = fixture(true, Scenario::Temp(32), &[]);
    assert_eq!(status, 0);
    assert_eq!(state.observed, [2, 3]);
}
#[test]
fn static_and_short_temp_carriers_fail_bounds_before_issue() {
    for scripted in [false, true] {
        for capacity in [0, 31, usize::MAX] {
            let (status, state) = fixture(scripted, Scenario::Bounds(capacity), &[]);
            assert_eq!(status, 101);
            assert_eq!(state.diagnostics, b"Bounds at 123\n");
            assert!(state.issued.is_empty());
        }
    }
}
#[test]
fn waiting_rows_cancel_before_issue_but_clocks_do_not_cancel() {
    for scripted in [false, true] {
        let (status, state) = fixture(scripted, Scenario::Cancel, &[]);
        assert_eq!(status, 101);
        assert_eq!(state.diagnostics, b"Cancelled at 123\n");
        assert_eq!(state.issued, [WALL_MS, MONO_NS]);
        assert!(state.stdout.is_empty());
    }
}

#[test]
fn sleep_until_cancellation_is_cancelled_at_the_native_wait_boundary() {
    for scripted in [false, true] {
        let (status, state) = fixture(scripted, Scenario::SleepCancel, &[]);
        assert_eq!(status, 101);
        assert_eq!(state.diagnostics, b"Cancelled at 123\n");
        assert!(state.issued.is_empty());
        assert!(state.delivered.is_empty());
    }
}

#[derive(Clone, Copy)]
struct TimerChild {
    provider: *const Provider,
    deadline: u64,
    started: *const AtomicBool,
    finished: *const AtomicBool,
    order: *const AtomicU64,
    id: u64,
}

#[derive(Clone, Copy)]
struct TimerRoot {
    provider: *const Provider,
    first: *const AtomicBool,
    second: *const AtomicBool,
    order: *const AtomicU64,
}

fn timer_layout<T>() -> crate::tasks::TaskLayout {
    crate::tasks::TaskLayout {
        input: crate::tasks::ValueLayout {
            bytes: size_of::<T>(),
            align: align_of::<T>(),
            logical: 0,
            clean: None,
        },
        result: crate::tasks::ValueLayout {
            bytes: 0,
            align: 1,
            logical: 0,
            clean: None,
        },
    }
}

unsafe extern "C" fn timer_child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the scheduler owns this copied input for the child lifetime.
    unsafe {
        let input = input.cast::<TimerChild>().read();
        (*input.started).store(true, Ordering::Release);
        let mut bytes = input.deadline.to_ne_bytes();
        let mut request = Request {
            operation: SLEEP_UNTIL,
            mode: 0,
            handle: 0,
            path: ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 8,
                capacity: 8,
            },
            site: 123,
        };
        let out = hirsel_core_host(context, input.provider, &mut request);
        assert_eq!(out.status, 0);
        input
            .finished
            .as_ref()
            .unwrap()
            .store(true, Ordering::Release);
        input
            .order
            .as_ref()
            .unwrap()
            .compare_exchange(0, input.id, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        0
    }
}

unsafe extern "C" fn cancellable_timer_child(
    context: *mut Context,
    _: *mut u8,
    input: *mut u8,
) -> u8 {
    // SAFETY: the scheduler owns this copied input for the child lifetime.
    unsafe {
        let input = input.cast::<TimerChild>().read();
        (*input.started).store(true, Ordering::Release);
        let mut bytes = input.deadline.to_ne_bytes();
        let mut request = Request {
            operation: SLEEP_UNTIL,
            mode: 0,
            handle: 0,
            path: ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 8,
                capacity: 8,
            },
            site: 123,
        };
        let out = hirsel_core_host(context, input.provider, &mut request);
        assert_eq!(out.status, 1);
        1
    }
}

unsafe extern "C" fn cancel_timer_root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: root owns the copied input and child handle until join.
    unsafe {
        let input = input.cast::<TimerRoot>().read();
        let mut child = TimerChild {
            provider: input.provider,
            deadline: 200_000_000,
            started: input.first,
            finished: input.second,
            order: input.order,
            id: 1,
        };
        let spawned = hirsel_core_spawn(
            context,
            0,
            0,
            (&mut child as *mut TimerChild).cast(),
            cancellable_timer_child,
            timer_layout::<TimerChild>(),
            0,
        );
        assert!(!spawned.task.is_null());
        crate::tasks::checkpoint();
        hirsel_core_cancel(context, spawned.task);
        let joined = hirsel_core_join(context, spawned.task, ptr::null_mut(), 123);
        assert_eq!((joined.kind, joined.status), (2, 0));
        0
    }
}

unsafe extern "C" fn timer_root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: root owns the copied input and each child handle until join.
    unsafe {
        let input = input.cast::<TimerRoot>().read();
        let mut first = TimerChild {
            provider: input.provider,
            deadline: 50_000_000,
            started: input.first,
            finished: input.first,
            order: input.order,
            id: 1,
        };
        let mut second = first;
        second.deadline = 5_000_000;
        second.started = input.second;
        second.finished = input.second;
        second.id = 2;
        let a = hirsel_core_spawn(
            context,
            0,
            0,
            (&mut first as *mut TimerChild).cast(),
            timer_child,
            timer_layout::<TimerChild>(),
            0,
        );
        let b = hirsel_core_spawn(
            context,
            0,
            0,
            (&mut second as *mut TimerChild).cast(),
            timer_child,
            timer_layout::<TimerChild>(),
            0,
        );
        assert!(!a.task.is_null() && !b.task.is_null());
        let joined_a = hirsel_core_join(context, a.task, ptr::null_mut(), 123);
        assert_eq!((joined_a.kind, joined_a.status), (0, 0));
        let joined_b = hirsel_core_join(context, b.task, ptr::null_mut(), 123);
        assert_eq!((joined_b.kind, joined_b.status), (0, 0));
        0
    }
}

#[test]
fn os_sleep_parks_worker_runs_peer_and_orders_deadlines() {
    let stdout: std::os::fd::OwnedFd = File::open("/dev/null").unwrap().into();
    let stderr: std::os::fd::OwnedFd = File::open("/dev/null").unwrap().into();
    let mut os = Box::new(crate::os::OsProvider::new(stdout, stderr));
    let provider = os.provider();
    let first = AtomicBool::new(false);
    let second = AtomicBool::new(false);
    let order = AtomicU64::new(0);
    let input = TimerRoot {
        provider: &provider,
        first: &first,
        second: &second,
        order: &order,
    };
    let mut context = Context::new(1 << 20);
    crate::causes::task_start(&mut context).unwrap();
    let started = Instant::now();
    let status = unsafe {
        crate::tasks::run_root(
            &mut context,
            timer_root,
            (&input as *const TimerRoot).cast_mut().cast(),
            ptr::null_mut(),
            size_of::<TimerRoot>(),
            0,
            crate::tasks::SchedulerProvider::default(),
        )
    };
    let elapsed = started.elapsed();
    assert_eq!(status, 0);
    assert!(first.load(Ordering::Acquire) && second.load(Ordering::Acquire));
    assert_eq!(order.load(Ordering::Acquire), 2);
    assert!(elapsed >= std::time::Duration::from_millis(40));
    assert!(elapsed < std::time::Duration::from_millis(500));
    drop(os);
}

#[test]
fn os_sleep_cancelled_while_blocked_returns_cancelled_promptly() {
    let stdout: std::os::fd::OwnedFd = File::open("/dev/null").unwrap().into();
    let stderr: std::os::fd::OwnedFd = File::open("/dev/null").unwrap().into();
    let mut os = Box::new(crate::os::OsProvider::new(stdout, stderr));
    let provider = os.provider();
    let started = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    let order = AtomicU64::new(0);
    let input = TimerRoot {
        provider: &provider,
        first: &started,
        second: &finished,
        order: &order,
    };
    let mut context = Context::new(1 << 20);
    crate::causes::task_start(&mut context).unwrap();
    let began = Instant::now();
    let status = unsafe {
        crate::tasks::run_root(
            &mut context,
            cancel_timer_root,
            (&input as *const TimerRoot).cast_mut().cast(),
            ptr::null_mut(),
            size_of::<TimerRoot>(),
            0,
            crate::tasks::SchedulerProvider::default(),
        )
    };
    assert_eq!(status, 0);
    assert!(started.load(Ordering::Acquire));
    assert!(began.elapsed() < std::time::Duration::from_millis(100));
    drop(os);
}

unsafe extern "C" fn scripted_sleep(_: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: the fixture pins the request for the callback.
    unsafe {
        assert_eq!((*request).operation, SLEEP_UNTIL);
    }
    Outcome::default()
}
unsafe extern "C" fn scripted_sleep_step(s: *mut u8, r: *mut Request, _: u64) -> Step {
    Step {
        kind: STEP_DONE,
        outcome: scripted_sleep(s, r),
        deadline_ns: 0,
    }
}

unsafe extern "C" fn scripted_deliver(_: *mut u8, _: *mut Request, out: Outcome) -> Outcome {
    out
}

#[test]
fn replayed_sleep_does_not_wait() {
    let provider = Provider {
        state: ptr::null_mut(),
        wait: crate::provider::immediate::wait,
        issue: scripted_sleep_step,
        resume: crate::provider::immediate::resume,
        abort: crate::provider::immediate::abort,
        deliver: scripted_deliver,
    };
    let mut context = Context::new(1 << 20);
    crate::causes::task_start(&mut context).unwrap();
    let mut bytes = 1_000_000_000u64.to_ne_bytes();
    let mut request = Request {
        operation: SLEEP_UNTIL,
        mode: 0,
        handle: 0,
        path: ptr::null(),
        path_len: 0,
        buffer: Buffer {
            data: bytes.as_mut_ptr(),
            len: 8,
            capacity: 8,
        },
        site: 123,
    };
    let started = Instant::now();
    let out = unsafe { hirsel_core_host(&mut context, &provider, &mut request) };
    assert_eq!(out.status, 0);
    assert!(started.elapsed() < std::time::Duration::from_millis(20));
}

fn child_case(name: &str) -> std::process::Output {
    crate::test_process::command()
        .args([
            "--ignored",
            "--exact",
            name,
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap()
}
fn assert_child_passes(name: &str) {
    let output = child_case(name);
    assert!(
        output.status.success(),
        "{name}: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[ignore = "intentional ENOSYS ffi abort, invoked by reporting regression"]
fn unavailable_host_operations_child() {
    fixture(true, Scenario::Unavailable, &[]);
}
#[test]
#[ignore = "intentional parent failure, invoked by reporting regression"]
fn reports_unavailable_host_child_failure() {
    assert_child_passes("host_ops_tests::unavailable_host_operations_child");
}
#[test]
#[ignore = "continuation witness, invoked by reporting regression"]
fn z_later_host_body_child() {
    println!("later host body executed");
}
#[test]
fn ffi_abort_is_a_reported_failure_and_later_bodies_continue() {
    let output = crate::test_process::command()
        .args([
            "--ignored",
            "--exact",
            "host_ops_tests::reports_unavailable_host_child_failure",
            "host_ops_tests::z_later_host_body_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(101),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 passed; 1 failed"), "{stdout}");
    assert!(stdout.contains("later host body executed"), "{stdout}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("code: {}", libc::ENOSYS)));
}

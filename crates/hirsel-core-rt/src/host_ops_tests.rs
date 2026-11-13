use crate::{Context, abi_host::*, entry::Grant, process::*, provider::*};
use std::{
    fs::File,
    path::PathBuf,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy)]
enum Scenario {
    Exercise,
    Echo(u64),
    SmallAllocation,
    Temp(usize),
    Bounds(usize),
    Cancel,
}
struct State {
    os: Option<Box<crate::os::OsProvider>>,
    directory: u64,
    root: PathBuf,
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
            return (provider.issue)(provider.state, r);
        }
        let mut out = Outcome::default();
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
                (*ctx).cancel_requested = true;
                assert_eq!(call(ctx, STDOUT, 3, Some(&mut literal(b"no")), 0).status, 1);
                assert_eq!(call(ctx, WALL_MS, 2, None, 0).status, 0);
                assert_eq!(call(ctx, MONO_NS, 2, None, 0).status, 0);
                1
            }
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
    let root = std::env::temp_dir().join(format!(
        "hirsel-hostops-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let mut os = Box::new(crate::os::OsProvider::new(
        File::create(root.join("stdout")).unwrap().into(),
        File::create(root.join("stderr")).unwrap().into(),
    ));
    let directory = os.grant(&root).unwrap();
    // A real EACCES, independent of path confinement.
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(root.join("denied"), b"secret").unwrap();
    std::fs::set_permissions(root.join("denied"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut state = Box::new(State {
        os: (!scripted).then_some(os),
        directory: if scripted { 7 } else { directory },
        root,
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
        issue,
        deliver,
    };
    let grants = [
        Grant {
            kind: crate::entry::DIR,
            provider: &provider,
            handle: state.directory,
        },
        Grant {
            kind: crate::entry::CLOCK,
            provider: &provider,
            handle: 0,
        },
        Grant {
            kind: crate::entry::STDOUT,
            provider: &provider,
            handle: 0,
        },
        Grant {
            kind: crate::entry::STDERR,
            provider: &provider,
            handle: 0,
        },
        Grant {
            kind: crate::entry::ARGS,
            provider: &provider,
            handle: 0,
        },
    ];
    let carriers = args.iter().map(|a| literal(a)).collect::<Vec<_>>();
    let config = Config {
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
        state.stdout = std::fs::read(state.root.join("stdout")).unwrap();
        state.stderr = std::fs::read(state.root.join("stderr")).unwrap();
    }
    std::fs::remove_dir_all(&state.root).unwrap();
    (status, state)
}
#[test]
fn every_host_op_routes_to_the_host_and_records_delivery() {
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

use crate::{abi::*, causes, os::OsProvider, process::*, provider::*};
use std::{
    fs::File,
    os::fd::OwnedFd,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    os: OsProvider,
    dir: u64,
    context: *mut crate::Context,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hirsel-provider-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let out: OwnedFd = File::create(root.join("stdout")).unwrap().into();
        let err: OwnedFd = File::create(root.join("stderr")).unwrap().into();
        let mut os = OsProvider::new(out, err);
        let dir = os.grant(&root).unwrap();
        let context = hirsel_core_context_new(1 << 20);
        // SAFETY: fixture exclusively owns this fresh context.
        unsafe {
            assert!(!hirsel_core_task_start(context).pointer.is_null());
        }
        Self {
            root,
            os,
            dir,
            context,
        }
    }
    fn call(
        &mut self,
        op: u8,
        handle: u64,
        path: &[u8],
        mode: u8,
        bytes: &mut [u8],
        len: usize,
    ) -> (Outcome, usize) {
        let provider = self.os.provider();
        let mut request = Request {
            operation: op,
            mode,
            handle,
            path: path.as_ptr(),
            path_len: path.len(),
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len,
                capacity: bytes.len(),
            },
            site: 17,
        };
        // SAFETY: all operands are live and exclusively borrowed through delivery.
        let outcome = unsafe { hirsel_core_host(self.context, &provider, &mut request) };
        assert_eq!(outcome.status, 0);
        (outcome, request.buffer.len)
    }
    fn simple(&mut self, op: u8, handle: u64, path: &[u8], mode: u8) -> Outcome {
        self.call(op, handle, path, mode, &mut [], 0).0
    }
    fn open(&mut self, path: &[u8], mode: u8) -> u64 {
        let o = self.simple(OPEN, self.dir, path, mode);
        assert_eq!(o.error, IoError::OK);
        o.value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: no operation or transferred value remains live in this fixture.
        unsafe {
            hirsel_core_context_free(self.context);
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
#[test]
fn files_deliver_committed_prefixes_and_all_open_modes() {
    let mut f = Fixture::new();
    assert_eq!(f.simple(OPEN, f.dir, b"absent", 0).error.tag, 1);
    let h = f.open(b"file", 3);
    assert_eq!(f.simple(OPEN, f.dir, b"file", 3).error.tag, 3);
    let mut bytes = *b"abcdef";
    assert_eq!(f.call(WRITE, h, b"", 0, &mut bytes, 6).0.written, 6);
    // Reading a write-only descriptor fails without touching the buffer/cursor.
    let mut read = [42; 4];
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 1).0.error.tag, 4);
    assert_eq!(read, [42; 4]);
    assert_eq!(f.simple(CLOSE, h, b"", 0).error, IoError::OK);
    let h = f.open(b"file", 0);
    let (out, len) = f.call(READ, h, b"", 0, &mut read, 1);
    assert_eq!((out.value, len, read), (3, 4, [42, b'a', b'b', b'c']));
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 4).0.value, 0);
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 0).0.value, 3);
    assert_eq!(&read[..3], b"def");
    assert_eq!(f.call(READ, h, b"", 0, &mut read, 0).0.value, 0);
    assert_eq!(f.call(WRITE, h, b"", 0, &mut bytes, 6).0.error.tag, 4);
    f.simple(CLOSE, h, b"", 0);
    let h = f.open(b"file", 2);
    f.call(WRITE, h, b"", 0, &mut bytes, 1);
    f.simple(CLOSE, h, b"", 0);
    assert_eq!(std::fs::read(f.root.join("file")).unwrap(), b"abcdefa");
    let h = f.open(b"file", 1);
    f.simple(CLOSE, h, b"", 0);
    assert!(std::fs::read(f.root.join("file")).unwrap().is_empty());
    let h = f.open(b"new-append", 2);
    f.simple(CLOSE, h, b"", 0);
    for op in [STDOUT, STDERR] {
        assert_eq!(f.call(op, 0, b"", 0, &mut bytes, 6).0.error, IoError::OK);
    }
    for name in ["stdout", "stderr"] {
        assert_eq!(std::fs::read(f.root.join(name)).unwrap(), b"abcdef");
    }
}
#[test]
fn locks_are_exclusive_idempotent_and_end_at_close() {
    let mut f = Fixture::new();
    let a = f.open(b"lock", 1);
    let b = f.open(b"lock", 1);
    for _ in 0..2 {
        assert_eq!(f.simple(TRY_LOCK, a, b"", 0).value, 1);
    }
    assert_eq!(f.simple(TRY_LOCK, b, b"", 0).value, 0);
    for _ in 0..2 {
        assert_eq!(f.simple(UNLOCK, a, b"", 0).error, IoError::OK);
    }
    assert_eq!(f.simple(TRY_LOCK, b, b"", 0).value, 1);
    f.simple(CLOSE, b, b"", 0);
    assert_eq!(f.simple(TRY_LOCK, a, b"", 0).value, 1);
    f.simple(CLOSE, a, b"", 0);
}
#[test]
fn paths_cannot_escape_grants_and_tree_drop_never_follows_links() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    for p in [b"../escape".as_slice(), b"/tmp/escape"] {
        assert_eq!(f.simple(OPEN, f.dir, p, 1).error.tag, 2);
    }
    symlink("/tmp", f.root.join("outside")).unwrap();
    assert_eq!(f.simple(OPEN, f.dir, b"outside/escape", 1).error.tag, 2);
    let h = f.open(b"inside", 1);
    f.simple(CLOSE, h, b"", 0);
    symlink("inside", f.root.join("link")).unwrap();
    let h = f.open(b"link", 0);
    f.simple(CLOSE, h, b"", 0);
    assert_eq!(f.simple(MKDIR, f.dir, b"tree", 0).error, IoError::OK);
    assert_eq!(f.simple(MKDIR, f.dir, b"tree", 0).error.tag, 3);
    assert_eq!(f.simple(MKDIR, f.dir, b"tree/sub", 0).error, IoError::OK);
    std::fs::write(f.root.join("tree/sub/data"), b"data").unwrap();
    symlink("../../inside", f.root.join("tree/sub/link")).unwrap();
    assert_eq!(f.simple(REMOVE_TREE, f.dir, b"tree", 0).error, IoError::OK);
    assert!(!f.root.join("tree").exists());
    assert!(f.root.join("inside").exists());
    assert_eq!(f.simple(REMOVE, f.dir, b"link", 0).error, IoError::OK);
    assert_eq!(f.simple(REMOVE, f.dir, b"absent", 0).error.tag, 1);
    assert_eq!(f.simple(OPEN, f.dir, b"inside/child", 0).error.tag, 5);
    f.simple(MKDIR, f.dir, b"empty", 0);
    assert_eq!(f.simple(REMOVE, f.dir, b"empty", 0).error, IoError::OK);
    let mut name = [0; 40];
    name[..7].copy_from_slice(b"prefix:");
    let (out, n) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 7);
    assert_eq!(out.error, IoError::OK);
    assert_eq!(&name[..7], b"prefix:");
    assert!(
        f.root
            .join(std::str::from_utf8(&name[..n]).unwrap())
            .is_dir()
    );
    assert_eq!(
        f.simple(REMOVE_TREE, f.dir, &name[..n], 0).error,
        IoError::OK
    );
}
#[test]
fn clocks_and_argv_are_borrowed_inputs() {
    let mut f = Fixture::new();
    let wall = f.simple(WALL_MS, 1, b"", 0).value;
    assert!(wall > 1_700_000_000_000);
    let mono = f.simple(MONO_NS, 1, b"", 0).value;
    assert!(f.simple(MONO_NS, 1, b"", 0).value >= mono);
    let bytes = b"argument";
    let args = [Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: 0,
    }];
    let mut stderr = Vec::new();
    let config = config(args.as_ptr(), 1, &mut stderr);
    // SAFETY: table and bytes live through every borrowed view.
    unsafe {
        (*f.context).run = &config;
        assert_eq!(hirsel_core_args_len(f.context, 1), 1);
        let v = hirsel_core_args_get(f.context, 1, 0);
        assert!(!v.is_null());
        assert_eq!((*v).capacity, 0);
        assert_eq!(std::slice::from_raw_parts((*v).data, (*v).len), bytes);
        assert!(hirsel_core_args_get(f.context, 1, u64::MAX).is_null());
    }
}
#[test]
fn temp_basename_matches_the_completed_carrier_and_invalid_prefixes_do_not_create() {
    for prefix in [b"t-".as_slice(), b"", b"prefix--"] {
        let mut f = Fixture::new();
        let mut name = vec![0xa5; prefix.len() + 32];
        name[..prefix.len()].copy_from_slice(prefix);
        let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, prefix.len());
        assert_eq!(out.error, IoError::OK);
        assert!(len > prefix.len() && len <= name.len());
        assert_eq!(&name[..prefix.len()], prefix);
        assert!(name[prefix.len()..len].is_ascii());
        let basename = std::str::from_utf8(&name[..len]).unwrap();
        let file = f.simple(OPEN, out.value, b"x", 3);
        assert_eq!(file.error, IoError::OK);
        assert_eq!(
            f.call(WRITE, file.value, b"", 0, &mut [b'1'], 1).0.error,
            IoError::OK
        );
        assert_eq!(f.simple(CLOSE, file.value, b"", 0).error, IoError::OK);
        assert_eq!(
            std::fs::read(f.root.join(basename).join("x")).unwrap(),
            b"1"
        );
        assert_eq!(
            f.simple(REMOVE_TREE, f.dir, &name[..len], 0).error,
            IoError::OK
        );
        assert!(!f.root.join(basename).exists());
    }
    for prefix in [b"a/b".as_slice(), b"/", b".", b"..", b"nul\0byte"] {
        let mut f = Fixture::new();
        let mut name = vec![0xa5; prefix.len() + 32];
        name[..prefix.len()].copy_from_slice(prefix);
        let before = name.clone();
        let before_entries = std::fs::read_dir(&f.root).unwrap().count();
        let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, prefix.len());
        assert_eq!(
            out.error,
            if prefix.contains(&0) {
                IoError::from_errno(libc::EIO)
            } else {
                IoError::from_errno(libc::EACCES)
            }
        );
        assert_eq!((name, len), (before, prefix.len()));
        assert_eq!(std::fs::read_dir(&f.root).unwrap().count(), before_entries);
    }
}
#[test]
fn temp_collisions_retry_inside_issue_and_exhaustion_preserves_the_carrier() {
    let mut f = Fixture::new();
    // Host fixture occupies the provider's deterministic first candidate. The
    // oracle is freshness/preservation, not which suffix the next attempt uses.
    let occupied = format!("retry-h{:x}-0", std::process::id());
    std::fs::create_dir(f.root.join(&occupied)).unwrap();
    let mut name = [0xa5; 40];
    name[..6].copy_from_slice(b"retry-");
    let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 6);
    assert_eq!(out.error, IoError::OK);
    assert_ne!(&name[..len], occupied.as_bytes());
    assert!(f.root.join(&occupied).is_dir());
    assert_eq!(
        f.simple(REMOVE_TREE, f.dir, &name[..len], 0).error,
        IoError::OK
    );

    let mut f = Fixture::new();
    for sequence in 0..crate::os::TEMP_ATTEMPTS {
        std::fs::create_dir(
            f.root
                .join(format!("retry-h{:x}-{sequence:x}", std::process::id())),
        )
        .unwrap();
    }
    let mut name = [0xa5; 40];
    name[..6].copy_from_slice(b"retry-");
    let before = name;
    let before_entries = std::fs::read_dir(&f.root).unwrap().count();
    let (out, len) = f.call(MKDTEMP, f.dir, b"", 0, &mut name, 6);
    assert_eq!(out.error, IoError::from_errno(libc::EAGAIN));
    assert_eq!((name, len), (before, 6));
    assert_eq!(std::fs::read_dir(&f.root).unwrap().count(), before_entries);
}
unsafe extern "C" fn diagnostic(state: *mut u8, p: *const u8, n: usize) {
    // SAFETY: test config owns a Vec sink and callback borrows exactly n bytes.
    unsafe {
        (&mut *state.cast::<Vec<u8>>()).extend_from_slice(std::slice::from_raw_parts(p, n));
    }
}
fn config(args: *const Buffer, args_len: usize, stderr: &mut Vec<u8>) -> Config {
    Config {
        root_limit: 1024,
        args,
        args_len,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: std::ptr::null(),
        state: (stderr as *mut Vec<u8>).cast(),
        diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: std::ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    }
}
unsafe extern "C" fn main_result(
    _: *mut crate::Context,
    config: *const Config,
    out: *mut MainResult,
) -> u8 {
    // SAFETY: test config encodes a case in grants_len; out is the live destination.
    unsafe {
        let n = (*config).grants_len;
        *out = MainResult {
            kind: if n == 256 {
                1
            } else if n == 257 {
                0
            } else {
                2
            },
            exit: n as u8,
        };
    }
    0
}
#[test]
fn process_reports_only_main_result_after_cleanup() {
    for n in 0..=257 {
        let mut stderr = Vec::new();
        let mut config = config(std::ptr::null(), 0, &mut stderr);
        config.grants_len = n;
        // SAFETY: generated-thunk contract is implemented by the fixture.
        let status = unsafe { hirsel_core_run(&config, main_result) };
        assert_eq!(
            status,
            if n == 257 {
                0
            } else if (1..=100).contains(&n) {
                n as u8
            } else {
                1
            }
        );
        assert!(stderr.is_empty());
    }
}
unsafe extern "C" fn failing_main(
    context: *mut crate::Context,
    config: *const Config,
    _: *mut MainResult,
) -> u8 {
    // SAFETY: prepaid task primary and resource cell, each recorded exactly once.
    unsafe {
        let resource = hirsel_core_resource_alloc(context, 8, 8, 0, 0).pointer;
        hirsel_core_raise(
            context,
            causes::Cause {
                kind: causes::FAIL,
                site: 41,
                message: Default::default(),
            },
        );
        hirsel_core_finalizer_begin(context, std::ptr::NonNull::new(resource).unwrap());
        hirsel_core_raise(
            context,
            causes::Cause {
                kind: causes::OVERFLOW,
                site: 42,
                message: Default::default(),
            },
        );
        diagnostic((*config).state, b"cleaned\n".as_ptr(), 8);
        hirsel_core_finalizer_end(context, std::ptr::NonNull::new(resource).unwrap());
        hirsel_core_resource_free(std::ptr::NonNull::new(resource).unwrap(), 0);
    }
    1
}
#[test]
fn failure_chain_keeps_order_sites_and_cleanup_precedes_diagnostics() {
    let mut stderr = Vec::new();
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: live test config and thunk with explicit cleanup.
    assert_eq!(unsafe { hirsel_core_run(&config, failing_main) }, 101);
    assert_eq!(stderr, b"cleaned\nFail(\"\") at 41\nOverflow at 42\n");
}
#[test]
fn mkdtemp_bounds_and_pending_cancellation_happen_before_issue() {
    unsafe extern "C" fn forbidden(_: *mut u8, _: *mut Request) -> Outcome {
        panic!("operation must not issue")
    }
    let provider = Provider {
        state: std::ptr::null_mut(),
        issue: forbidden,
        deliver,
    };
    for cancel in [false, true] {
        let f = Fixture::new();
        let mut bytes = [0xa5; 39];
        bytes[..8].copy_from_slice(b"prefix--");
        let before = bytes;
        let mut request = Request {
            operation: MKDTEMP,
            mode: 0,
            handle: f.dir,
            path: b"".as_ptr(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 8,
                capacity: 39,
            },
            site: 88,
        };
        // SAFETY: live operands; fixture owns the context's pending request flag.
        unsafe {
            (*f.context).cancel_requested = cancel;
            assert_eq!(
                hirsel_core_host(f.context, &provider, &mut request).status,
                1
            );
            let chain = hirsel_core_causes_take(f.context);
            assert_eq!(
                (*hirsel_core_cause_value(chain)).kind,
                if cancel {
                    causes::CANCELLED
                } else {
                    causes::BOUNDS
                }
            );
            hirsel_core_causes_drop(chain);
            assert_eq!(request.buffer.len, 8);
            assert_eq!(bytes, before);
        }
    }
}
#[test]
fn issued_operations_deliver_even_when_cancellation_arrives_at_delivery() {
    struct Script {
        context: *mut crate::Context,
        points: Vec<u8>,
        effect: Vec<u8>,
    }
    unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
        // SAFETY: script exclusively owns state, request and the task during issue.
        unsafe {
            let state = &mut *state.cast::<Script>();
            state.points.push(0);
            state
                .effect
                .extend_from_slice(std::slice::from_raw_parts((*request).buffer.data, 2));
            (*state.context).cancel_requested = true;
        }
        Outcome {
            written: 2,
            error: IoError {
                tag: 5,
                code: libc::ENOSPC as u32,
            },
            ..Outcome::default()
        }
    }
    unsafe extern "C" fn delivery(state: *mut u8, _: *mut Request, result: Outcome) -> Outcome {
        // SAFETY: delivery borrows the same script state after issue.
        unsafe {
            (&mut *state.cast::<Script>()).points.push(1);
        }
        result
    }
    let f = Fixture::new();
    let mut state = Script {
        context: f.context,
        points: vec![],
        effect: vec![],
    };
    let provider = Provider {
        state: (&mut state as *mut Script).cast(),
        issue,
        deliver: delivery,
    };
    let mut bytes = *b"abcdef";
    let mut request = Request {
        operation: WRITE,
        mode: 0,
        handle: 0,
        path: b"".as_ptr(),
        path_len: 0,
        buffer: Buffer {
            data: bytes.as_mut_ptr(),
            len: 6,
            capacity: 6,
        },
        site: 19,
    };
    // SAFETY: script and all borrowed operands live through both provider points.
    unsafe {
        let out = hirsel_core_host(f.context, &provider, &mut request);
        assert_eq!((out.status, out.written, out.error.tag), (0, 2, 5));
        assert_eq!(state.points, [0, 1]);
        assert_eq!(state.effect, b"ab");
        assert_eq!(crate::tasks::hirsel_core_check_cancel(f.context, 20), 1);
    }
}
#[test]
fn scripted_host_errors_preserve_the_named_partial_effect_contracts() {
    struct Script {
        calls: usize,
        operation: u8,
        lock: bool,
        closed: bool,
    }
    unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
        // SAFETY: test adapter gets exclusive script/request access at issue.
        unsafe {
            let state = &mut *state.cast::<Script>();
            assert_eq!((*request).operation, state.operation);
            state.calls += 1;
            if state.operation == CLOSE {
                state.closed = true;
                state.lock = false;
            }
            Outcome {
                error: IoError::from_errno(libc::EIO),
                ..Outcome::default()
            }
        }
    }
    for operation in [
        OPEN,
        READ,
        WRITE,
        CLOSE,
        TRY_LOCK,
        UNLOCK,
        MKDIR,
        MKDTEMP,
        REMOVE,
        REMOVE_TREE,
        STDOUT,
        STDERR,
    ] {
        let f = Fixture::new();
        let mut state = Script {
            calls: 0,
            operation,
            lock: operation == UNLOCK || operation == CLOSE,
            closed: false,
        };
        let mut bytes = [42; 40];
        let before = bytes;
        let mut request = Request {
            operation,
            mode: 0,
            handle: 0,
            path: b"x".as_ptr(),
            path_len: 1,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 3,
                capacity: 40,
            },
            site: 1,
        };
        let provider = Provider {
            state: (&mut state as *mut Script).cast(),
            issue,
            deliver,
        };
        // SAFETY: live script and caller capacity for both provider phases.
        let out = unsafe { hirsel_core_host(f.context, &provider, &mut request) };
        assert_eq!(
            (out.status, out.error, state.calls),
            (0, IoError::from_errno(libc::EIO), 1)
        );
        assert_eq!((request.buffer.len, bytes), (3, before));
        assert_eq!(state.closed, operation == CLOSE);
        assert_eq!(state.lock, operation == UNLOCK);
    }
}
#[test]
fn guarded_root_routes_real_os_calls_through_the_separate_host_stack() {
    unsafe extern "C" fn main(
        context: *mut crate::Context,
        config: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        // SAFETY: live run grants/provider, static path, no returned owners.
        unsafe {
            *result = MainResult::default();
            let config = &*config;
            let mut request = Request {
                operation: MKDIR,
                mode: 0,
                handle: *config.grants,
                path: b"from-main".as_ptr(),
                path_len: 9,
                buffer: Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                },
                site: 1,
            };
            assert_eq!(
                hirsel_core_host(context, config.provider, &mut request).error,
                IoError::OK
            );
            request.operation = REMOVE;
            assert_eq!(
                hirsel_core_host(context, config.provider, &mut request).error,
                IoError::OK
            );
        }
        0
    }
    use std::ptr;
    let mut fixture = Fixture::new();
    let provider = fixture.os.provider();
    let grants = [fixture.dir];
    let mut stderr = vec![];
    let mut config = config(ptr::null(), 0, &mut stderr);
    config.provider = &provider;
    config.grants = grants.as_ptr();
    config.grants_len = 1;
    // SAFETY: config, OS adapter and grants outlive the guarded root.
    assert_eq!(unsafe { hirsel_core_run(&config, main) }, 0);
    assert!(stderr.is_empty());
    assert!(!fixture.root.join("from-main").exists());
}
#[test]
fn literal_failure_messages_keep_static_backing_and_render_one_line_with_their_site() {
    unsafe extern "C" fn main(
        context: *mut crate::Context,
        _: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        let message = b"static\nmessage";
        // SAFETY: static String carrier has capacity zero and stays live forever.
        unsafe {
            hirsel_core_raise_string(
                context,
                17,
                message.as_ptr().cast_mut(),
                message.len() as u64,
                0,
            );
        }
        1
    }
    let mut stderr = vec![];
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: generated failure consumes only its prepaid primary and static carrier.
    assert_eq!(unsafe { hirsel_core_run(&config, main) }, 101);
    assert_eq!(stderr, b"Fail(\"static\\nmessage\") at 17\n");
}

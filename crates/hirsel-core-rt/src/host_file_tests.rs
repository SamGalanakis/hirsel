use crate::{Context, abi_host::*, entry::*, process::*, provider::*};
use std::{fs::File, os::fd::AsRawFd, ptr};

struct Track {
    provider: Provider,
    opens: usize,
    closes: usize,
    fd: i32,
    mode: String,
    sentinel: Option<File>,
}
unsafe extern "C" fn legacy_issue(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: fixture owns the tracker and OS provider throughout the run.
    unsafe {
        let t = &mut *state.cast::<Track>();
        let out = (t.provider.issue)(t.provider.state, request, 0).outcome;
        if (*request).operation == OPEN && out.error == IoError::OK {
            t.opens += 1;
            t.fd = (*t.provider.state.cast::<crate::os::OsProvider>()).file_descriptor(out.value);
        }
        if (*request).operation == CLOSE {
            t.closes += 1;
            assert_eq!(t.closes, 1, "second provider close");
            assert_eq!(libc::fcntl(t.fd, libc::F_GETFD), -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            // In this isolated process the next open reuses the closed descriptor.
            // Grant teardown must leave that new owner intact.
            let sentinel = File::open("/dev/null").unwrap();
            assert_eq!(sentinel.as_raw_fd(), t.fd);
            t.sentinel = Some(sentinel);
            if t.mode == "close_error" {
                return Outcome {
                    error: IoError::from_errno(libc::EIO),
                    status: 1,
                    ..out
                };
            }
        }
        out
    }
}
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, _: u64) -> Step {
    Step {
        kind: STEP_DONE,
        outcome: legacy_issue(state, request),
        deadline_ns: 0,
    }
}
unsafe extern "C" fn deliver(state: *mut u8, request: *mut Request, out: Outcome) -> Outcome {
    // SAFETY: immutable fixture metadata; delivery failure follows committed OPEN.
    unsafe {
        if (*state.cast::<Track>()).mode == "delivery_failure" && (*request).operation == OPEN {
            Outcome { status: 1, ..out }
        } else {
            out
        }
    }
}
fn request(operation: u8) -> Request {
    Request {
        operation,
        mode: 1,
        handle: 0,
        path: b"file".as_ptr(),
        path_len: 4,
        buffer: Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
        site: 31,
    }
}
unsafe extern "C" fn entry(ctx: *mut Context, cfg: *const Config, _: *mut MainResult) -> u8 {
    // SAFETY: native fixture uses the same C exports and carrier as either emitter.
    unsafe {
        let track = (*cfg).scheduler.state.cast::<Track>();
        let mode = (*track).mode.clone();
        // Different from cfg.provider: the handle must retain this exact issuing
        // provider even after this local vtable disappears at entry return.
        let provider = Provider {
            state: track.cast(),
            wait: crate::provider::immediate::wait,
            issue,
            resume: crate::provider::immediate::resume,
            abort: crate::provider::immediate::abort,
            deliver,
        };
        if mode == "refusal" {
            (*ctx).current.inject(1, true);
            let refused = crate::abi::hirsel_core_alloc(
                ctx,
                ptr::null_mut(),
                8,
                8,
                8,
                false,
                crate::block::PLAIN,
            );
            assert!(refused.pointer.is_null());
            assert_eq!((*ctx).current.attempts(), 1);
        }
        let mut open = request(OPEN);
        open.handle = hirsel_core_capability_handle(ctx, *(*cfg).grants);
        let opened = hirsel_core_host_file_open(ctx, &provider, &mut open);
        assert_eq!(opened.error, IoError::OK);
        assert_eq!((*track).opens, 1);
        if mode == "delivery_failure" {
            assert_eq!(opened.status, 1);
            assert_eq!((*track).closes, 1);
            return 0;
        }
        assert_eq!(opened.status, 0);
        let file = opened.value;
        let mut lock = request(TRY_LOCK);
        let locked = hirsel_core_host_file(ctx, file, &mut lock);
        assert_eq!((locked.error, locked.value), (IoError::OK, 1));
        if mode == "stop" {
            assert_eq!((*track).closes, 0);
            libc::write(2, b"open; cleanup skipped\n".as_ptr().cast(), 22);
            crate::process::stop(b"HostFileFixture");
        }
        if mode == "failure" {
            crate::causes::record(
                &mut *ctx,
                crate::causes::Cause::new(crate::causes::UNWRAP, 31),
            );
        }
        if mode == "explicit" {
            assert_eq!(
                hirsel_core_host_file(ctx, file, &mut request(CLOSE)).error,
                IoError::OK
            );
        } else if mode == "cancelled_close" {
            (*ctx)
                .cancel_requested
                .store(true, std::sync::atomic::Ordering::Release);
            assert_eq!(
                hirsel_core_host_file(ctx, file, &mut request(CLOSE)).status,
                1
            );
        } else if mode != "teardown" {
            // Pending cancellation cannot interfere with mechanical cleanup.
            (*ctx)
                .cancel_requested
                .store(true, std::sync::atomic::Ordering::Release);
            hirsel_core_hostfile_drop(ctx, file);
        }
        if mode == "refusal" {
            assert_eq!(
                (*ctx).current.attempts(),
                1,
                "open/drop attempted admission"
            );
        }
        u8::from(mode == "failure" || mode == "cancelled_close")
    }
}

#[test]
#[ignore = "isolated descriptor-ownership fixture, invoked by parent tests"]
fn native_host_file_child() {
    crate::openat2_required::require();
    let root = std::env::var_os("HIRSEL_HOST_FILE_ROOT").unwrap();
    let root = std::path::Path::new(&root);
    use std::os::unix::ffi::OsStrExt;
    let bytes = root.as_os_str().as_bytes();
    let dirs = [View {
        data: bytes.as_ptr(),
        len: bytes.len(),
    }];
    let requirements = [Requirement {
        kind: DIR,
        index: 0,
    }];
    let sink = File::options().write(true).open("/dev/null").unwrap();
    let setup = OsConfig {
        directory_ops: std::ptr::null(),
        process_ops: std::ptr::null(),
        socket_activation: std::ptr::null(),
        secret_grants: core::ptr::null(),
        secret_grants_len: 0,
        env_patterns: crate::process::View {
            data: std::ptr::null(),
            len: 0,
        },
        secret_ops: std::ptr::null(),
        env_ops: std::ptr::null(),
        exec_bindings: std::ptr::null(),
        exec_bindings_len: 0,
        exec_ops: std::ptr::null(),
        revision: std::ptr::null(),
        place: 0,
        cred_grants: std::ptr::null(),
        cred_grants_len: 0,
        target: crate::process::View {
            data: std::ptr::null(),
            len: 0,
        },
        root_limit: 4096,
        args: ptr::null(),
        args_len: 0,
        directories: dirs.as_ptr(),
        directories_len: 1,
        grant_mask: 0,
        stdout_fd: sink.as_raw_fd(),
        stderr_fd: sink.as_raw_fd(),
        parameters: requirements.as_ptr(),
        parameters_len: 1,
        net_rules: std::ptr::null(),
        net_rules_len: 0,
    };
    // SAFETY: process owner pins grants; tracker remains live until process_free.
    unsafe {
        let owner = hirsel_core_os_process_new(&setup);
        assert_eq!(owner.error, IoError::OK);
        let cfg = hirsel_core_os_process_config(owner.process).cast_mut();
        let mut track = Track {
            provider: *(*cfg).provider,
            opens: 0,
            closes: 0,
            fd: -1,
            mode: std::env::var("HIRSEL_HOST_FILE_MODE").unwrap(),
            sentinel: None,
        };
        (*cfg).scheduler.state = (&mut track as *mut Track).cast();
        (*cfg).stop_fd = 2;
        let status = hirsel_core_run(cfg, entry);
        assert_eq!(
            status,
            if matches!(track.mode.as_str(), "failure" | "cancelled_close") {
                101
            } else {
                0
            }
        );
        assert_eq!((track.opens, track.closes), (1, 1));
        assert!(track.sentinel.as_ref().unwrap().metadata().is_ok());
        hirsel_core_os_process_free(owner.process);
        assert!(track.sentinel.as_ref().unwrap().metadata().is_ok());
    }
}
fn child(root: &std::path::Path, mode: &str) -> std::process::Output {
    crate::test_process::command()
        .args([
            "--exact",
            "host_file_tests::native_host_file_child",
            "--ignored",
            "--nocapture",
        ])
        .env("HIRSEL_HOST_FILE_ROOT", root)
        .env("HIRSEL_HOST_FILE_MODE", mode)
        .output()
        .unwrap()
}
#[test]
fn native_host_file_exit_paths_close_once_and_open_admits_nothing() {
    crate::openat2_required::require();
    for mode in [
        "normal",
        "failure",
        "teardown",
        "explicit",
        "cancelled_close",
        "close_error",
        "delivery_failure",
        "refusal",
    ] {
        let root = tempfile::tempdir().unwrap();
        let out = child(root.path(), mode);
        assert!(
            out.status.success(),
            "{mode}: {:?}\n{}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
#[test]
fn run_stop_skips_host_file_cleanup_and_kernel_releases_the_descriptor() {
    crate::openat2_required::require();
    let root = tempfile::tempdir().unwrap();
    let out = child(root.path(), "stop");
    assert_eq!(out.status.code(), Some(102));
    assert_eq!(
        out.stderr,
        b"open; cleanup skipped\nrun stop: HostFileFixture\n"
    );
    let file = File::open(root.path().join("file")).unwrap();
    // SAFETY: newly owned fd; lock success proves the exited child's locked file
    // description was closed by _exit without invoking any cleanup callback.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}

fn slot_file_child(case: &str) -> bool {
    const CHILD: &str = "HIRSEL_SLOT_FILE_LAW_CHILD";
    crate::openat2_required::require();
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", case])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn slot_file_fixture<T: Send>(
    directory: File,
    run: impl Fn(&mut crate::provider_tests::Fixture) -> T + Sync,
) -> T {
    use crate::{
        abi::{hirsel_core_context_new, hirsel_core_task_start},
        os::OsProvider,
        provider_tests::Fixture,
    };

    let result = std::sync::Mutex::new(None);
    // SAFETY: the launcher reads no fields of this fixture's configuration.
    let config = unsafe { std::mem::zeroed() };
    let status = crate::units::launch(1, 1, &config, &|_| {
        let mut os = OsProvider::from_streams(None, None);
        let dir = os.adopt_directory(directory.try_clone().unwrap().into());
        let context = hirsel_core_context_new(1 << 20);
        // SAFETY: fixture exclusively owns this fresh context.
        assert!(!unsafe { hirsel_core_task_start(context) }.pointer.is_null());
        let mut fixture = Fixture {
            root: tempfile::tempdir().unwrap(),
            os,
            dir,
            context,
        };
        let value = run(&mut fixture);
        *result.lock().unwrap() = Some(value);
        0
    });
    assert_eq!(status, 0);
    result.into_inner().unwrap().unwrap()
}

fn check_file_backend() {
    let expected_helper = crate::host::sys::file_helper_expected();
    let threads = crate::host::sys::thread_names();
    assert_eq!(
        threads.iter().any(|comm| comm == "hirsel-host"),
        expected_helper,
        "file operations used the wrong backend"
    );
    // §3.3b: syncs take the one file seam (the ring, or `off_slot` on a
    // helper host); no backend starts a separate sync thread.
    assert!(
        !threads.iter().any(|comm| comm == "hirsel-sync"),
        "a sync ran on a dedicated sync thread"
    );
}

#[test]
fn slot_ring_open_stat_and_close_need_no_helper() {
    if !slot_file_child("host_file_tests::slot_ring_open_stat_and_close_need_no_helper") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("granted")).unwrap();
    std::fs::write(root.path().join("outside"), b"secret").unwrap();
    let granted = root.path().join("granted");
    std::fs::write(granted.join("file"), b"contents").unwrap();
    std::os::unix::fs::symlink("../outside", granted.join("escape")).unwrap();
    let directory = File::open(granted).unwrap();
    let (by_path, by_handle, link, escape, close, closed) = slot_file_fixture(directory, |f| {
        let file = f.open(b"file", 0);
        let by_path = f.simple(STAT, f.dir, b"file", 0);
        let by_handle = f.simple(STAT_FILE, file, b"", 0);
        let link = f.simple(STAT, f.dir, b"escape", 0);
        let escape = f.simple(OPEN, f.dir, b"escape", 0);
        let fd = f.os.file_descriptor(file);
        let close = f.simple(CLOSE, file, b"", 0);
        // SAFETY: F_GETFD inspects the descriptor number without taking ownership.
        let closed = unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF);
        (by_path, by_handle, link, escape, close, closed)
    });
    assert_eq!(
        (by_path.error, by_path.kind, by_path.value),
        (IoError::OK, 0, 8)
    );
    assert_eq!(
        (by_handle.error, by_handle.kind, by_handle.value),
        (IoError::OK, 0, 8)
    );
    assert_eq!((link.error, link.kind), (IoError::OK, 2));
    assert_eq!(escape.error.tag, 2);
    assert_eq!(close.error, IoError::OK);
    assert!(closed);
    check_file_backend();
}

/// §3.3b: RENAME, MKDIR and REMOVE run on the slot ring (a confined parent
/// open, then RENAMEAT/MKDIRAT/UNLINKAT on that fd), with no helper thread,
/// and keep core.md's rules there: a rename moves only a regular file and
/// replaces only one, never following a final link, and REMOVE takes an
/// empty directory as well as a file.
#[test]
fn slot_ring_rename_mkdir_and_remove_need_no_helper() {
    if !slot_file_child("host_file_tests::slot_ring_rename_mkdir_and_remove_need_no_helper") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    std::fs::write(r.join("a"), b"1").unwrap();
    std::fs::write(r.join("b"), b"2").unwrap();
    std::fs::create_dir(r.join("d")).unwrap();
    std::os::unix::fs::symlink("a", r.join("l")).unwrap();
    let directory = File::open(r).unwrap();
    let tags = slot_file_fixture(directory, |f| {
        [
            f.simple(RENAME, f.dir, b"a\0b", 0),
            f.simple(RENAME, f.dir, b"d\0c", 0),
            f.simple(RENAME, f.dir, b"l\0c", 0),
            f.simple(RENAME, f.dir, b"b\0d", 0),
            f.simple(RENAME, f.dir, b"gone\0c", 0),
            f.simple(MKDIR, f.dir, b"m", 0),
            f.simple(MKDIR, f.dir, b"m", 0),
            f.simple(REMOVE, f.dir, b"m", 0),
            f.simple(REMOVE, f.dir, b"l", 0),
        ]
        .map(|outcome| outcome.error)
    });
    let denied = IoError::from_errno(libc::EACCES);
    assert_eq!(
        tags,
        [
            IoError::OK,
            denied,
            denied,
            denied,
            IoError::from_errno(libc::ENOENT),
            IoError::OK,
            IoError::from_errno(libc::EEXIST),
            IoError::OK,
            IoError::OK,
        ]
    );
    assert_eq!(std::fs::read(r.join("b")).unwrap(), b"1");
    assert!(!r.join("a").exists() && !r.join("m").exists() && !r.join("c").exists());
    assert!(r.join("d").is_dir() && std::fs::symlink_metadata(r.join("l")).is_err());
    check_file_backend();
}

#[test]
fn slot_ring_writes_report_progress_and_preserve_positioned_cursor() {
    if !slot_file_child(
        "host_file_tests::slot_ring_writes_report_progress_and_preserve_positioned_cursor",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let directory = File::open(root.path()).unwrap();
    let (at, cursor, refusal) = slot_file_fixture(directory, |f| {
        let file = f.open(b"file", 1);
        let mut positioned_bytes = *b"XYZ";
        let mut cursor_bytes = *b"ab";
        let mut refusal_bytes = *b"fail";
        let at = f
            .call(
                WRITE_AT,
                file,
                &2u64.to_le_bytes(),
                1,
                &mut positioned_bytes,
                3,
            )
            .0;
        let cursor = f.call(WRITE, file, b"", 0, &mut cursor_bytes, 2).0;
        let sync = f.simple(SYNC, file, b"", 0);
        let sync_dir = f.simple(SYNC_DIR, f.dir, b"", 0);
        let _ = f.simple(CLOSE, file, b"", 0);
        let readonly = f.open(b"file", 0);
        let refusal = f.call(WRITE, readonly, b"", 0, &mut refusal_bytes, 4).0;
        let _ = f.simple(CLOSE, readonly, b"", 0);
        assert_eq!((sync.error, sync_dir.error), (IoError::OK, IoError::OK));
        (at, cursor, refusal)
    });
    assert_eq!((at.error, at.written), (IoError::OK, 3));
    assert_eq!((cursor.error, cursor.written), (IoError::OK, 2));
    assert_eq!(
        (refusal.error, refusal.written),
        (IoError::from_errno(libc::EBADF), 0)
    );
    assert_eq!(std::fs::read(root.path().join("file")).unwrap(), b"abXYZ");
    check_file_backend();
}

/// How many of this process's descriptors refer to `path`.
fn descriptors_on(path: &std::path::Path) -> usize {
    std::fs::read_dir(crate::host::sys::FD_DIR)
        .unwrap()
        .filter(|fd| std::fs::read_link(fd.as_ref().unwrap().path()).is_ok_and(|to| to == path))
        .count()
}

/// Every unit of a run issues on the run's one OS provider, whichever slot
/// thread it is on. Units on four slots each hold 64 files open at once and
/// close them. Each OPEN must get its own handle and each CLOSE must close
/// exactly the descriptor that OPEN made, so every descriptor is gone at the end.
#[test]
fn units_on_every_slot_share_one_provider_handle_table() {
    use crate::{
        abi::{hirsel_core_context_free, hirsel_core_context_new, hirsel_core_task_start},
        os::OsProvider,
    };
    const UNITS: usize = 4;
    const HELD: usize = 64;
    const ROUNDS: usize = 64;
    if !slot_file_child("host_file_tests::units_on_every_slot_share_one_provider_handle_table") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file"), b"contents").unwrap();
    let mut os = OsProvider::from_streams(None, None);
    let dir = os.adopt_directory(File::open(root.path()).unwrap().into());
    let provider = os.provider();
    struct Shared(Provider);
    // SAFETY: the provider state outlives the launch, which joins every unit.
    unsafe impl Sync for Shared {}
    let shared = Shared(provider);
    let handles = std::sync::Mutex::new(Vec::new());
    // SAFETY: the launcher reads no fields of this fixture's configuration.
    let config = unsafe { std::mem::zeroed() };
    let status = crate::units::launch(UNITS, UNITS, &config, &|_| {
        let shared = &shared;
        let provider = &shared.0;
        let context = hirsel_core_context_new(1 << 20);
        // SAFETY: this unit exclusively owns its fresh context.
        assert!(!unsafe { hirsel_core_task_start(context) }.pointer.is_null());
        let mut issue = |operation, handle, path: &[u8]| {
            let mut r = request(operation);
            r.handle = handle;
            r.path = path.as_ptr();
            r.path_len = path.len();
            r.mode = 0;
            // SAFETY: the request and its path stay live through delivery.
            unsafe { hirsel_core_host(context, provider, &mut r) }
        };
        let mut mine = Vec::new();
        for _ in 0..ROUNDS {
            let opened: Vec<u64> = (0..HELD)
                .map(|_| {
                    let o = issue(OPEN, dir, b"file");
                    assert_eq!((o.error, o.status), (IoError::OK, 0));
                    o.value
                })
                .collect();
            for &file in &opened {
                let o = issue(CLOSE, file, b"");
                assert_eq!((o.error, o.status), (IoError::OK, 0));
            }
            mine.extend(opened);
        }
        // SAFETY: no operation remains live on this context.
        unsafe { hirsel_core_context_free(context) };
        handles.lock().unwrap().extend(mine);
        0
    });
    assert_eq!(status, 0);
    let mut handles = handles.into_inner().unwrap();
    let issued = handles.len();
    handles.sort_unstable();
    handles.dedup();
    assert_eq!(handles.len(), UNITS * HELD * ROUNDS);
    assert_eq!(issued, handles.len(), "two OPENs got one handle");
    let file = root.path().canonicalize().unwrap().join("file");
    assert_eq!(descriptors_on(&file), 0, "a CLOSE missed its descriptor");
    drop(os);
}

#[test]
fn slot_ring_reads_preserve_prefix_cursor_and_errors() {
    if !slot_file_child("host_file_tests::slot_ring_reads_preserve_prefix_cursor_and_errors") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file"), b"abcdef").unwrap();
    let directory = File::open(root.path()).unwrap();
    slot_file_fixture(directory, |f| {
        let file = f.open(b"file", 0);
        let mut at = *b"prefix????";
        let (positioned, len) = f.call(READ_AT, file, &4u64.to_le_bytes(), 0, &mut at, 6);
        assert_eq!(
            (positioned.error, positioned.value, len),
            (IoError::OK, 2, 8)
        );
        assert_eq!(&at[..len], b"prefixef");

        let mut cursor = [0; 3];
        let (read, len) = f.call(READ, file, b"", 0, &mut cursor, 0);
        assert_eq!((read.error, read.value, len), (IoError::OK, 3, 3));
        assert_eq!(&cursor, b"abc");
        assert_eq!(f.simple(CLOSE, file, b"", 0).error, IoError::OK);

        let write_only = f.open(b"file", 1);
        let (failed, len) = f.call(READ, write_only, b"", 0, &mut cursor, 1);
        assert_eq!((failed.error, len), (IoError::from_errno(libc::EBADF), 1));
        assert_eq!(&cursor, b"abc");
        assert_eq!(f.simple(CLOSE, write_only, b"", 0).error, IoError::OK);
    });
    check_file_backend();
}

/// Runs the ring write loop over scripted completions. Returns the outcome
/// and each piece the loop issued, as (offset, length).
fn scripted_write(
    len: usize,
    direct: bool,
    script: Vec<std::io::Result<usize>>,
) -> (Outcome, Vec<(i64, usize)>) {
    let bytes = vec![7u8; len];
    let mut script = script.into_iter();
    let mut issued = Vec::new();
    let outcome = crate::os::write_all_from(&bytes, Some(4096), true, direct, |piece, at| {
        issued.push((at.unwrap(), piece.len()));
        script.next().expect("loop issued past its script")
    });
    assert!(
        script.next().is_none(),
        "loop stopped before its script ended"
    );
    (outcome, issued)
}

#[test]
fn ring_write_loop_continues_and_retries_the_remainder() {
    let eintr = || Err(std::io::Error::from_raw_os_error(libc::EINTR));
    let eagain = || Err(std::io::Error::from_raw_os_error(libc::EAGAIN));
    // An aligned short Direct write continues at its end; EINTR and EAGAIN
    // reissue the same remainder.
    let (out, issued) = scripted_write(
        12288,
        true,
        vec![Ok(4096), eintr(), eagain(), Ok(4096), Ok(4096)],
    );
    assert_eq!((out.error, out.written), (IoError::OK, 12288));
    assert_eq!(
        issued,
        [
            (4096, 12288),
            (8192, 8192),
            (8192, 8192),
            (8192, 8192),
            (12288, 4096)
        ]
    );
}

#[test]
fn ring_write_loop_refuses_zero_and_unaligned_direct_results() {
    let eio = IoError::from_errno(libc::EIO);
    // A zero result on a nonempty write would loop forever if retried.
    let (out, issued) = scripted_write(4096, false, vec![Ok(0)]);
    assert_eq!((out.error, out.written, issued.len()), (eio, 0, 1));
    let (out, _) = scripted_write(8192, true, vec![Ok(4096), Ok(0)]);
    assert_eq!((out.error, out.written), (eio, 4096));
    // A valid aligned Direct request whose completion is unaligned stops:
    // the next piece would break the alignment contract.
    let (out, issued) = scripted_write(8192, true, vec![Ok(512)]);
    assert_eq!((out.error, out.written, issued.len()), (eio, 512, 1));
}

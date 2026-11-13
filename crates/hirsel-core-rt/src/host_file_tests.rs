use crate::{Context, abi_host::*, entry::*, process::*, provider::*};
use std::{fs::File, os::fd::AsRawFd, process::Command, ptr};

struct Track {
    provider: Provider,
    opens: usize,
    closes: usize,
    fd: i32,
    mode: String,
    sentinel: Option<File>,
}
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: fixture owns the tracker and OS provider throughout the run.
    unsafe {
        let t = &mut *state.cast::<Track>();
        let out = (t.provider.issue)(t.provider.state, request);
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
            issue,
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
            (*ctx).cancel_requested = true;
            assert_eq!(
                hirsel_core_host_file(ctx, file, &mut request(CLOSE)).status,
                1
            );
        } else if mode != "teardown" {
            // Pending cancellation cannot interfere with mechanical cleanup.
            (*ctx).cancel_requested = true;
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
    Command::new(std::env::current_exe().unwrap())
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
        let root =
            std::env::temp_dir().join(format!("hirsel-hostfile-{}-{mode}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let out = child(&root, mode);
        std::fs::remove_dir_all(&root).unwrap();
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
    let root = std::env::temp_dir().join(format!("hirsel-hostfile-{}-stop", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let out = child(&root, "stop");
    assert_eq!(out.status.code(), Some(102));
    assert_eq!(out.stderr, b"open; cleanup skipped\nHostFileFixture\n");
    let file = File::open(root.join("file")).unwrap();
    // SAFETY: newly owned fd; lock success proves the exited child's locked file
    // description was closed by _exit without invoking any cleanup callback.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    std::fs::remove_dir_all(&root).unwrap();
}

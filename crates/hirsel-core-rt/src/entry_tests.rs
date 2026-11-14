use crate::{Context, entry::*, process::*, provider::*};
use std::{fs::File, os::fd::AsRawFd, ptr};
#[test]
fn c_entry_constructor_fills_main_order_with_u64_handles_and_copies_argv() {
    crate::openat2_required::require();
    let out = File::options().write(true).open("/dev/null").unwrap();
    let mut argument = b"copied".to_vec();
    let argv = [View {
        data: argument.as_ptr(),
        len: argument.len(),
    }];
    let dirs = [
        View {
            data: b"/tmp".as_ptr(),
            len: 4,
        },
        View {
            data: b"/dev".as_ptr(),
            len: 4,
        },
    ];
    let parameters = [
        Requirement {
            kind: crate::entry::STDOUT,
            index: 0,
        },
        Requirement {
            kind: DIR,
            index: 1,
        },
        Requirement {
            kind: ARGS,
            index: 0,
        },
        Requirement {
            kind: TASKS,
            index: 0,
        },
    ];
    let config = OsConfig {
        root_limit: 4096,
        args: argv.as_ptr(),
        args_len: 1,
        directories: dirs.as_ptr(),
        directories_len: 2,
        grant_mask: 0x3fe,
        stdout_fd: out.as_raw_fd(),
        stderr_fd: out.as_raw_fd(),
        parameters: parameters.as_ptr(),
        parameters_len: parameters.len(),
    };
    // SAFETY: constructor copies bytes and pins all descriptors before returning.
    unsafe {
        let owner = hirsel_core_os_process_new(&config);
        assert_eq!(owner.error, IoError::OK);
        assert!(!owner.process.is_null());
        argument.fill(b'x');
        let process = hirsel_core_os_process_config(owner.process);
        let first = hirsel_core_entry_grant(process, DIR, 0);
        let second = hirsel_core_entry_grant(process, DIR, 1);
        assert_ne!(first, 0);
        assert_ne!(second, 0);
        assert_ne!(first, second);
        let mut context = Context::new(0);
        context.run = process;
        assert_ne!(
            hirsel_core_capability_handle(&context, first),
            hirsel_core_capability_handle(&context, second)
        );
        for kind in NET..=TASKS {
            let operand = hirsel_core_entry_grant(process, kind, 0);
            assert_ne!(operand, 0);
            assert_eq!(
                hirsel_core_capability_provider(&context, operand),
                (*process).provider
            );
            assert_eq!(hirsel_core_capability_handle(&context, operand), 0);
        }
        let args = hirsel_core_entry_grant(process, ARGS, 0);
        assert_ne!(args, 0);
        assert_eq!(hirsel_core_args_len(&context, args), 1);
        let arg = hirsel_core_args_get(&context, args, 0);
        assert_eq!(
            std::slice::from_raw_parts((*arg).data, (*arg).len),
            b"copied"
        );
        assert_eq!(hirsel_core_entry_grant(process, DIR, 2), 0);
        assert_eq!(hirsel_core_entry_grant(process, ARGS, 1), 0);
        let expected = parameters
            .iter()
            .map(|r| hirsel_core_entry_grant(process, r.kind, r.index))
            .collect::<Vec<_>>();
        assert_eq!(
            std::slice::from_raw_parts((*process).grants, (*process).grants_len),
            expected
        );
        assert!(hirsel_core_entry_grants_present(
            process,
            parameters.as_ptr(),
            parameters.len()
        ));
        hirsel_core_os_process_free(owner.process);
        assert!(out.metadata().is_ok());
    }
    let empty = OsConfig {
        grant_mask: 0,
        directories_len: 0,
        args_len: 0,
        parameters_len: 0,
        stdout_fd: -1,
        ..config
    };
    // SAFETY: no implicit capability is granted; empty argv is still a valid Args handle.
    unsafe {
        let owner = hirsel_core_os_process_new(&empty);
        assert_eq!(owner.error, IoError::OK);
        let process = hirsel_core_os_process_config(owner.process);
        for kind in DIR..=TASKS {
            assert_eq!(hirsel_core_entry_grant(process, kind, 0), 0);
        }
        let mut context = Context::new(0);
        context.run = process;
        assert_eq!(
            hirsel_core_args_len(&context, hirsel_core_entry_grant(process, ARGS, 0)),
            0
        );
        hirsel_core_os_process_free(owner.process);
    }
}
#[test]
fn c_constructor_refuses_missing_grants_bad_descriptors_and_invalid_argv_before_entry() {
    let sink = File::options().write(true).open("/dev/null").unwrap();
    let missing = [Requirement {
        kind: DIR,
        index: 0,
    }];
    let config = OsConfig {
        root_limit: 1,
        args: ptr::null(),
        args_len: 0,
        directories: ptr::null(),
        directories_len: 0,
        grant_mask: 0,
        stdout_fd: sink.as_raw_fd(),
        stderr_fd: sink.as_raw_fd(),
        parameters: missing.as_ptr(),
        parameters_len: 1,
    };
    // SAFETY: missing external configuration is refused before a task exists.
    unsafe {
        let failed = hirsel_core_os_process_new(&config);
        assert!(failed.process.is_null());
        assert_eq!(failed.error.tag, 2);
    }
    let invalid = OsConfig {
        stderr_fd: -1,
        ..config
    };
    // SAFETY: fcntl checks the external host descriptor.
    unsafe {
        let failed = hirsel_core_os_process_new(&invalid);
        assert!(failed.process.is_null());
        assert_eq!(failed.error.tag, 4);
    }
    let argv = [View {
        data: b"\xff".as_ptr(),
        len: 1,
    }];
    let invalid = OsConfig {
        args: argv.as_ptr(),
        args_len: 1,
        parameters_len: 0,
        ..config
    };
    // SAFETY: invalid host bytes cannot become a source String view.
    unsafe {
        let failed = hirsel_core_os_process_new(&invalid);
        assert!(failed.process.is_null());
        assert_eq!(failed.error.code, libc::EILSEQ as u32);
    }
}
#[test]
fn c_u64_capability_operands_feed_host_requests_and_ok_is_initialized() {
    unsafe extern "C" fn entry(
        context: *mut Context,
        config: *const Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: assembly supplies the declared u64 operand from Config.grants[0].
        unsafe {
            let clock = *(*config).grants;
            let provider = hirsel_core_capability_provider(context, clock);
            let mut request = Request {
                operation: MONO_NS,
                mode: 0,
                handle: hirsel_core_capability_handle(context, clock),
                path: ptr::null(),
                path_len: 0,
                buffer: Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                },
                site: 0,
            };
            assert_eq!(
                hirsel_core_host(context, provider, &mut request).error,
                IoError::OK
            );
        }
        0
    }
    let sink = File::options().write(true).open("/dev/null").unwrap();
    let parameters = [Requirement {
        kind: CLOCK,
        index: 0,
    }];
    let config = OsConfig {
        root_limit: 1024,
        args: ptr::null(),
        args_len: 0,
        directories: ptr::null(),
        directories_len: 0,
        grant_mask: 1 << CLOCK,
        stdout_fd: sink.as_raw_fd(),
        stderr_fd: sink.as_raw_fd(),
        parameters: parameters.as_ptr(),
        parameters_len: 1,
    };
    // SAFETY: process owner lives through run, including its non-failing grant close.
    unsafe {
        let owner = hirsel_core_os_process_new(&config);
        assert_eq!(owner.error, IoError::OK);
        assert_eq!(
            hirsel_core_run(hirsel_core_os_process_config(owner.process), entry),
            0
        );
        hirsel_core_os_process_free(owner.process);
    }
}
#[test]
fn run_closes_grants_after_main_cleanup_and_before_reporting_status_without_admission() {
    struct State {
        events: Vec<u8>,
    }
    unsafe extern "C" fn close(state: *mut u8) {
        // SAFETY: host callback consumes its run-owned grants without touching task limits.
        unsafe {
            (&mut *state.cast::<State>()).events.push(2);
        }
    }
    unsafe extern "C" fn entry(
        context: *mut Context,
        config: *const Config,
        out: *mut MainResult,
    ) -> u8 {
        // SAFETY: fixture models the completed main cleanup then injects refusal at
        // every later admission; run-owned grant close cannot attempt one.
        unsafe {
            (&mut *(*config).state.cast::<State>()).events.push(1);
            (*context).current.inject(1, true);
            *out = MainResult { kind: 2, exit: 7 };
        }
        0
    }
    unsafe extern "C" fn diagnostic(_: *mut u8, _: *const u8, _: usize) {
        panic!("no error diagnostics");
    }
    let mut state = State { events: vec![] };
    let config = Config {
        root_limit: 1024,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: ptr::null(),
        state: (&mut state as *mut State).cast(),
        diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: ptr::null(),
        capabilities_len: 0,
        close_grants: Some(close),
    };
    // SAFETY: root, state and close callback remain pinned until status is returned.
    assert_eq!(unsafe { hirsel_core_run(&config, entry) }, 7);
    state.events.push(3);
    assert_eq!(state.events, [1, 2, 3]);
}
#[test]
#[ignore = "run only by broken_pipe_is_a_typed_write_error_from_native_process_entry"]
fn broken_pipe_child() {
    use std::os::fd::{FromRawFd, OwnedFd};
    unsafe extern "C" fn entry(
        context: *mut Context,
        config: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        // SAFETY: stdout operand is explicitly configured, and buffer is a static byte.
        unsafe {
            let cap = *(*config).grants;
            let mut request = Request {
                operation: crate::provider::STDOUT,
                mode: 0,
                handle: hirsel_core_capability_handle(context, cap),
                path: ptr::null(),
                path_len: 0,
                buffer: Buffer {
                    data: b"x".as_ptr().cast_mut(),
                    len: 1,
                    capacity: 1,
                },
                site: 1,
            };
            let out = hirsel_core_host(
                context,
                hirsel_core_capability_provider(context, cap),
                &mut request,
            );
            assert_eq!(
                (out.status, out.written, out.error.code),
                (0, 0, libc::EPIPE as u32)
            );
            *result = MainResult { kind: 2, exit: 7 };
        }
        0
    }
    // SAFETY: this isolated child owns both pipe ends and restores the C process's
    // default policy to prove Run supplies EPIPE rather than signal termination.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let mut fds = [0; 2];
        assert_eq!(libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC), 0);
        let read = OwnedFd::from_raw_fd(fds[0]);
        let write = OwnedFd::from_raw_fd(fds[1]);
        drop(read);
        let sink = File::options().write(true).open("/dev/null").unwrap();
        let parameters = [Requirement {
            kind: crate::entry::STDOUT,
            index: 0,
        }];
        let config = OsConfig {
            root_limit: 1024,
            args: ptr::null(),
            args_len: 0,
            directories: ptr::null(),
            directories_len: 0,
            grant_mask: 1 << crate::entry::STDOUT,
            stdout_fd: write.as_raw_fd(),
            stderr_fd: sink.as_raw_fd(),
            parameters: parameters.as_ptr(),
            parameters_len: 1,
        };
        let owner = hirsel_core_os_process_new(&config);
        assert_eq!(owner.error, IoError::OK);
        assert_eq!(
            hirsel_core_run(hirsel_core_os_process_config(owner.process), entry),
            7
        );
        hirsel_core_os_process_free(owner.process);
    }
}
#[test]
fn broken_pipe_is_a_typed_write_error_from_native_process_entry() {
    let out = crate::test_process::command()
        .args([
            "--exact",
            "entry_tests::broken_pipe_child",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
}

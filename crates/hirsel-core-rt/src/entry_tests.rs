use crate::{Context, entry::*, process::*, provider::*};
use std::{fs::File, os::fd::AsRawFd, ptr};

fn launch_config(launch: &hirsel_core_ids::launch::Metadata) -> Config {
    unsafe extern "C" fn diagnostic(_: *mut u8, _: *const u8, _: usize) {
        panic!("launch identity emitted a diagnostic");
    }
    Config {
        root_limit: 4096,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: ptr::null(),
        state: ptr::null_mut(),
        diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: ptr::null(),
        capabilities_len: 0,
        close_grants: None,
        launch,
        describe_output: None,
    }
}

#[test]
fn identity_stable() {
    use std::io::{BufRead, Read, Write};
    const TEST: &str = "entry_tests::identity_stable";
    const COPY_TO: &str = "HIRSEL_TEST_IDENTITY_COPY_TO";
    let executable = std::env::current_exe().unwrap();
    if let Some(destination) = std::env::var_os(COPY_TO) {
        std::fs::copy(&executable, destination).unwrap();
        return;
    }
    if executable.file_name().unwrap() == "launch-child" {
        let launch = hirsel_core_ids::launch::Metadata::native("test-target", 419, false);
        let config = launch_config(&launch);
        let mut context = Context::new(4096);
        context.run = &config;
        let digest = || {
            // SAFETY: Config, context and launch remain pinned in this child.
            unsafe {
                (0..hirsel_core_launch_word(&context, 1, 0))
                    .map(|i| format!("{:02x}", hirsel_core_launch_word(&context, 2, i)))
                    .collect::<String>()
            }
        };
        println!("IDENTITY {}", digest());
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_exact(&mut [0]).unwrap();
        println!("IDENTITY {}", digest());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let child_path = directory.path().join("launch-child");
    // A parallel fork can inherit the writable copy descriptor until it
    // execs. Copy in a separate process and wait for it to close the file.
    let copy = crate::test_process::command()
        .args(["--exact", TEST])
        .env(COPY_TO, &child_path)
        .output()
        .unwrap();
    assert!(
        copy.status.success(),
        "{}",
        String::from_utf8_lossy(&copy.stdout)
    );
    let mut child = crate::test_process::command_for(child_path.as_os_str())
        .args(["--exact", TEST, "--nocapture"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before identity"
        );
        if let Some((_, digest)) = line.split_once("IDENTITY ") {
            first = digest.trim().into();
            break;
        }
    }
    // FIPS 180-4 over the whole launched file, from ring's independent SHA-256;
    // the image spans many of the launcher's hashing windows.
    let image = std::fs::read(&child_path).unwrap();
    assert!(image.len() > 4 << 20);
    let expected: String = crate::os_tls::sha256(&image)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(first, expected);
    let replacement = directory.path().join("replacement");
    std::fs::write(&replacement, b"a different executable on disk").unwrap();
    std::fs::rename(replacement, &child_path).unwrap();
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    let mut tail = String::new();
    output.read_to_string(&mut tail).unwrap();
    assert!(child.wait().unwrap().success(), "{tail}");
    assert!(
        tail.lines().any(|line| line == format!("IDENTITY {first}")),
        "{tail}"
    );
}

#[test]
fn identity_unavailable() {
    let missing = std::env::temp_dir().join(format!("hirsel-missing-exe-{}", std::process::id()));
    let launch = hirsel_core_ids::launch::Metadata::artifact(&missing, "test", 73, false);
    let config = launch_config(&launch);
    let mut context = Context::new(4096);
    context.run = &config;
    // SAFETY: the immutable launch and its owner remain live through both reads.
    unsafe {
        assert_eq!(hirsel_core_launch_word(&context, 1, 0), 0);
        assert_eq!(hirsel_core_launch_word(&context, 0, 0), 73);
    }
}

#[test]
fn describe_launch_skips_directory_grants_and_owns_only_transport_fds() {
    // The descriptor table is process-wide. Other tests can open and close
    // descriptors between these snapshots.
    if crate::test_process::run_isolated(
        "entry_tests::describe_launch_skips_directory_grants_and_owns_only_transport_fds",
    ) {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("describe.out");
    let out = File::create(&path).unwrap();
    let arguments = [
        b"--hirsel-describe".as_slice(),
        b"--hirsel-n-place=1",
        b"argument",
    ];
    let views = arguments.map(|a| View {
        data: a.as_ptr(),
        len: a.len(),
    });
    let missing = b"/a-directory-that-describe-must-never-open";
    let dirs = [View {
        data: missing.as_ptr(),
        len: missing.len(),
    }];
    let parameters = [DIR, CLOCK, TASKS, ARGS].map(|kind| Requirement { kind, index: 0 });
    let target = b"x86_64-unknown-linux-gnu";
    let input = OsConfig {
        root_limit: 4096,
        args: views.as_ptr(),
        args_len: views.len(),
        directories: dirs.as_ptr(),
        directories_len: dirs.len(),
        grant_mask: 0,
        stdout_fd: out.as_raw_fd(),
        stderr_fd: out.as_raw_fd(),
        parameters: parameters.as_ptr(),
        parameters_len: parameters.len(),
        net_rules: std::ptr::null(),
        net_rules_len: 0,
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
        target: View {
            data: target.as_ptr(),
            len: target.len(),
        },
    };
    let fds = || std::fs::read_dir(crate::host::sys::FD_DIR).unwrap().count();
    let before = fds();
    // SAFETY: constructor copies its views and owns the duplicated descriptors.
    unsafe {
        let owner = hirsel_core_os_process_new(&input);
        assert_eq!(owner.error, IoError::OK);
        assert_eq!(fds(), before + 3); // stderr/stdout transport duplicates only.
        {
            let config = hirsel_core_os_process_config(owner.process);
            let mut context = Context::new(4096);
            context.run = config;
            assert_eq!(hirsel_core_launch_word(&context, 5, 0), 1);
            assert_eq!(hirsel_core_launch_word(&context, 0, 0), 1);
            let a = hirsel_core_entry_grant(config, ARGS, 0);
            assert_eq!(hirsel_core_args_len(&context, a), 1);
            let argument = &*hirsel_core_args_get(&context, a, 0);
            assert_eq!(
                std::slice::from_raw_parts(argument.data, argument.len),
                b"argument"
            );
            let output = Buffer {
                data: b"abc".as_ptr().cast_mut(),
                len: 3,
                capacity: 0,
            };
            assert!(hirsel_core_describe_emit(&context, &output));
        }
        hirsel_core_os_process_free(owner.process);
    }
    assert_eq!(fds(), before);
    assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    drop(out);
}
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
            kind: ENV,
            index: 0,
        },
        Requirement {
            kind: STDIN,
            index: 0,
        },
        Requirement {
            kind: TASKS,
            index: 0,
        },
    ];
    let net_rule = hirsel_core_net::RuleView::address(
        hirsel_core_net::Rule::parse("Connect 0.0.0.0/0 0-65535").unwrap(),
    );
    let config = OsConfig {
        directory_ops: std::ptr::null(),
        process_ops: std::ptr::null(),
        socket_activation: std::ptr::null(),
        secret_grants: core::ptr::null(),
        secret_grants_len: 0,
        env_patterns: crate::process::View {
            data: b"HIRSEL_FIXTURE_*".as_ptr(),
            len: b"HIRSEL_FIXTURE_*".len(),
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
        args: argv.as_ptr(),
        args_len: 1,
        directories: dirs.as_ptr(),
        directories_len: 2,
        grant_mask: 0x3fe,
        stdout_fd: out.as_raw_fd(),
        stderr_fd: out.as_raw_fd(),
        parameters: parameters.as_ptr(),
        parameters_len: parameters.len(),
        net_rules: &net_rule,
        net_rules_len: 1,
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
            let handle = hirsel_core_capability_handle(&context, operand);
            match kind {
                NET => {
                    assert_ne!(handle, 0);
                    let rules = &*(handle as *const crate::net::Rules);
                    let decoded = rules
                        .as_slice()
                        .iter()
                        .map(|rule| rule.address)
                        .collect::<Vec<_>>();
                    assert_eq!(decoded, [net_rule.address]);
                }
                ENV => {
                    assert_ne!(handle, 0);
                    let outside = b"OUTSIDE_FIXTURE";
                    let name = Buffer {
                        data: outside.as_ptr().cast_mut(),
                        len: outside.len(),
                        capacity: 0,
                    };
                    let report = crate::env::hirsel_core_env_get(&mut context, operand, &name, 0);
                    assert_eq!(report.error, IoError { tag: 1, code: 0 });
                    assert_eq!(report.value, 0);
                }
                STDIN => assert_ne!(handle, 0),
                _ => assert_eq!(handle, 0),
            }
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
        net_rules: std::ptr::null(),
        net_rules_len: 0,
    };
    // SAFETY: missing external configuration is refused before a task exists.
    unsafe {
        let failed = hirsel_core_os_process_new(&config);
        assert!(failed.process.is_null());
        assert_eq!(failed.error.tag, 2);
    }
    for stderr_fd in [-1, i32::MAX] {
        let invalid = OsConfig {
            stderr_fd,
            ..config
        };
        // SAFETY: fcntl checks the external host descriptor.
        unsafe {
            let failed = hirsel_core_os_process_new(&invalid);
            assert!(failed.process.is_null(), "stderr fd {stderr_fd}");
            assert_eq!(failed.error.tag, 4, "stderr fd {stderr_fd}");
        }
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
        net_rules: std::ptr::null(),
        net_rules_len: 0,
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
fn product_start_double_isolate_catch_refunds_the_first_cause_cell() {
    unsafe extern "C" fn entry(context: *mut Context, _: *const Config, _: *mut MainResult) -> u8 {
        // SAFETY: the product entry owns its task context and each caught chain.
        unsafe {
            let before = (*context).available();
            assert!(crate::isolate::hirsel_core_isolate_enter(context).entered);
            crate::causes::record(
                &mut *context,
                crate::causes::Cause::new(crate::causes::UNWRAP, 1),
            );
            let first = crate::isolate::hirsel_core_isolate_catch(context);
            drop(crate::causes::Causes::from_raw(first.causes));
            assert!(crate::isolate::hirsel_core_isolate_enter(context).entered);
            crate::causes::record(
                &mut *context,
                crate::causes::Cause::new(crate::causes::UNWRAP, 2),
            );
            let second = crate::isolate::hirsel_core_isolate_catch(context);
            drop(crate::causes::Causes::from_raw(second.causes));
            assert_eq!((*context).available(), before);
        }
        0
    }
    unsafe extern "C" fn diagnostic(_: *mut u8, _: *const u8, _: usize) {
        panic!("unexpected product diagnostic");
    }
    let config = Config {
        launch: std::ptr::null(),
        describe_output: None,
        root_limit: 1024,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: ptr::null(),
        state: ptr::null_mut(),
        diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    };
    // SAFETY: config and entry remain pinned for the synchronous product run.
    assert_eq!(unsafe { hirsel_core_run(&config, entry) }, 0);
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
            *out = MainResult {
                kind: 2,
                exit: 7,
                ..Default::default()
            };
        }
        0
    }
    unsafe extern "C" fn diagnostic(_: *mut u8, _: *const u8, _: usize) {
        panic!("no error diagnostics");
    }
    let mut state = State { events: vec![] };
    let config = Config {
        launch: std::ptr::null(),
        describe_output: None,
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
    use std::os::fd::OwnedFd;
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
                (out.status, out.written, out.error),
                (0, 0, crate::provider::IoError { tag: 4, code: 0 })
            );
            *result = MainResult {
                kind: 2,
                exit: 7,
                ..Default::default()
            };
        }
        0
    }
    // SAFETY: this isolated child owns both pipe ends and restores the C process's
    // default policy to prove Run supplies Closed (EPIPE) rather than signal termination.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let (read, write) = std::io::pipe().unwrap();
        let (read, write) = (OwnedFd::from(read), OwnedFd::from(write));
        drop(read);
        let sink = File::options().write(true).open("/dev/null").unwrap();
        let parameters = [Requirement {
            kind: crate::entry::STDOUT,
            index: 0,
        }];
        let config = OsConfig {
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
            net_rules: std::ptr::null(),
            net_rules_len: 0,
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

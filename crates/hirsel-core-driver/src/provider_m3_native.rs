mod provider_m3 {
    use super::*;
    use std::{
        os::unix::process::CommandExt,
        process::{Output, Stdio},
        time::{Duration, Instant},
    };

    fn bounded_output(command: &mut Command) -> Output {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if Instant::now() >= deadline {
                // Only the child started above is terminated, never a discovered PID.
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("provider case exceeded test deadline: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn scripted(backend: Backend) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = tempfile::tempdir().unwrap();
        let mut request = source_request(include_str!("provider_m3.hirsel"), dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let cc = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang");
        let c = dir.path().join("provider.c");
        std::fs::write(&c, include_str!("provider_m3.c")).unwrap();
        let object = dir.path().join("provider.o");
        command(Command::new(&cc).args(["-c", "-I"]).arg(root.join("crates/hirsel-core-rt/include"))
            .arg(&c).arg("-o").arg(&object)).unwrap();
        let mut options = native_options(backend, dir.path());
        let (runtime, linker) = with_link_wrappers(dir.path(), "cc", &options.runtime, &object, &[
            "hirsel_core_os_run_with_sources", "hirsel_core_spawn", "hirsel_core_receive",
            "hirsel_core_send", "hirsel_core_cancel", "hirsel_core_host_io",
            "hirsel_core_host_clock", "hirsel_core_host_op",
        ]);
        options.runtime = runtime;
        options.link = crate::native::Link::Driver(linker);
        options.root_limit = 1 << 20;
        options.grant_mask = (1 << 3) | (1 << 9) | (1 << 10);
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let built = product.build(&entry, &options).unwrap();
        for case in [
            "read",
            "errors",
            "append0",
            "append1",
            "append3",
            "sync",
            "sync-fail",
            "crash0",
            "crash1",
            "crash2",
            "crash3",
            "crash4",
            "crash5",
            "try-receive",
            "random",
            "before-read",
            "issue-read",
            "delivery-read",
            "issue-write",
            "delivery-write",
            "blocked-receive",
            "blocked-send",
            // REPLAY-SPEC section 3 (a): a sleep does not block the scheduler,
            // directly or through a five-callback wrapper; (d): every tag is
            // delivered exactly once through the wrapper.
            "s0-sleep-direct",
            "s0-sleep-wrapper",
            "s0-deliver",
        ] {
            let output = bounded_output(Command::new(&built.executable).arg(case));
            assert_eq!(
                output.status.code(),
                Some(0),
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty(), "{case}: {:?}", output.stdout);
            assert!(
                output.stderr.is_empty(),
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        // F3: each of the five callbacks is required; a NULL one stops the run
        // at install, before main, with the named FAIL-15 stop.
        for callback in 0..5 {
            let case = format!("s0-null-{callback}");
            let output = bounded_output(Command::new(&built.executable).arg(&case));
            assert_eq!(output.status.code(), Some(102), "{case}: {output:?}");
            assert!(output.stdout.is_empty(), "{case}: {output:?}");
            assert_eq!(output.stderr, b"run stop: Provider\n", "{case}");
        }
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn scripted_faults_and_blocked_cancel_cranelift() {
        scripted(Backend::Cranelift);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn scripted_faults_and_blocked_cancel_llvm() {
        scripted(Backend::Llvm);
    }

    fn closed_stdout(backend: Backend) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = tempfile::tempdir().unwrap();
        let source = r#"
pub fn main(out: Stdout, err: Stderr) -> Result[(), WriteError] waits {
    if stdout.is_terminal(out) { fail("closed stdout reports a terminal") }
    stderr.write(err, "MAIN RAN\n")?
    stdout.write(out, "out")
}
"#;
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let mut options = native_options(backend, dir.path());
        options.grant_mask = (1 << 4) | (1 << 5);
        let built = product
            .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
            .unwrap();
        let mut command = Command::new(built.executable);
        // SAFETY: close only this child's stdout after fork, before exec.
        unsafe {
            command.pre_exec(|| {
                unsafe extern "C" {
                    fn close(fd: i32) -> i32;
                }
                close(1);
                Ok(())
            });
        }
        let output = bounded_output(&mut command);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"MAIN RAN\nerror: WriteError(written: 0, error: Closed)\n"
        );
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn closed_stdout_reports_write_error_cranelift() {
        closed_stdout(Backend::Cranelift);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn closed_stdout_reports_write_error_llvm() {
        closed_stdout(Backend::Llvm);
    }
}

mod provider_m3 {
    use super::*;
    use std::{
        io::Write,
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
        let object = dir.path().join("provider.o");
        let c = dir.path().join("provider.c");
        std::fs::write(&c, include_str!("provider_m3.c")).unwrap();
        let compile = Command::new(&cc)
            .args(["-std=c11", "-O1", "-c"])
            .arg(&c)
            .arg("-I")
            .arg(root.join("crates/hirsel-core-rt/include"))
            .arg("-o")
            .arg(&object)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let linker = dir.path().join("cc");
        let mut script = std::fs::File::create(&linker).unwrap();
        // Paths are our generated scratch/toolchain paths, with POSIX single-quote escaping.
        let quote = |p: &Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
        writeln!(script, "#!/bin/sh\nexec {} \"$@\" {} -Wl,--wrap=hirsel_core_os_run -Wl,--wrap=hirsel_core_spawn -Wl,--wrap=hirsel_core_receive -Wl,--wrap=hirsel_core_send -Wl,--wrap=hirsel_core_cancel -Wl,--wrap=hirsel_core_host_op", quote(&cc), quote(&object)).unwrap();
        drop(script);
        std::fs::set_permissions(&linker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut options = native_options(backend, dir.path());
        options.cc = linker;
        options.root_limit = 1 << 20;
        options.grant_mask = (1 << 9) | (1 << 10);
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let built = product.build(&entry, &options).unwrap();
        for case in [
            "read",
            "errors",
            "append0",
            "append1",
            "append3",
            "before-read",
            "issue-read",
            "delivery-read",
            "issue-write",
            "delivery-write",
            "blocked-receive",
            "blocked-send",
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
        let source = "pub fn main(out:Stdout, err:Stderr)->Result[(),Exit] { result.unwrap(stderr.write(err,\"MAIN RAN\")); result.unwrap(stdout.write(out,\"out\")); Ok(()) }";
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
        assert_eq!(output.status.code(), Some(103), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"not started: host grants could not be configured: Bad file descriptor (os error 9)\n"
        );
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn closed_stdout_is_named_before_main_cranelift() {
        closed_stdout(Backend::Cranelift);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn closed_stdout_is_named_before_main_llvm() {
        closed_stdout(Backend::Llvm);
    }
}

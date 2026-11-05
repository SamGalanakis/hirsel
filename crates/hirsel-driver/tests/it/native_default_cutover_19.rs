//! Public fresh build readiness through the compiler-owned native query graph.
use hirsel_driver::native;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_native::Backend;
use hirsel_native::BuildProfile;
use hirsel_native::TargetSpec;

fn public_record_replay(backend: Backend) {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.hir");
        let tape = directory.path().join("recording");
        let text = "pub fn main(rt:Io,args:Args)->Result[(),GrantError]{let _=out.print(io.stdout(rt)?,\"cutover replay\\n\");Ok(())}";
        std::fs::write(&source, text).unwrap();
        crate::image_fixture::core_manifest(&source, &directory.path().join("main.hir.json"));
        let run = |launch| {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let outcome = native::run_path_native_backend_with_validation(
                &source,
                launch,
                None,
                backend,
                native::Output::Streams {
                    stdout: &mut stdout,
                    stderr: &mut stderr,
                },
                hirsel_driver::session::Policy::default(),
                hirsel_native::Validation::EveryStage,
            );
            (outcome, stdout, stderr)
        };
        let launch = hirsel_host_io::launch::HostLaunch::Live {
            config: hirsel_host_io::launch::LaunchConfig::Grants {
                input: std::ffi::OsStr::new(
                    r#"{"io":{"stdout":"stdout.bytes@1","stderr":"stderr.bytes@1","dirs":[]},"args":[]}"#,
                ),
                arguments: None,
            },
            record: Some(&tape),
            revocations: None,
        };
        let (recorded, stdout, stderr) = run(launch);
        assert_eq!(recorded.unwrap().unwrap().exit_code(), 0);
        assert_eq!(stdout, b"cutover replay\n");
        assert!(stderr.is_empty(), "{backend:?}: {stderr:?}");
        assert!(tape.is_file(), "recording was not published");
        let (replayed, replay_stdout, replay_stderr) =
            run(hirsel_host_io::launch::HostLaunch::Replay(&tape));
        assert_eq!(replayed.unwrap().unwrap().exit_code(), 0);
        assert_eq!(replay_stdout, stdout);
        assert_eq!(replay_stderr, stderr);
        std::fs::write(&source, text.replace("cutover replay", "changed replay")).unwrap();
        let (refused, stdout, stderr) = run(hirsel_host_io::launch::HostLaunch::Replay(&tape));
        let Err(hirsel_engine::ToolError::Runtime(message)) = refused else {
            panic!("changed source replay was not refused");
        };
        assert!(message.contains("replay refused"), "{message}");
        assert!(
            stdout.is_empty() && stderr.is_empty(),
            "refusal must precede entry"
        );
    });
}

#[test]
fn public_record_replay_cranelift() {
    public_record_replay(Backend::Cranelift);
}

#[test]
fn public_record_replay_llvm() {
    public_record_replay(Backend::Llvm);
}

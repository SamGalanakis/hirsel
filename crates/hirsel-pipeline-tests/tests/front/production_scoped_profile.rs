//! The production release pipeline must eliminate a synchronous closure environment.
#[test]
fn production_release_has_zero_scoped_closure_environment_allocations() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        use hirsel_native::{Backend, BuildProfile, TargetSpec, Validation};
        use hirsel_requests::{
            CompileOperation, CompileResult, CompilerRequests, NativeOutput, NativeRequest,
        };
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("scoped-profile.hir");
        std::fs::write(&source, "fn invoke(f:fn()->u64 uses {})->u64 uses {f}{f()}\npub fn main()->Result[(),u64]{let n=3u64;Error(invoke(fn()->u64 uses {}{n}))}").unwrap();
        let compiler = hirsel_front_phases::COMPILER;
        let mut requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration());
        let output = requests
            .execute(
                &source,
                CompileOperation::NativeBuild(NativeRequest {
                    definition: None,
                    test_entries: vec![],
                    supplemental_sources: vec![],
                    output: NativeOutput::Ready,
                    options: BuildProfile::Release
                        .options(
                            Backend::Llvm,
                            TargetSpec::x86_64_unknown_linux_gnu(),
                            Validation::EveryStage,
                        )
                        .unwrap(),
                    run: None,
                    evidence: None,
                    platform: None,
                }),
            )
            .unwrap();
        let CompileResult::NativeReady(ready) = &output.result else {
            panic!("production release request did not return Ready");
        };
        let run =
            hirsel_native_testing::run_heap(&ready.program, &ready.entry, &ready.sites()).unwrap();
        assert!(run.observation.failures.is_empty());
        assert_eq!(run.observation.exit_code, Some(1));
        assert_eq!(
            (
                run.counters.allocations,
                run.counters.frees,
                run.counters.releases
            ),
            (0, 0, 0),
            "production release closure environment",
        );
    });
}

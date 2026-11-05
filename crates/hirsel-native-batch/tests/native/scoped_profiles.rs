use hirsel_engine::DefinitionQueries;
use hirsel_native::*;
use hirsel_native_testing::*;
#[test]
fn profile_switch_keeps_debug_heap_and_release_zero_environment() {
    hirsel_engine::with_compiler_stack(|| {
        let source = "fn invoke(f:fn()->u64 uses {})->u64 uses {f}{f()}\npub fn main()->Result[(),u64]{let n=3u64;Error(invoke(fn()->u64 uses {}{n}))}";
        let mut engine = hirsel_engine::QueryEngine::new(
            hirsel_engine::default_environment(),
            hirsel_front_phases::compiler_adapters(),
        );
        let file = engine
            .sources
            .add("scoped-profile.hir", source.into())
            .unwrap();
        let checked = hirsel_engine::source_check_request(
            &mut engine,
            file,
            &hirsel_engine::default_context(),
        )
        .unwrap();
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let raw = engine.export_native_input().unwrap();
        let input = engine.import_native_input(raw).unwrap();
        let entry = hirsel_engine::entry_path(&hirsel_engine::default_context());
        let sites = hirsel_native::Sites::new(input.snapshot());
        for profile in [
            BuildProfile::Debug,
            BuildProfile::Default,
            BuildProfile::Release,
            BuildProfile::Profiling,
            BuildProfile::ReleaseLto,
        ] {
            let options = profile
                .options(
                    hirsel_native::Backend::Llvm,
                    TargetSpec::x86_64_unknown_linux_gnu(),
                    hirsel_native::Validation::EveryStage,
                )
                .unwrap();
            let release = matches!(
                profile,
                BuildProfile::Release | BuildProfile::ReleaseLto | BuildProfile::Profiling
            );
            let ready = crate::build(&input, &options.passes).unwrap();
            let run = hirsel_native_testing::run_heap(&ready, &entry, &sites).unwrap();
            assert!(run.observation.failures.is_empty());
            assert_eq!(run.observation.exit_code, Some(1));
            assert_eq!(run.counters.allocations, u64::from(!release), "{profile:?}");
            assert_eq!(run.counters.frees, u64::from(!release), "{profile:?}");
            assert_eq!(run.counters.releases, u64::from(!release), "{profile:?}");
        }
    });
}

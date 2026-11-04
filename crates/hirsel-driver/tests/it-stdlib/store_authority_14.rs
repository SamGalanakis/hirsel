//! Bounded std-store success/failure vectors from the original DET crash cases.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use std::path::PathBuf;

#[test]
fn compaction_success_and_injected_failure_agree_on_h_llvm_and_cranelift() {
    with_compiler_stack(|| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/store-authority-14/compaction.hir");
        let mut loaded = load_program(&path).unwrap();
        loaded
            .check_for_execution()
            .unwrap_or_else(|error| panic!("{error}"));
        let config = RunConfiguration {
            entry: loaded.entry.clone(),
            hash_seed: 0,
            inputs: vec![],
        };
        let execution = hirsel_engine::run_observed_checked(
            &mut loaded.engine,
            &config,
            SimulationConfig::default(),
        )
        .unwrap();
        let reference =
            hirsel_native::Observation::of_reference(execution.observation.as_ref().unwrap());
        assert_eq!(reference.exit_code, Some(0), "{reference:?}");
        assert!(reference.failures.is_empty(), "{reference:?}");
        let input = loaded.engine.export_native_input().unwrap();
        let validated = loaded.engine.import_native_input(input).unwrap();
        let options =
            hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
        let configurations = vec![
            hirsel_native_testing::Configuration {
                options: options.clone(),
                engine: hirsel_native_testing::Engine::Heap { campaign: false },
            },
            hirsel_native_testing::Configuration {
                options: options.clone(),
                engine: hirsel_native_testing::Engine::Native {
                    opt: hirsel_native::OptLevel::Speed,
                    partition: hirsel_native::Partition::Whole,
                    runtime: hirsel_native_testing::Runtime::Diagnostic,
                },
            },
            hirsel_native_testing::Configuration {
                options,
                engine: hirsel_native_testing::Engine::Cranelift {
                    partition: hirsel_native::Partition::Whole,
                    runtime: hirsel_native_testing::Runtime::Diagnostic,
                },
            },
        ];
        let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("store-authority-14");
        std::fs::create_dir_all(&scratch).unwrap();
        let toolchain =
            hirsel_native_testing::toolchain_for_matrix(&scratch.join("runtime"), &configurations)
                .unwrap();
        let outcomes = hirsel_native_testing::differential(
            &validated,
            &loaded.entry,
            &reference,
            &configurations,
            &toolchain,
            &scratch,
        );
        assert_eq!(outcomes.len(), 3);
        for (configuration, outcome) in &outcomes {
            eprintln!("{configuration}: {outcome:?}");
        }
        for (configuration, outcome) in outcomes {
            assert!(
                matches!(outcome, hirsel_native_testing::Outcome::Agree),
                "{configuration}: {outcome:?}"
            );
        }
    });
}

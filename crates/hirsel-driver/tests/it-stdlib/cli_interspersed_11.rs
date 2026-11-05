//! Interspersed parsing, exact errors and unchanged stop-at-positional controls.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/cli-interspersed-11/{name}.hir"))
}

fn all_engines(name: &str) {
    with_compiler_stack(|| {
        let mut loaded = load_program(&fixture(name)).unwrap();
        loaded.check_for_execution().unwrap_or_else(|e| match e {
            ToolError::Query { error, .. } => panic!("{error:?}"),
            e => panic!("{e}"),
        });
        let reference = run_observed_snapshot(
            &mut loaded.engine,
            &RunConfiguration {
                entry: loaded.entry.clone(),
                hash_seed: 0,
                inputs: vec![],
            },
            SimulationConfig::default(),
        )
        .unwrap();
        assert!(
            matches!(
                reference.outcome,
                Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
            ),
            "{name}: {:?}",
            reference.outcome
        );
        let expected =
            hirsel_native::Observation::of_reference(reference.observation.as_ref().unwrap());
        let input = loaded.engine.export_native_input().unwrap();
        let validated = loaded.engine.import_native_input(input).unwrap();
        let options =
            hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
        let ready = hirsel_native_batch::build(&validated, &options).unwrap();
        let sites = hirsel_native::Sites::new(validated.snapshot());
        let heap = hirsel_native_testing::run_heap_in(
            &ready,
            &loaded.entry,
            &sites,
            &hirsel_native_testing::HeapWorld::default(),
        )
        .unwrap();
        assert_eq!(heap.observation, expected);
        assert_eq!(heap.counters.live_heap_bytes, 0);
        let configs = [
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
        let evidence = std::env::var_os("HIRSEL_CLI_INTERSPERSED_11_EVIDENCE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = evidence.join(format!("cli-interspersed-11-{}-{name}", std::process::id()));
        let tools =
            hirsel_native_testing::toolchain_for_matrix(&dir.join("runtime"), &configs).unwrap();
        let mut count = 0;
        for (config, outcome) in hirsel_native_testing::differential(
            &validated,
            &loaded.entry,
            &expected,
            &configs,
            &tools,
            &dir,
        ) {
            assert!(
                matches!(outcome, hirsel_native_testing::Outcome::Agree),
                "{name}: {config}: {outcome:?}"
            );
            count += 1;
        }
        assert_eq!(count, 2);
        eprintln!("cli-interspersed-11 {name}: interpreter + H + LLVM + Cranelift agree");
    });
}

#[test]
fn clap_and_hirsel_vectors_all_engines() {
    all_engines("vectors");
}

#[test]
fn exact_errors_and_original_indices_all_engines() {
    all_engines("errors");
}

#[test]
fn old_parse_controls_all_engines() {
    all_engines("controls");
}

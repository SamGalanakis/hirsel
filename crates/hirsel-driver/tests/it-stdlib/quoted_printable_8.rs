//! Go binary writer/compatible reader vectors plus independent byte and offset controls.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use std::path::PathBuf;

fn check(name: &str, cases: usize) {
    with_compiler_stack(|| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/quoted-printable-8-{name}.hir"));
        let mut loaded = load_program(&path).unwrap_or_else(|e| panic!("{e}"));
        loaded
            .check_for_execution()
            .unwrap_or_else(|e| panic!("{e}"));
        // Fixed bounded Rust harness for the long-line/generated controls;
        // no source-corpus budget is changed.
        loaded.engine.adapters.interpreter = Some(Box::new(InterpreterAdapter {
            limits: hirsel_interp::Limits {
                steps: Some(20_000_000),
                call_depth: 256,
                source: None,
            },
            ..Default::default()
        }));
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
        let options =
            hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
        let configs = vec![
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
        let dir =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("quoted-printable-8-{name}"));
        let tools =
            hirsel_native_testing::toolchain_for_matrix(&dir.join("runtime"), &configs).unwrap();
        let mut ready = crate::native_ready::supplier(&path, None, &loaded.entry, &tools).unwrap();
        for (config, outcome) in hirsel_native_testing::differential_ready(
            &mut ready,
            &loaded.entry,
            &expected,
            &configs,
            &tools,
            &dir,
            &hirsel_native::Fixture {
                steps: Some(20_000_000),
                ..Default::default()
            },
        ) {
            assert!(
                matches!(outcome, hirsel_native_testing::Outcome::Agree),
                "{name}: {config}: {outcome:?}"
            );
        }
        eprintln!("quoted-printable-8 {name}: {cases} cases on interpreter, H, LLVM, Cranelift");
    });
}

#[test]
fn writer_vectors() {
    check("writer", 42);
}
#[test]
fn reader_vectors() {
    check("reader", 51);
}
#[test]
fn long_lines() {
    check("long", 3);
}
#[test]
fn generated_controls() {
    check("generated", 1025);
}

#[test]
fn mime_body() {
    check("mime", 4);
}

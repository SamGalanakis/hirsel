//! PROG-3's return contract on the interpreter, checked interpreter, heap and both emitters.
use hirsel_driver::native;
use hirsel_engine::{DefinitionQueries, RunConfiguration, SimulationConfig};
use hirsel_front_phases::load_program_source;
use hirsel_intern::jobs::with_compiler_stack;

#[test]
fn exit_code_16_all_entry_shapes_and_status_bytes_agree() {
    with_compiler_stack(|| {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/std_numeric.hir");
        let mut matrix = hirsel_native_testing::trace_roots_matrix();
        matrix.push(hirsel_native_testing::Configuration {
            options: hirsel_native::Options::new(
                hirsel_native::TargetSpec::x86_64_unknown_linux_gnu(),
            ),
            engine: hirsel_native_testing::Engine::Heap { campaign: false },
        });
        let scratch = tempfile::tempdir().unwrap();
        let tools =
            hirsel_native_testing::toolchain_for_matrix(&scratch.path().join("runtime"), &matrix)
                .unwrap();
        for wrapped in [false, true] {
            for code in [0, 1, 2, 255] {
                let ty = if wrapped {
                    "Result[std@1/process.ExitCode, String]"
                } else {
                    "std@1/process.ExitCode"
                };
                let value = match code {
                    0 => "std@1/process.SUCCESS".to_owned(),
                    1 => "std@1/process.FAILURE".to_owned(),
                    n => format!("std@1/process.exit_code({n}u8)"),
                };
                let value = if wrapped {
                    format!("Ok({value})")
                } else {
                    value
                };
                let source = format!("pub fn main() -> {ty} {{ {value} }}");
                let mut program = load_program_source(&fixture, Some(&source)).unwrap();
                program.check_for_execution().unwrap();
                let run = RunConfiguration {
                    entry: program.entry.clone(),
                    hash_seed: 0,
                    inputs: vec![],
                };
                let reference = hirsel_engine::run_observed_snapshot(
                    &mut program.engine,
                    &run,
                    SimulationConfig::default(),
                )
                .unwrap();
                assert_eq!(
                    reference.outcome.as_ref().unwrap().exit_code(),
                    code,
                    "{source}"
                );
                let checked = hirsel_engine::run_observed_checked(
                    &mut program.engine,
                    &run,
                    SimulationConfig::default(),
                )
                .unwrap();
                assert_eq!(reference.observation, checked.observation, "{source}");
                let observation = hirsel_native::Observation::of_reference(
                    reference.observation.as_ref().unwrap(),
                );
                let input = program.engine.export_native_input().unwrap();
                let validated = program.engine.import_native_input(input).unwrap();
                let directory = scratch.path().join(format!("{wrapped}-{code}"));
                std::fs::create_dir_all(&directory).unwrap();
                let outcomes = hirsel_native_testing::differential(
                    &validated,
                    &run.entry,
                    &observation,
                    &matrix,
                    &tools,
                    &directory,
                );
                assert_eq!(outcomes.len(), 3);
                for (configuration, outcome) in outcomes {
                    assert!(
                        matches!(outcome, hirsel_native_testing::Outcome::Agree),
                        "{source}: {configuration}: {outcome:?}"
                    );
                }
            }
        }
    });
}

#[test]
fn exit_code_16_entry_admission_rejects_u8_and_nominal_lookalikes() {
    with_compiler_stack(|| {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/std_numeric.hir");
        for source in [
            "pub fn main() -> u8 { 2u8 }",
            "pub type ExitCode { code: u8 }\npub fn main() -> ExitCode { ExitCode(code: 2u8) }",
            "pub fn main() -> Result[u8, ()] { Ok(2u8) }",
        ] {
            let mut program = load_program_source(&fixture, Some(source))
                .unwrap_or_else(|error| panic!("{error}"));
            program.check_for_execution().unwrap();
            let signature = program.engine.item_signature(&program.entry).unwrap().value;
            assert!(
                hirsel_engine_admission::entry_roots(
                    program.engine.types(),
                    &signature,
                    hirsel_run_schema::LaunchKind::Main
                )
                .is_err(),
                "{source}"
            );
        }
    });
}

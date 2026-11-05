use hirsel_driver::native;
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;
use std::path::Path;

#[test]
fn native_allocation_failure_releases_an_already_prepared_resource_argument() {
    with_compiler_stack(|| {
        for (cap, output) in [
            (7, b"released\nout of space\n".as_slice()),
            (8, b"entered\nreleased\nfits\n".as_slice()),
        ] {
            // A u64 element charges eight logical bytes. The resource is the
            // first prepared argument; the second argument crosses that cap.
            let source = format!(
                r#"
resource type Trace {{ stdout: Stdout }}
drop(x) {{ let _ = out.print(x.stdout, "released\n") }}
fn make_list(a: Arena) -> List[u64] in a {{ [7u64] }}
fn consume(take guard: Trace, values: List[u64]) -> u64 {{
  let _ = out.print(guard.stdout, "entered\n")
  list.len(values)
}}
pub fn main(rt: Io) -> Result[(), GrantError] {{
  let stdout = io.stdout(rt)?
  let outcome: Result[u64, OutOfSpace] = arena.bounded({cap}u64, fn(a: Arena) -> u64 {{
    consume(Trace(stdout: stdout), make_list(a))
  }})
  let _ = out.print(stdout, match outcome {{ Ok(n) => if n == 1 {{ "fits\n" }} else {{ "wrong\n" }}, Error(_) => "out of space\n" }})
  Ok(())
}}
"#
            );
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../conformance/MEM/MEM-6-exact-cap-fits-run.hir");
            let mut program =
                load_program_source(&path, Some(&source)).unwrap_or_else(|error| match error {
                    ToolError::Query { error, .. } => panic!("{error:?}"),
                    error => panic!("{error}"),
                });
            program
                .check_for_execution()
                .unwrap_or_else(|error| match error {
                    ToolError::Query { error, .. } => panic!("{error:?}"),
                    error => panic!("{error}"),
                });
            let run = RunConfiguration {
                entry: program.entry.clone(),
                hash_seed: 0,
                inputs: vec![],
            };
            let reference =
                run_observed_snapshot(&mut program.engine, &run, Default::default()).unwrap();
            let expected = hirsel_native::Observation {
                exit_code: Some(0),
                blocked: false,
                budget: None,
                signaled: None,
                output: output.to_vec(),
                failures: vec![],
                roots: vec![],
            };
            assert_eq!(
                hirsel_native::Observation::of_reference(reference.observation.as_ref().unwrap()),
                expected
            );
            let input = program.engine.export_native_input().unwrap();
            let validated = program.engine.import_native_input(input).unwrap();
            let options =
                hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
            let matrix = [
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
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("prepared-argument-failure-{cap}"));
            let tools =
                hirsel_native_testing::toolchain_for_matrix(&dir.join("runtime"), &matrix).unwrap();
            let outcomes = hirsel_native_testing::differential(
                &validated,
                &program.entry,
                &expected,
                &matrix,
                &tools,
                &dir,
            );
            assert_eq!(outcomes.len(), 3);
            for (configuration, outcome) in outcomes {
                assert_eq!(
                    outcome,
                    hirsel_native_testing::Outcome::Agree,
                    "cap={cap}/{configuration}: {outcome:?}"
                );
            }
        }
    });
}

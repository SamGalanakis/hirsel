//! FAIL-2 and MODE-4: isolate transfers sendable unique results to its caller.
use crate::rc_support::target;
use hirsel_intern::jobs::with_compiler_stack;

const UNIQUE_BYTES: &str = r#"
pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
  let tasks = io.tasks(rt)?
  let caught: Result[unique Bytes, Failure] = task.isolate(tasks, fn() -> unique Bytes { unique b"x" })
  match take caught {
    Ok(value) => failure.check(value == b"x", Arithmetic()),
    Error(_) => failure.check(false, Arithmetic()),
  }
  Ok(())
}
"#;

#[test]
#[ignore = "native: isolate owned results on H, checked heap, LLVM and Cranelift"]
fn isolate_returns_unique_bytes() {
    native_parity(&[("unique-bytes", UNIQUE_BYTES)]);
}

#[test]
fn isolate_rejects_borrowed_counted_capture_once() {
    use hirsel_engine::*;
    use hirsel_front_phases::compiler_adapters;
    with_compiler_stack(|| {
        let source =
            include_str!("../../../../conformance/CLO/CLO-3-isolate-unique-bytes-borrow-bad.hir");
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine.sources.add("borrowed.hir", source.into()).unwrap();
        let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
        assert_eq!(checked.diagnostics.len(), 1, "{:?}", checked.diagnostics);
        assert_eq!(checked.diagnostics[0].rule.as_str(), "CLO-3");
    });
}

fn native_parity(cases: &[(&str, &str)]) {
    use hirsel_driver::native::*;
    use hirsel_driver::*;
    use hirsel_engine::*;
    use hirsel_front_phases::*;
    use hirsel_host_io::launch::*;
    use hirsel_native::*;
    use hirsel_native_testing::*;
    let options = Options::new(target());
    let matrix = vec![
        Configuration {
            options: options.clone(),
            engine: Engine::Heap { campaign: true },
        },
        Configuration {
            options: options.clone(),
            engine: Engine::Native {
                opt: OptLevel::Speed,
                partition: Partition::Whole,
                runtime: Runtime::Diagnostic,
            },
        },
        Configuration {
            options,
            engine: Engine::Cranelift {
                partition: Partition::Whole,
                runtime: Runtime::Diagnostic,
            },
        },
    ];
    let directory =
        std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("isolate-bytes-leak-17");
    std::fs::create_dir_all(&directory).unwrap();
    let toolchain =
        hirsel_native_testing::toolchain_for_matrix(&directory.join("runtime"), &matrix).unwrap();
    for &(name, source) in cases {
        with_compiler_stack(|| {
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let context = default_context();
            let file = engine.sources.add("isolate.hir", source.into()).unwrap();
            let checked = source_check_request(&mut engine, file, &context).unwrap();
            assert!(
                checked.diagnostics.is_empty(),
                "{name}: {:?}",
                checked.diagnostics
            );
            let entry = entry_path(&context);
            let reference = run_observed_snapshot(
                &mut engine,
                &RunConfiguration {
                    entry: entry.clone(),
                    hash_seed: 0,
                    inputs: vec![],
                },
                SimulationConfig::default(),
            )
            .unwrap();
            let reference = Observation::of_reference(reference.observation.as_ref().unwrap());
            assert_eq!(reference.exit_code, Some(0), "{name}: {reference:?}");
            assert!(reference.failures.is_empty(), "{name}: {reference:?}");
            eprintln!("{name}: interpreter exit0");
            let snapshot = engine.export_native_input().unwrap();
            let validated = engine.import_native_input(snapshot).unwrap();
            let directory = directory.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            let mut executions = 0;
            for (configuration, outcome) in hirsel_native_testing::differential_ready(
                &mut *hirsel_requests::source_ready_supplier(
                    &source,
                    "isolate.hir",
                    &entry,
                    &toolchain,
                )
                .unwrap(),
                &entry,
                &reference,
                &matrix,
                &toolchain,
                &directory,
                &hirsel_native::Fixture::default(),
            ) {
                assert!(
                    matches!(outcome, Outcome::Agree),
                    "{name}: {configuration}: {outcome:?}"
                );
                eprintln!("{name}: {configuration}: agrees");
                executions += 1;
            }
            assert_eq!(executions, 3);
        });
    }
}

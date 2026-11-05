use hirsel_driver::native;
use hirsel_engine::{
    QueryEngine, default_context, default_environment, entry_path, source_check_request,
};
use hirsel_front_phases::compiler_adapters;
use hirsel_intern::jobs::with_compiler_stack;
use std::{fs, process::Command};

fn matrix(mapper: &str, stopped: bool) {
    with_compiler_stack(|| {
        let source = format!(
            r#"
pub fn main(rt: Io) -> Result[(), GrantError] {{
  let tasks = io.tasks(rt)?
  for repeat in 0u64..2u64 {{
    let outcome = core@1/cpu.map_scoped(tasks, 1u32,
      fn() -> List[u64] uses {{}} {{ [1u64, 2u64] }},
      fn(take job: u64) -> List[u64] uses {{}} {{ {mapper} }})
    match outcome {{
      Ok(rows) => failure.check(rows == [[1u64, 2u64], [2u64, 3u64]], Bounds()),
      Error(_) => failure.check(false, Bounds()),
    }}
  }}
  Ok(())
}}
"#
        );
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine.sources.add("cpu.hir", source).unwrap();
        let context = default_context();
        let output = source_check_request(&mut engine, file, &context).unwrap();
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let input = engine.export_native_input().unwrap();
        let validated = engine.import_native_input(input).unwrap();
        let target = hirsel_native::TargetSpec::x86_64_unknown_linux_gnu();
        let program =
            hirsel_native_batch::build(&validated, &hirsel_native::Options::new(target.clone()))
                .unwrap();
        let root = tempfile::tempdir().unwrap();
        for backend in [
            hirsel_native::Backend::Llvm,
            hirsel_native::Backend::Cranelift,
        ] {
            let objects = hirsel_native::emit_backend_objects(
                &program,
                backend,
                hirsel_native::OptLevel::Default,
                hirsel_native::Partition::Whole,
                false,
                None,
            )
            .unwrap();
            for diagnostic in [false, true] {
                for system_allocator in [false, true] {
                    let dir = root.path().join(format!(
                        "{backend:?}-diagnostic{diagnostic}-system{system_allocator}"
                    ));
                    fs::create_dir_all(&dir).unwrap();
                    let tools = hirsel_native::Toolchain::in_tree_with_runtime(
                        &dir,
                        &[&target],
                        hirsel_native::RuntimeBuild {
                            diagnostic,
                            system_allocator,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    let executable = hirsel_native::link_standalone_objects(
                        &program,
                        &entry_path(&context),
                        &objects,
                        &tools,
                        &dir,
                    )
                    .unwrap();
                    let grants = dir.join("grants.json");
                    fs::write(
                        &grants,
                        serde_json::json!({"io":{"dirs":[],"tasks":"tasks.free@1",
                            "cpu":{"provider":"cpu.scoped@1","max_workers":1,
                                "max_domains":1,"max_physical_bytes":268435456u64,
                                "max_batch_bytes":1048576}}})
                        .to_string(),
                    )
                    .unwrap();
                    let tape = dir.join("run.tape");
                    for replay in [false, true] {
                        let observation = dir.join(format!("observation-{replay}"));
                        let mut command = Command::new(&executable);
                        command.env("HIRSEL_RT_OBSERVATION", &observation);
                        if replay {
                            command.arg("--hirsel-replay").arg(&tape);
                        } else {
                            command
                                .arg("--hirsel-grants")
                                .arg(&grants)
                                .arg("--hirsel-record")
                                .arg(&tape);
                        }
                        let result = command.output().unwrap();
                        assert_eq!(
                            result.status.code(),
                            Some(if stopped {
                                hirsel_run_schema::exit::BUDGET_EXCEEDED
                            } else {
                                0
                            }),
                            "{backend:?} diagnostic={diagnostic} system={system_allocator} \
                             replay={replay}: {}",
                            String::from_utf8_lossy(&result.stderr)
                        );
                        if stopped {
                            let report = fs::read_to_string(observation).unwrap();
                            assert!(report.contains("budget_exceeded memory"), "{report}");
                        }
                    }
                }
            }
        }
    });
}

#[test]
fn cpu_heap_preflight_preserves_aggregate_cleanup_and_reentry_on_both_allocators() {
    matrix("[job, job + 1u64]", false);
}

#[test]
fn cpu_heap_preflight_preserves_backing_stop_and_replay_on_both_allocators() {
    matrix(
        "let mut storage: List[u64] = []; list.reserve(mut storage, 2000000); [job, job + list.len(storage)]",
        true,
    );
}

//! Source admissions must survive the native formatting path.
use crate::rc_support::{compile, target, unit_named};

#[test]
fn appended_format_keeps_reserve_and_finish_invocations() {
    let program = compile(
        r#"pub fn main() -> Result[(), u64] {
  let mut output: List[u8] = []
  std@1/text.append(mut output, f"{42u64},")
  Error(list.len(output))
}"#,
    );
    let lowered = hirsel_rc::import::lower(&program, &target()).unwrap();
    let unit = unit_named(&lowered, "main");
    let reference = program.operation_registry();
    let rows = &reference.invocations[&unit.header.definition];
    assert!(
        rows.iter()
            .any(|r| r.callee.as_ref().is_some_and(|p| p.is_core("list.reserve")))
    );
    assert!(rows.iter().any(|r| {
        r.callee
            .as_ref()
            .is_some_and(|p| p.is_core("$intrinsic.TextFinish"))
    }));
    assert_eq!(hirsel_lir::invocations::check(unit, rows, 0), Ok(()));
}

#[test]
fn primitive_formats_share_the_reserved_output_allocation() {
    hirsel_engine::with_compiler_stack(|| {
        let source = r#"pub fn main() -> Result[(), ()] {
  let mut output: List[u8] = []
  list.reserve(mut output, 128u64)
  for i in 0u64..16u64 {
    std@1/text.append(mut output, f"{i},")
  }
  if std@1/text.finish_valid(take output) == "0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15," {
    Ok(())
  } else {
    Error(())
  }
}"#;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../hirsel-driver/tests/fixtures/std_numeric.hir");
        let mut loaded = crate::source_std::load_program_source(&path, Some(source)).unwrap();
        loaded.check_for_execution().unwrap();
        let input = loaded.engine.export_native_input().unwrap();
        let sources = hirsel_driver::native::profile_sources(&input);
        let validated = loaded.engine.import_native_input(input).unwrap();
        let sites = hirsel_driver::native::Sites::new(validated.snapshot());
        let ready = hirsel_driver::native::build(
            &validated,
            &hirsel_driver::native::Options::new(target()),
        )
        .unwrap();
        let run = hirsel_driver::native::run_heap(&ready, &loaded.entry, &sites).unwrap();
        assert_eq!(run.observation.exit_code, Some(0));
        assert!(run.observation.failures.is_empty(), "{:?}", run.observation);
        use hirsel_driver::native::{OptLevel, Partition, RuntimeBuild, Toolchain};
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("fstring-append-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = target();
        let tools = Toolchain::in_tree_with_runtime(
            &dir.join("runtime"),
            &[&target],
            RuntimeBuild {
                measurement: true,
                ..Default::default()
            },
        )
        .unwrap();
        let exe = hirsel_driver::native::link_profile(
            &ready,
            &loaded.entry,
            OptLevel::Speed,
            Partition::Whole,
            &tools,
            &dir,
            Some(&sources),
        )
        .unwrap();
        let totals = dir.join("totals");
        let run = std::process::Command::new(exe)
            .env_clear()
            .env("HIRSEL_RT_MEASUREMENT", &totals)
            .output()
            .unwrap();
        assert!(run.status.success(), "{run:?}");
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(totals.with_extension("sites.json")).unwrap())
                .unwrap();
        let append_line = source
            .lines()
            .position(|line| line.contains("std@1/text.append"))
            .unwrap()
            + 1;
        let allocations: u64 = report["sites"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|site| {
                site["line"] == append_line
                    && site["function"]
                        .as_str()
                        .is_some_and(|name| name.ends_with("/main"))
            })
            .map(|site| site["counts"]["allocations"].as_u64().unwrap_or(0))
            .sum();
        // The 38 hand-counted bytes fit in the one reserved output block.
        // Per-format scratch allocations violate this bound even if freed.
        assert_eq!(allocations, 0, "{report}");
        std::fs::remove_dir_all(dir).unwrap();
    });
}

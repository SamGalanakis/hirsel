use crate::build;
use hirsel_native::stages::*;
use hirsel_native::*;
use hirsel_native_batch::*;
use hirsel_native_testing::*;

/// Operational budgets stop below LANG-10 and do not start user cleanup.
#[test]
fn native_logical_depth_does_not_turn_operational_budgets_into_failures() {
    use crate::Backend;
    use crate::OptLevel;
    use crate::Options;
    use crate::Partition;
    use crate::Sites;
    use crate::Tape;
    use hirsel_native as native;
    use hirsel_run_schema::{BudgetKind, Budgets, IoGrants, LaunchKind, RunOutcome};
    hirsel_engine::with_compiler_stack(|| {
        let source = r#"
resource type Trace {stdout:Stdout}
drop(x) {let _=out.print(x.stdout,"cleanup\n")}
fn descend(n:u64)->u64 {if n==0u64 {0u64}else {1u64+descend(n-1u64)}}
pub fn main(rt:Io)->Result[(),GrantError] {
 let trace=Trace(stdout:io.stdout(rt)?)
 let mut xs:List[u64]=[]
 list.reserve(mut xs,32u64)
 list.push(mut xs,7u64)
 let _=descend(40u64)+list.len(xs)
 Ok(())
}
"#;
        let mut engine = hirsel_engine::QueryEngine::new(
            hirsel_engine::default_environment(),
            hirsel_front_phases::compiler_adapters(),
        );
        let file = engine.sources.add("budget.hir", source.into()).unwrap();
        let checked = hirsel_engine::source_check_request(
            &mut engine,
            file,
            &hirsel_engine::default_context(),
        )
        .unwrap();
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let entry = hirsel_engine::entry_path(&hirsel_engine::default_context());
        let config = hirsel_engine::RunConfiguration {
            entry: entry.clone(),
            hash_seed: 0,
            inputs: vec![],
        };
        let reference = hirsel_engine::run_observed_snapshot(
            &mut engine,
            &config,
            hirsel_engine::SimulationConfig::default(),
        )
        .unwrap();
        let reference = crate::Observation::of_reference(reference.observation.as_ref().unwrap());
        assert_eq!(reference.output, b"cleanup\n");
        assert!(reference.failures.is_empty());
        let raw = engine.export_native_input().unwrap();
        let validated = engine.import_native_input(raw).unwrap();
        let scratch = hirsel_native::executable::Scratch::new().unwrap();
        let tools = hirsel_native_testing::toolchain_for_matrix(
            &scratch.0,
            &hirsel_native_testing::trace_roots_matrix(),
        )
        .unwrap();
        let mut ready = crate::build(
            &validated,
            &Options::new(TargetSpec::x86_64_unknown_linux_gnu()),
        )
        .unwrap();
        ready.fuel = true;
        let sites = Sites::new(validated.snapshot());
        for backend in [Backend::Llvm, Backend::Cranelift] {
            let objects = crate::emit_backend_objects(
                &ready,
                backend,
                OptLevel::None,
                Partition::Whole,
                false,
                None,
            )
            .unwrap();
            for kind in [None, Some(BudgetKind::Fuel), Some(BudgetKind::Memory)] {
                let dir = scratch
                    .0
                    .join(format!("logical-depth-budget-{backend:?}-{kind:?}"));
                std::fs::create_dir_all(&dir).unwrap();
                let mut record = hirsel_run_config::RunConfig::for_program(
                    ready.revision(&entry).unwrap(),
                    LaunchKind::Main,
                );
                record.inputs = ready.launch.clone();
                record.io = Some(IoGrants {
                    stdout: Some("stdout.bytes@1".into()),
                    ..Default::default()
                });
                record.budgets = kind.map(|kind| Budgets {
                    provider: "budgets.limits@2".into(),
                    fuel: (kind == BudgetKind::Fuel).then_some(10),
                    memory_bytes: (kind == BudgetKind::Memory).then_some(1),
                });
                let run = record.validated().unwrap();
                let runnable =
                    crate::link_runnable(&ready, &entry, &objects, &tools, &dir, Some(&run))
                        .unwrap();
                assert_eq!(
                    hirsel_run_config::RunConfig::read_path(&dir.join("run.json")).unwrap(),
                    run
                );
                let tape = dir.join("budget.tape");
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                let launched = hirsel_native::link::launch_live(
                    &runnable,
                    &ready.target,
                    &tools,
                    &dir,
                    Tape::Live {
                        record: Some(&tape),
                        revocations: None,
                    },
                    hirsel_native::link::LiveRun {
                        run: &run,
                        trace_events: None,
                        inherit: false,
                        stdout: &mut stdout,
                        stderr: &mut stderr,
                    },
                )
                .unwrap();
                let recording = hirsel_rt::launch::parse_recording(
                    &hirsel_rt::launch::read_recording_text(&tape).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    recording.bound_run().unwrap(),
                    run,
                    "the runtime consumed a different budget profile"
                );
                assert_eq!(launched.stdout, stdout);
                let observation = crate::observe(&launched, &ready, &sites).unwrap();
                assert_eq!(
                    observation.budget, kind,
                    "{backend:?}/{kind:?}: {observation:?}"
                );
                assert!(observation.failures.is_empty(), "{observation:?}");
                if let Some(kind) = kind {
                    assert_eq!(
                        observation.outcome(&sites, vec![]).unwrap(),
                        RunOutcome::BudgetExceeded(kind)
                    );
                    assert!(
                        observation.output.is_empty(),
                        "user cleanup ran: {observation:?}"
                    );
                } else {
                    assert_eq!(observation, reference);
                }
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    });
}

#[test]
fn http_reject_lowering_has_valid_projections() {
    hirsel_engine::with_compiler_stack(|| {
        use hirsel_engine::DefinitionQueries;
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/std@1/hirsel.json");
        let mut package = hirsel_front_phases::COMPILER
            .load_package_declarations(&manifest)
            .unwrap();
        let root = hirsel_syntax::DefPath(hirsel_syntax::ItemPath {
            package: package.package.clone(),
            modules: vec!["http".into()],
            name: "http_server_reject".into(),
        });
        // This private root is absent from ordinary program reachability.
        // Keep its independent V projection regression covered.
        package.engine.lower_to_tir(&root).unwrap();
    });
}

// A contract helper's closure is addressed and called in the same singleton
// object. An untyped LLVM address declaration conflicts with the bound call.
#[test]
fn ordinary_store_delete_has_one_consistent_entry_abi() {
    hirsel_engine::with_compiler_stack(|| {
        use hirsel_engine::DefinitionQueries;
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/std@1/hirsel.json");
        let mut package = hirsel_front_phases::COMPILER
            .load_package_declarations(&manifest)
            .unwrap();
        let root = hirsel_syntax::DefPath(hirsel_syntax::ItemPath {
            package: package.package.clone(),
            modules: vec!["store".into()],
            name: "tx_delete".into(),
        });
        let input = package.engine.native_input_for(&[root]).unwrap();
        for backend in [
            hirsel_native::Backend::Cranelift,
            hirsel_native::Backend::Llvm,
        ] {
            for opt in [
                hirsel_lir::emission::OptLevel::None,
                hirsel_lir::emission::OptLevel::Default,
                hirsel_lir::emission::OptLevel::Speed,
            ] {
                let options =
                    hirsel_native::Options::new(hirsel_lir::TargetSpec::x86_64_unknown_linux_gnu());
                let ready = crate::build(&input, &options)
                    .unwrap_or_else(|e| panic!("{backend:?} {opt:?}: {e}"));
                let sources = hirsel_native::profile_sources(input.snapshot());
                hirsel_native::emit_backend_objects(
                    &ready,
                    backend,
                    opt,
                    hirsel_native::Partition::Singleton,
                    false,
                    (opt == hirsel_lir::emission::OptLevel::None).then_some(&sources),
                )
                .unwrap_or_else(|e| panic!("{backend:?} {opt:?}: {e}"));
            }
        }
    });
}

use hirsel_syntax::DefPath;

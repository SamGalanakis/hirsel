//! Typed-table end-to-end scenarios exceed the corpus's 1M-step bound with
//! the real durable store/checksum implementation. Keep them bounded here.
//! Each runs on the interpreter and, except the std-rooted ones, on native.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/typed_tables")
        .join(format!("{name}.hir"))
}

fn run(name: &str, path: &Path, native: bool) {
    with_compiler_stack(|| {
        let mut loaded = load_program(path).unwrap();
        loaded.check_for_execution().unwrap();
        loaded.engine.adapters.interpreter = Some(Box::new(InterpreterAdapter {
            limits: hirsel_interp::Limits {
                steps: Some(16_000_000),
                call_depth: 256,
                source: None,
            },
            ..Default::default()
        }));
        let run = run_observed_snapshot(
            &mut loaded.engine,
            &RunConfiguration {
                entry: loaded.entry,
                hash_seed: 0,
                inputs: vec![],
            },
            SimulationConfig::default(),
        )
        .unwrap();
        assert!(
            matches!(
                run.outcome,
                Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
            ),
            "{name}: {:?}",
            run.outcome
        );
        if !native {
            return;
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = native::run_path_native_backend_with_validation(
            path,
            HostLaunch::Live {
                config: LaunchConfig::Grants {
                    input: std::ffi::OsStr::new("{}"),
                    arguments: None,
                },
                record: None,
                revocations: None,
            },
            None,
            hirsel_native::Backend::Llvm,
            native::Output::Streams {
                stdout: &mut stdout,
                stderr: &mut stderr,
            },
            hirsel_driver::session::Policy::default(),
            hirsel_native::Validation::EveryStage,
        )
        .unwrap()
        .unwrap();
        assert!(
            matches!(
                outcome,
                RunOutcome::Returned(hirsel_run_schema::Returned::Ok)
            ),
            "{name} native: {outcome:?} {}",
            String::from_utf8_lossy(&stderr)
        );
    });
}

fn execute(name: &str) {
    run(name, &fixture(name), true);
}

/// Runs a fixture as a module of std itself, for the migration engine that
/// std keeps private until `table.migrate` takes an Entry (STORE-9). Native
/// refuses a program rooted in the whole of std (its accepted-execution record
/// does not encode), so these run on the interpreter only.
fn execute_in_std(name: &str) {
    let stdlib = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib/std@1")
        .canonicalize()
        .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(stdlib.join("hirsel.json")).unwrap()).unwrap();
    for source in manifest["sources"].as_array_mut().unwrap() {
        let at = stdlib.join(source["path"].as_str().unwrap());
        source["path"] = at.to_str().unwrap().into();
    }
    manifest["sources"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"path": format!("{name}.hir"), "module": ["table_fixture"]}));
    manifest["entry"] = serde_json::json!({"module": ["table_fixture"], "name": "main"});
    let dir = tempfile::Builder::new()
        .prefix("hirsel-table-std-")
        .tempdir()
        .unwrap();
    let path = dir.path().join(format!("{name}.hir"));
    std::fs::copy(fixture(name), &path).unwrap();
    std::fs::write(path.with_extension("hir.json"), manifest.to_string()).unwrap();
    run(name, &path, false);
}

#[test]
fn typed_rows_reopen_and_ordered_ranges() {
    execute("STORE-5-typed-rows-run");
}
#[test]
fn refusals_leave_the_transaction_unchanged() {
    execute("STORE-5-typed-limits-run");
}
#[test]
fn version_reorder_rename_and_type_change_history() {
    execute("STORE-7-schema-history-run");
}
#[test]
fn reordered_record_keys_need_a_migration() {
    execute("STORE-7-key-reorder-run");
}
#[test]
fn renaming_one_owner_splits_shared_record_shapes() {
    execute("STORE-7-shared-record-rename-run");
}
#[test]
fn migrations_rewrite_rows_or_leave_the_table_unchanged() {
    execute_in_std("STORE-9-migrate-run");
}
#[test]
fn dropping_removes_rows_and_history_only() {
    execute("STORE-5-drop-run");
}
#[test]
fn a_crash_mid_migration_leaves_the_old_or_the_new_table() {
    execute_in_std("STORE-9-migrate-crash-run");
}

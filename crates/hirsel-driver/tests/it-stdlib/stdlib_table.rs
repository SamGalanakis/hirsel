//! Typed-table end-to-end scenarios exceed the corpus's 1M-step bound with
//! the real durable store/checksum implementation. Keep them bounded here.
//! Each runs on the interpreter and on native, but for the generation wrap,
//! over the memory authorities `main` and `side`. The crash and cancel sweeps
//! run on the interpreter only: they cut a simulated log at every request, or
//! cancel at every seam call, which only the simulation does.
//! The reader pin limit runs here too: each of its 1025 acquires writes the
//! whole readers table, far past the corpus's step bound.
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

const GRANT: &str = r#"{"main":{"provider":"authority.memory@1","floor":"memory"},"side":{"provider":"authority.memory@1","floor":"memory"}}"#;

fn run(name: &str, path: &Path, native: bool, steps: u64) {
    let simulation = SimulationConfig {
        authorities: hirsel_run_config::validate_authorities(GRANT.as_bytes()).unwrap(),
        ..SimulationConfig::default()
    };
    let (outcome, output) = interpret(path, None, steps, simulation);
    assert!(
        matches!(
            outcome,
            Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
        ),
        "{name}: {outcome:?} {output}"
    );
    if native {
        run_native(name, path);
    }
}

// Runs the fixture, or `source` in its place, on the interpreter.
fn interpret(
    path: &Path,
    source: Option<&str>,
    steps: u64,
    simulation: SimulationConfig,
) -> (Result<RunOutcome, RuntimeError>, String) {
    with_compiler_stack(|| {
        let mut loaded = load_program_source(path, source).unwrap();
        loaded.check_for_execution().unwrap();
        loaded.engine.adapters.interpreter = Some(Box::new(InterpreterAdapter {
            limits: hirsel_interp::Limits {
                steps: Some(steps),
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
            simulation,
        )
        .unwrap();
        let output = run
            .observation
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.output).into_owned())
            .unwrap_or_default();
        (run.outcome, output)
    })
}

// Runs the fixture natively only.
fn run_native(name: &str, path: &Path) {
    with_compiler_stack(|| {
        let grants = format!(
            r#"{{"args":[],"io":{{"dirs":[],"stdout":"stdout.bytes@1","tasks":"tasks.cooperative@1","authorities":{GRANT}}}}}"#
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = native::run_path_native_backend_with_validation(
            path,
            HostLaunch::Live {
                config: LaunchConfig::Grants {
                    input: std::ffi::OsStr::new(&grants),
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
            "{name} native: {outcome:?} {}{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    });
}

fn execute(name: &str) {
    run(name, &fixture(name), true, 16_000_000);
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
    execute("STORE-9-migrate-run");
}
#[test]
fn dropping_removes_rows_and_history_only() {
    execute("STORE-5-drop-run");
}
#[test]
fn a_generation_number_is_reused_only_once_its_rows_are_gone() {
    // The wrap takes 256 migrations, too many for the interpreter's step
    // budget here, so it runs natively only.
    let name = "STORE-9-generation-reuse-run";
    run_native(name, &fixture(name));
}

// Sweeps one cut kind over every request of the table history: the run grants
// `{kind}1..` over a simulated log, each with `faults` at its request.
fn table_cuts(kind: &str, faults: &[&str]) {
    let grants: Vec<String> = (1..=120)
        .map(|n| {
            let at: Vec<String> = faults.iter().map(|f| format!(r#""{f}":{n}"#)).collect();
            format!(
                r#""{kind}{n}":{{"provider":"authority.sim@1","floor":"disk","seed":1,"represent":"log","levels":["disk"],"faults":{{{}}}}}"#,
                at.join(",")
            )
        })
        .collect();
    let simulation = SimulationConfig {
        authorities: hirsel_run_config::validate_authorities(
            format!("{{{}}}", grants.join(",")).as_bytes(),
        )
        .unwrap(),
        ..SimulationConfig::default()
    };
    let (outcome, output) = interpret(
        &fixture("STORE-9-table-cuts-run"),
        None,
        2_000_000_000,
        simulation,
    );
    assert!(
        matches!(
            outcome,
            Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
        ),
        "{kind}: {outcome:?} {output}"
    );
    assert_eq!(output, "tables: 0\n", "{kind}");
}

#[test]
fn a_table_refused_at_any_request_is_whole_after_reopen() {
    table_cuts("f", &["fail_at", "crash_at"]);
}
#[test]
fn a_table_whose_answer_is_lost_at_any_request_is_whole_after_reopen() {
    table_cuts("l", &["lose_response_at", "crash_at"]);
}
#[test]
fn a_table_torn_at_any_request_is_whole_after_reopen() {
    table_cuts("t", &["tear_at"]);
}
#[test]
fn a_table_crashed_after_any_request_is_whole_after_reopen() {
    table_cuts("c", &["crash_at"]);
}

// Runs the cancel fixture with `batch` rows to a migration batch, its writer
// canceled just after seam call `k`; None once the cancel lands past the
// writer, on the run's own output.
fn migrate_canceled(batch: u64, k: Option<u64>) -> Option<String> {
    let path = fixture("STORE-9-migrate-cancel-run");
    let source = std::fs::read_to_string(&path).unwrap().replace(
        "const BATCH: u64 = 2",
        &format!("const BATCH: u64 = {batch}"),
    );
    let simulation = SimulationConfig {
        authorities: hirsel_run_config::validate_authorities(
            br#"{"main":{"provider":"authority.memory@1","floor":"memory"}}"#,
        )
        .unwrap(),
        debug: hirsel_driver::testing::SimulationDebug {
            cancel_at_seam: k,
            ..Default::default()
        },
        ..SimulationConfig::default()
    };
    let (outcome, output) = interpret(&path, Some(&source), 200_000_000, simulation);
    match outcome {
        Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok)) => Some(output),
        Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Error)) => None,
        other => panic!("batch {batch}, cancel at {k:?}: {other:?} {output}"),
    }
}

// The cancels of a migration at every seam call of its writer: each leaves the
// table whole and migrated or whole and seeded, with nothing left over, and a
// later migration reuses the discarded generation. Returns how many canceled.
fn cancel_sweep(batch: u64) -> u64 {
    const OK: &str = "ok: form 2 garbage 0 rows 8\n";
    const CANCELED: &str =
        "canceled: form 1 garbage 0 rows 8, migrated again true form true garbage 0 rows 8\n";
    assert_eq!(migrate_canceled(batch, None).as_deref(), Some(OK));
    let mut canceled = 0;
    for k in 1u64.. {
        assert!(k < 1000, "no cancel reached the run's own output");
        let Some(line) = migrate_canceled(batch, Some(k)) else {
            break;
        };
        if line == CANCELED {
            canceled += 1;
        } else {
            assert_eq!(line, OK, "batch {batch}, cancel at seam call {k}");
        }
    }
    canceled
}

#[test]
fn a_migration_canceled_at_any_seam_call_leaves_the_table_whole() {
    // With one batch a cancel lands only before it; with four, also after each
    // of the first three, so cancels after a completed non-empty batch happen.
    let whole = cancel_sweep(8);
    let batched = cancel_sweep(2);
    assert!(
        batched > whole,
        "no cancel landed after a completed batch: {batched} with batches of 2, {whole} with one batch"
    );
}

#[test]
fn the_reader_pin_after_the_limit_is_refused() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reader_pins/STORE-8-reader-pin-limit-run.hir");
    run("STORE-8-reader-pin-limit-run", &path, false, 2_000_000_000);
}

#[test]
fn the_named_pin_after_the_limit_is_refused() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reader_pins/STORE-8-named-pin-limit-run.hir");
    run("STORE-8-named-pin-limit-run", &path, false, 2_000_000_000);
}

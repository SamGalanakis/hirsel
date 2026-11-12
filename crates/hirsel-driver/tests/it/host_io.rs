use crate::source_std;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;

fn execute(source: &str, simulation: SimulationConfig) -> hirsel_engine::ObservedExecution {
    execute_returning(source, simulation, true).unwrap()
}

/// `execute`, except that when `must_return` is false a run whose `main` returns an
/// error yields `None` instead of failing the test.
fn execute_returning(
    source: &str,
    simulation: SimulationConfig,
    must_return: bool,
) -> Option<hirsel_engine::ObservedExecution> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/std_source/main.hir");
    let mut loaded = source_std::load_program_source(&path, Some(source)).unwrap();
    loaded.check_for_execution().unwrap();
    let mut engine = loaded.engine;
    let context = default_context();
    let run = RunConfiguration {
        entry: entry_path(&context),
        hash_seed: 0,
        inputs: vec![],
    };
    let result = run_observed_snapshot(&mut engine, &run, simulation.clone()).unwrap();
    if !must_return
        && matches!(
            result.outcome,
            Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Error))
        )
    {
        return None;
    }
    assert!(
        matches!(
            result.outcome,
            Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
        ),
        "{:?}",
        result.outcome
    );
    let events = result
        .observation
        .as_ref()
        .unwrap()
        .effects
        .iter()
        .filter_map(|e| match &e.effect {
            hirsel_interp::observation::Effect::CapabilityConsumed(input) => Some(input.clone()),
            _ => None,
        })
        .collect();
    let mut replay_input = simulation;
    replay_input.stdin.clear(); // Recorded chunks are the only stdin provider on replay.
    let replay = run_observed_snapshot(
        &mut engine,
        &RunConfiguration {
            inputs: events,
            ..run
        },
        replay_input,
    )
    .unwrap();
    assert!(
        matches!(
            replay.outcome,
            Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
        ),
        "{:?}",
        replay.outcome
    );
    assert_eq!(result.observation, replay.observation);
    Some(result)
}

#[test]
fn rust_read_until_vectors_preserve_newlines_unterminated_lines_and_eof() {
    with_compiler_stack(|| {
        for (stdin, args) in [
            ("", vec![]),
            ("12", vec!["12"]),
            ("12\n\n", vec!["12\n", "\n"]),
            ("λ\r\nlast", vec!["λ\r\n", "last"]),
        ] {
            execute(
                include_str!("../../../../conformance/CAP/CAP-1-stdin-lines-run.hir"),
                SimulationConfig {
                    stdin: stdin.as_bytes().to_vec(),
                    args: args.into_iter().map(str::to_owned).collect(),
                    ..Default::default()
                },
            );
        }
    });
}

#[test]
fn environment_names_distinguish_denial_unset_and_empty_with_replay() {
    with_compiler_stack(|| {
        execute(
            include_str!("../../../../conformance/CAP/CAP-1-env-names-run.hir"),
            SimulationConfig {
                args: vec!["check".into()],
                env: [
                    ("SET".into(), Some("λ🐑".into())),
                    ("UNSET".into(), None),
                    ("EMPTY".into(), Some("".into())),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
    });
}

#[test]
fn stderr_and_stdout_remain_separate_in_semantic_observations() {
    with_compiler_stack(|| {
        let result = execute(
            include_str!("../../../../conformance/CAP/CAP-1-stderr-stream-run.hir"),
            SimulationConfig::default(),
        );
        let observation = result.observation.unwrap();
        assert_eq!(observation.output, b"stdout\n");
        assert_eq!(observation.final_state.unwrap().stderr, b"stderr\n");
    });
}

#[test]
fn invalid_utf8_line_is_consumed_without_replacement_text() {
    with_compiler_stack(|| {
        execute(
            include_str!("../../../../conformance/CAP/CAP-1-stdin-invalid-line-run.hir"),
            SimulationConfig {
                stdin: vec![255, 10],
                args: vec!["invalid".into()],
                ..Default::default()
            },
        );
    });
}

// Extra simulator points must suspend once without duplicating a store, root
// release or charge; neither these points nor race hints enter observations.

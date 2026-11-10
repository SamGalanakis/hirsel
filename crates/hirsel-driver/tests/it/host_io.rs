use crate::source_std;
use hirsel_driver::*;
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
    let mut engine = source_std::engine();
    let file = engine.sources.add("host-io.hir", source.into()).unwrap();
    let context = default_context();
    let checked = source_check_request(&mut engine, file, &context).unwrap();
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
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
#[test]
fn optional_simulator_points_fire_once_and_diagnostics_preserve_observations() {
    use hirsel_interp::library::simulation::{SchedulePoint, SimulationDebug};
    with_compiler_stack(|| {
        let source = r#"
fn release(take root: Shared[String]) {}
pub fn main(rt: Io) -> Result[(), GrantError] {
  let tasks = io.tasks(rt)?
  let counter: Atomic[u64] = atomic.create(tasks, 0u64)
  atomic.store(tasks, counter, 1u64)
  let built: String = "root"
  let root = shared.freeze(take built)
  release(take root)
  Ok(())
}
"#;
        let mut engine = source_std::engine();
        let file = engine.sources.add("sim-points.hir", source.into()).unwrap();
        let context = default_context();
        let checked = source_check_request(&mut engine, file, &context).unwrap();
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let entry = entry_path(&context);
        let signature = engine.item_signature(&entry).unwrap().value;
        let runtime = engine.runtime_types().unwrap();
        let sources = engine.sources.clone();
        let config = RunConfiguration {
            entry,
            hash_seed: 0,
            inputs: vec![],
        };
        let mut run = |debug| {
            hirsel_interp::library::launch_with_runtime(
                &mut engine,
                &sources,
                &signature,
                &config,
                SimulationConfig {
                    debug,
                    ..Default::default()
                },
                hirsel_interp::Limits::default(),
                &runtime,
            )
            .unwrap()
        };
        let ordinary = run(SimulationDebug::default());
        let debug = SimulationDebug {
            races: true,
            root_releases: true,
            atomic_stores: true,
            ..SimulationDebug::default()
        };
        let explored = run(debug);
        let points: Vec<_> = explored
            .execution
            .trace
            .iter()
            .filter_map(|event| match event {
                hirsel_interp::TraceEvent::SimulationPoint(point) => Some(*point),
                _ => None,
            })
            .collect();
        assert_eq!(
            points,
            [SchedulePoint::AtomicStore, SchedulePoint::RootRelease]
        );
        assert!(!ordinary.execution.trace.iter().any(|e| matches!(
            e,
            hirsel_interp::TraceEvent::SimulationPoint(_)
                | hirsel_interp::TraceEvent::RaceCandidate(_)
        )));
        assert_eq!(ordinary.execution.steps, explored.execution.steps);
        assert_eq!(
            ordinary.observation(&sources),
            explored.observation(&sources)
        );
        assert_eq!(
            explored
                .events
                .iter()
                .filter(|event| event.operation == "atomic.store")
                .count(),
            1
        );
    });
}

/// The cancellation sweep over a store writer (STORE-2, STORE-3, STORE-11, CONC-3): a
/// cancel delivered at any seam call, during the open, a commit or the close, is
/// observed only at the writer's next cancellation point, between its transactions,
/// so no seam call is a cancellation point and every transaction whose outcome the
/// writer received is in the store a later open recovers, and nothing else is. The
/// writer opens the store, runs up to four transactions, checking for cancellation
/// before each, and closes it; a later head reopens the store and counts the rows.
#[test]
fn a_cancel_at_any_seam_call_is_observed_between_transactions() {
    use hirsel_interp::library::simulation::SimulationDebug;
    const SOURCE: &str = r#"
pub type Failed = Failed()

fn step(mut tx: store.Tx, info: store.CallInfo, j: u64) -> Result[(), store.TxError] {
  store.tx_put(mut tx, copy string.as_bytes(f"k/{j}"), b"v")
}

fn writer(s: Scope, a: Authority) -> (u64, bool, bool) {
  let Ok(opened) = store.open(a, store.default_config()) else {
    return (0, false, false)
  }
  let mut db = take opened
  let mut acked: u64 = 0
  let mut canceled = false
  while acked < 4 && !canceled {
    if let Error(_) = task.check_canceled(s) {
      canceled = true
    } else {
      let done: Result[(), store.TxFailure[store.TxError]] = store.transact(mut db, step, acked)
      if done != Ok(()) {
        let _ = store.close(take db)
        return (acked, canceled, false)
      }
      acked = acked + 1
    }
  }
  if !canceled {
    if let Error(_) = task.check_canceled(s) {
      canceled = true
    }
  }
  let closed = store.close(take db) == Ok(())
  (acked, canceled, closed)
}

fn say(stdout: Stdout, line: String) -> Result[(), Failed] {
  match out.print(stdout, f"{line}\n") {
    Ok(_) => Ok(()),
    Error(_) => Error(Failed()),
  }
}

pub fn main(rt: Io) -> Result[(), Failed] {
  let Ok(stdout) = io.stdout(rt) else {
    return Error(Failed())
  }
  let Ok(tasks) = io.tasks(rt) else {
    return say(stdout, "no tasks")
  }
  let Ok(a) = io.authority(rt, "main") else {
    return say(stdout, "no authority")
  }
  let (acked, canceled, closed) = task.scope(
    tasks,
    fn(s: Scope) -> (u64, bool, bool) {
      let child = task.async(
        s,
        fn() -> (u64, bool, bool) {
          task.scope(tasks, fn(inner: Scope) -> (u64, bool, bool) { writer(inner, a) })
        }
      )
      task.await(s, take child)
    }
  )
  let Ok(reopened) = store.open(a, store.default_config()) else {
    return say(stdout, f"acked {acked} canceled {canceled} closed {closed}, reopen refused")
  }
  let mut db = take reopened
  let Ok(snap) = store.snapshot(a, mut db, authority.floor(a)) else {
    return say(stdout, f"acked {acked} canceled {canceled} closed {closed}, snapshot refused")
  }
  let (from, to) = store.prefix_bounds(b"k/")
  let held = list.len(store.range(snap, from, to, Ascending(), 100))
  let _ = store.close(take db)
  say(stdout, f"acked {acked} canceled {canceled} closed {closed} held {held}")
}
"#;
    with_compiler_stack(|| {
        let grant: hirsel_run_schema::AuthorityGrant =
            serde_json::from_str(r#"{"provider":"authority.memory@1","floor":"memory"}"#).unwrap();
        let run = |cancel_at_seam| {
            let result = execute_returning(
                SOURCE,
                SimulationConfig {
                    authorities: [("main".to_string(), grant.clone())].into(),
                    debug: SimulationDebug {
                        cancel_at_seam,
                        ..SimulationDebug::default()
                    },
                    ..Default::default()
                },
                false,
            )?;
            Some(String::from_utf8(result.observation.unwrap().output).unwrap())
        };
        assert_eq!(
            run(None).as_deref(),
            Some("acked 4 canceled false closed true held 4\n")
        );
        // Every seam call of the writer's run, up to the first one past it: a cancel
        // there lands on the root task's reopen, which cancels the run's own output,
        // so `main` returns an error and the sweep is over.
        let mut last = 0;
        for k in 1u64.. {
            assert!(k < 1000, "no cancel reached the root task's reopen");
            let Some(line) = run(Some(k)) else {
                assert!(k > 1, "the writer made no seam call");
                break;
            };
            let words: Vec<&str> = line.split_whitespace().collect();
            let acked: u64 = words[1].parse().unwrap();
            // Only a cancel stops the writer early, and it never costs an
            // acknowledged transaction or adds an unacknowledged one.
            let canceled = acked < 4 || words[3] == "true";
            assert_eq!(
                line,
                format!("acked {acked} canceled {canceled} closed true held {acked}\n"),
                "cancel at seam call {k}"
            );
            assert!(acked >= last, "cancel at seam call {k}: {line}");
            last = acked;
        }
        assert_eq!(last, 4, "no cancel landed after the last transaction");
    });
}

use crate::source_std;
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;

fn execute(source: &str, simulation: SimulationConfig) -> hirsel_engine::ObservedExecution {
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
    result
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

/// The cancellation sweep over a writer's seam calls (STORE-3, STORE-11, CONC-3): a
/// cancel delivered at any seam call is observed only at the writer's next
/// cancellation point, between its steps, so the calls of the step it arrived in
/// complete and every step the writer acknowledged is in the history a later read
/// returns. The writer is one claim, then four steps of an advance and a durable
/// (calls 2j and 2j + 1 for step j), as a commit is; delivered at call k it
/// acknowledges k / 2 steps (the durable after a cancelled advance still answers),
/// and the history holds its claim and those. The storage cutover writes each step
/// as a store transaction.
#[test]
fn a_cancel_at_any_seam_call_is_observed_between_advances() {
    use hirsel_interp::library::simulation::SimulationDebug;
    const SOURCE: &str = r#"
pub type Failed = Failed()

fn writer(s: Scope, a: Authority) -> (u64, bool) {
  let floor = authority.floor(a)
  let Ok(made) = authority.claim(a) else {
    return (0, false)
  }
  let mut c = take made
  let mut acked: u64 = 0
  while acked < 4 {
    let Ok(()) = task.check_canceled(s) else {
      return (acked, true)
    }
    let from = claim.end(c)
    let Advanced(_) = authority.advance(a, mut c, from, b"entry", floor) else {
      return (acked, false)
    }
    let Ok(_) = authority.durable(a, mut c, floor) else {
      return (acked, false)
    }
    acked = acked + 1
  }
  let Ok(()) = task.check_canceled(s) else {
    return (acked, true)
  }
  (acked, false)
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
  let (acked, canceled) = task.scope(
    tasks,
    fn(s: Scope) -> (u64, bool) {
      let child = task.async(
        s,
        fn() -> (u64, bool) {
          task.scope(tasks, fn(inner: Scope) -> (u64, bool) { writer(inner, a) })
        }
      )
      task.await(s, take child)
    }
  )
  let Ok(all) = authority.read(a, 0, authority.floor(a), 100) else {
    return say(stdout, f"acked {acked} canceled {canceled}, recovery read refused")
  }
  let mut held: u64 = 0
  for _ in all {
    held = held + 1
  }
  say(stdout, f"acked {acked} canceled {canceled} held {held}")
}
"#;
    with_compiler_stack(|| {
        let grant: hirsel_run_schema::AuthorityGrant =
            serde_json::from_str(r#"{"provider":"authority.memory@1","floor":"memory"}"#).unwrap();
        let run = |cancel_at_seam| {
            let result = execute(
                SOURCE,
                SimulationConfig {
                    authorities: [("main".to_string(), grant.clone())].into(),
                    debug: SimulationDebug {
                        cancel_at_seam,
                        ..SimulationDebug::default()
                    },
                    ..Default::default()
                },
            );
            String::from_utf8(result.observation.unwrap().output).unwrap()
        };
        // The writer's claim is 1 entry, then 1 per advance. (Its entries are opaque,
        // so the reader reads without claiming: a claim validates the chain.)
        assert_eq!(run(None), "acked 4 canceled false held 5\n");
        for k in 1..=9u64 {
            assert_eq!(
                run(Some(k)),
                format!("acked {} canceled true held {}\n", k / 2, k / 2 + 1),
                "cancel at seam call {k}"
            );
        }
    });
}

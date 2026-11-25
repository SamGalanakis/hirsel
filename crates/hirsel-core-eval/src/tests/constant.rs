//! A constant's budget (MEM-34) on the pure core.
use super::*;

/// Bug: a constant's run is charged otherwise than MEM-34 says, or a root
/// refusal reaches the run as a failure it can catch: the entry takes no step, a
/// step or a `bound steps` reservation past the root bound, or an admission the
/// root limit refuses, fails instead of stopping the run, or a bound the run made
/// itself stops it. Or the engines disagree on where it stopped. `run constant
/// [steps N] [limit N]` lines are derived by hand beside their source; every
/// engine, with the test-mode checks off and on, must give each one.
pub(super) fn run(
    at: &str,
    m: &Module,
    f: FuncId,
    args: &[u64],
    head: &str,
    expected: &str,
    checks: &[&str],
) {
    let mut input = eval::PureInput::constant();
    let mut words = head.split_whitespace();
    while let Some(word) = words.next() {
        let n: u64 = words.next().unwrap().parse().unwrap();
        match word {
            "steps" => input.steps = Some(n),
            "limit" => input.limit = n,
            other => panic!("{at}: `{other}` in a constant run"),
        }
    }
    let sig = &m.signatures[m.functions[f.index()].signature.index()];
    let want = match expected.trim().strip_prefix("stopped") {
        Some(kind) => eval::Outcome::Stopped(kind.trim().to_owned().leak()),
        None => outcome(&sig.results, expected),
    };
    let calls = checks.iter().find_map(|c| c.trim().strip_prefix("calls"));
    let prepared = eval::PureModule::new(m);
    for engine in [eval::Engine::Tree, eval::Engine::Flat] {
        for checks in [false, true] {
            let got = eval::run_pure(
                &prepared,
                f,
                args,
                eval::PureInput {
                    checks,
                    engine,
                    ..input
                },
            );
            let what = format!("{at}: {engine:?} (checks {checks})");
            assert_eq!(got.outcome, want, "{what}");
            if let Some(calls) = calls {
                let calls: Vec<FuncId> = calls
                    .split(',')
                    .map(|c| FuncId(c.trim().trim_start_matches('f').parse().unwrap()))
                    .collect();
                assert_eq!(got.calls, calls, "{what}: calls");
            }
        }
    }
}

/// Bug: a pure run treats `net.class` as a world operation and stops, though
/// LANG-20 lets an initialiser call it; or a closed-list function's mark is
/// ignored in a pure run, so the initialiser goes on, or stops a world run.
/// 127.0.0.1 is Loopback, the sixth class of NET-18's table (index 5);
/// `rt.closed_11` marks `unique.new` (`CLOSED`).
#[test]
fn only_a_pure_run_stops_at_a_closed_function() {
    let m = load(
        "closed",
        "core v1\nsig g0 = () -> (u64)\nsig g1 = (u8, u64, u64) -> (u64, u8, u32, u64)\nsig g2 = () -> ()\ninst n0 = app@1/main/class [] []\ninst n1 = app@1/main/fresh [] []\nfn f0 = n0 g0 {\nb0:\n  v0: u8 = const 0\n  v1: u64 = const 0\n  v2: u64 = const 2130706433\n  v3: u64, v4: u8, v5: u32, v6: u64 = call rt.net_class g1 (v0, v1, v2) fail b1\n  return v3\nb1:\n  propagate\n}\nfn f1 = n1 g0 {\nb0:\n  call rt.closed_11 g2 ()\n  v0: u64 = const 1\n  return v0\n}\n",
    );
    let prepared = eval::PureModule::new(&m);
    for engine in [eval::Engine::Tree, eval::Engine::Flat] {
        let input = eval::PureInput {
            engine,
            ..eval::PureInput::constant()
        };
        let class = eval::run_pure(&prepared, FuncId(0), &[], input);
        assert_eq!(
            class.outcome,
            eval::Outcome::Returned(vec![5]),
            "{engine:?}"
        );
        let fresh = eval::run_pure(&prepared, FuncId(1), &[], input);
        assert_eq!(
            fresh.outcome,
            eval::Outcome::Stopped("Closed"),
            "{engine:?}"
        );
        assert_eq!(fresh.reached, Some("unique.new"), "{engine:?}");
        assert_eq!(fresh.calls, [FuncId(1)], "{engine:?}");
    }
    for (f, want) in [(0, 5), (1, 1)] {
        let world = eval::run_args(&m, FuncId(f), &[], u64::MAX, &[]);
        assert_eq!(world.outcome, eval::Outcome::Returned(vec![want]), "f{f}");
    }
}

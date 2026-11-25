//! The flat engine against its oracle where a run line's world run cannot go.
use super::*;

/// Bug: the flat engine stops on an exhausted executed-instruction budget at
/// another instruction than the tree walker, which stops at the first one past
/// the budget (so a run that returns or fails within it does, and one that does
/// not panics). `run budget N` lines are derived by hand beside their source;
/// every engine, with the test-mode checks off and on, must give each one.
pub(super) fn budget(
    name: &str,
    run: &str,
    m: &Module,
    f: FuncId,
    args: &[u64],
    n: u64,
    want: &str,
) {
    let sig = &m.signatures[m.functions[f.index()].signature.index()];
    let want = (want.trim() != "exhausted").then(|| outcome(&sig.results, want));
    let prepared = eval::PureModule::new(m);
    for engine in [eval::Engine::Tree, eval::Engine::Flat] {
        for checks in [false, true] {
            let input = eval::PureInput {
                instructions: n,
                checks,
                engine,
                ..eval::PureInput::default()
            };
            let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                eval::run_pure(&prepared, f, args, input).outcome
            }))
            .map_err(|p| p.downcast_ref::<&str>().copied());
            let got = match got {
                Ok(outcome) => Some(outcome),
                Err(Some("evaluator instruction budget exhausted")) => None,
                Err(other) => panic!("{name}: {run}: {engine:?} panicked: {other:?}"),
            };
            assert_eq!(got, want, "{name}: {run}: {engine:?} (checks {checks})");
        }
    }
}

/// Bug: the flat engine lowers a body in which a register has two definitions,
/// and a frame's constant template (each constant's register holds it from the
/// frame's start) reads a value the tree walker never would. wf refuses such a
/// body, and so does the text parser; this one is seeded after parsing, as a
/// producer bug would reach the evaluator: the `add` writes `v1`, a constant's
/// register.
#[test]
fn a_register_defined_twice_is_not_lowered() {
    let mut m = load(
        "twice",
        "core v1\nsig g0 = (u64) -> (u64)\ninst n0 = app@1/main/twice [] []\nfn f0 = n0 g0 {\nb0(v0: u64):\n  v1: u64 = const 1\n  v2: u64 = add v0, v1\n  return v2\n}\n",
    );
    let b = m.functions[0].body.as_mut().unwrap();
    let Inst::Binary { result, .. } = &mut b.insts[1] else {
        panic!("the add is the body's second instruction")
    };
    *result = ValueId(1);
    assert!(wf::module(&m).is_err());
    let prepared = eval::PureModule::new(&m);
    let lowered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eval::run_pure(&prepared, FuncId(0), &[1], eval::PureInput::default())
    }))
    .expect_err("a body defining v1 twice is lowered");
    assert_eq!(
        lowered.downcast_ref::<String>().map(String::as_str),
        Some("flat: v1 defined twice")
    );
}

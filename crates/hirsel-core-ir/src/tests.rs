//! Tests over the FORM v0 samples. Every expected value is the hand-derived `run:`
//! line in the sample file, not output of this crate.

use crate::text::{parse, print};
use crate::*;

use crate::eval;

const SAMPLES: &[(&str, &str)] = &[
    ("arith", include_str!("../samples/arith.core")),
    (
        "compare-convert",
        include_str!("../samples/compare-convert.core"),
    ),
    ("control", include_str!("../samples/control.core")),
    ("slots", include_str!("../samples/slots.core")),
    ("calls", include_str!("../samples/calls.core")),
    ("failure", include_str!("../samples/failure.core")),
    ("cleanup", include_str!("../samples/cleanup.core")),
    ("admission", include_str!("../samples/admission.core")),
    ("counted", include_str!("../samples/counted.core")),
    ("custody", include_str!("../samples/custody.core")),
    ("variants", include_str!("../samples/variants.core")),
    ("sequences", include_str!("../samples/sequences.core")),
    ("strings", include_str!("../samples/strings.core")),
];

fn load(name: &str, text: &str) -> Module {
    parse(text).unwrap_or_else(|e| panic!("{name}: line {}: {}", e.line, e.message))
}

/// Bug: the printer drops or renames something the parser reads (a failure target,
/// an anchor, a flag result, an instance type), or the parser reads a field into
/// the wrong table. Law: print is the inverse of parse on canonical text, and
/// parse(print(m)) == m.
#[test]
fn text_round_trips_every_sample() {
    for (name, text) in SAMPLES {
        let m = load(name, text);
        let printed = print(&m);
        let canonical: String = text
            .lines()
            .map(|l| l.split(';').next().unwrap().trim_end())
            .filter(|l| !l.is_empty())
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            printed, canonical,
            "{name}: print(parse(text)) differs from the text"
        );
        assert_eq!(load(name, &printed), m, "{name}: parse(print(m)) != m");
    }
}

/// Bug: a sample (the producers' reference) is malformed: a wrong arity, a value
/// defined twice, a missing failure block.
#[test]
fn every_sample_is_well_formed() {
    for (name, text) in SAMPLES {
        wf::module(&load(name, text)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// Bug: well-formedness accepts a producer mistake an emitter would miscompile.
/// Each case is a one-line edit of a correct body and the rule it breaks.
#[test]
fn well_formedness_rejects_producer_mistakes() {
    let head = "core v1\nsig g0 = (i32, i32) -> (i32)\nsig g1 = (u64) -> (ptr)\nsig g2 = (u64) -> never\ninst n0 = p@0/m/f [] []\ninst n1 = p@0/m/stop [] []\nfn f0 = n0 g0 {\n";
    let tail = "fn f1 = n1 g2 extern\n";
    let ok =
        "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  return v2\nb1:\n  propagate\n}\n";
    wf::module(&parse(&format!("{head}{ok}{tail}")).unwrap())
        .expect("the base case is well formed");
    let cases = [
        // a failure path that returns would catch the failure (FAIL-14)
        (
            "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  return v2\nb1:\n  jump b2\nb2:\n  return v0\n}\n",
            "failure path",
        ),
        // a failure block receives no parameters: the cause is in the chain
        (
            "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  return v2\nb1(v3: i32):\n  propagate\n}\n",
            "parameters",
        ),
        // an engine operation called through a signature other than its row
        (
            "b0(v0: i32, v1: i32):\n  v2: u64 = const 4\n  v3: ptr = call rt.channel_sender g1 (v2)\n  return v0\n}\n",
            "through another signature",
        ),
        // an edge passes the wrong number of arguments
        (
            "b0(v0: i32, v1: i32):\n  jump b1(v0, v1)\nb1(v2: i32):\n  return v2\n}\n",
            "arity",
        ),
        // branching on an integer instead of a bool
        (
            "b0(v0: i32, v1: i32):\n  branch v0, b1, b1\nb1:\n  return v0\n}\n",
            "non-bool",
        ),
        // returning a different type than the signature
        (
            "b0(v0: i32, v1: i32):\n  v2: bool = lt v0, v1\n  return v2\n}\n",
            "return",
        ),
        // entry parameters must be the signature's
        ("b0(v0: i32):\n  return v0\n}\n", "entry"),
        // a call that returns Never followed by anything but `unreachable` (Q21)
        (
            "b0(v0: i32, v1: i32):\n  v2: u64 = const 1\n  call f1 g2 (v2)\n  propagate\n}\n",
            "never call",
        ),
        // mixed widths (SEM-1) never reach CoreIR
        (
            "b0(v0: i32, v1: i32):\n  v2: i64 = widen v0\n  v3: i64 = add v2, v1 fail b1\n  return v0\nb1:\n  return v0\n}\n",
            "binary operand types",
        ),
    ];
    for (body, why) in cases {
        let m = parse(&format!("{head}{body}{tail}")).unwrap();
        let err = wf::module(&m).expect_err(why);
        assert!(err.contains(why), "expected `{why}`, got `{err}`");
    }
    // v0 names a failure block on every fallible instruction: a v0 reader must not
    // take a propagating `add` as one that cannot fail.
    let v0 = format!("{head}b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1\n  return v2\n}}\n")
        .replace("core v1", "core v0");
    assert!(parse(&v0).unwrap_err().message.contains("under `core v0`"));
    let twice =
        "b0(v0: i32, v1: i32):\n  v1: i32 = add v0, v0 fail b1\n  return v1\nb1:\n  propagate\n}\n";
    assert!(
        parse(&format!("{head}{twice}"))
            .unwrap_err()
            .message
            .contains("defined twice")
    );
}

/// Bug: a constant silently truncated to its type (300 read as 44 in a u8).
#[test]
fn constants_out_of_range_are_refused() {
    let m = |c: &str| {
        let t = format!(
            "core v0\nsig g0 = () -> (u8)\ninst n0 = p@0/m/f [] []\nfn f0 = n0 g0 {{\nb0:\n  v0: u8 = const {c}\n  return v0\n}}\n"
        );
        parse(&t)
    };
    assert!(m("255").is_ok());
    assert!(m("256").is_err());
    assert!(m("-1").is_err());
}

/// Bug: a sample's lowering, or this crate's statement of an op's meaning, differs
/// from the hand-derived behaviour written beside the source.
#[test]
fn samples_behave_as_derived_by_hand() {
    for (name, text) in SAMPLES {
        let mut runs = 0;
        let m = load(name, text);
        for line in text.lines() {
            // `; run [limit N]: fK(args) -> results | used N | trace a, b`, or
            // `-> fail Cause, ...`; `used` and `trace` are checked when written.
            let Some(run) = line.strip_prefix("; run") else {
                continue;
            };
            let (head, run) = run.split_once(':').unwrap();
            let limit = match head.trim().strip_prefix("limit") {
                Some(n) => n.trim().parse().unwrap(),
                None => u64::MAX,
            };
            let run = run.split(';').next().unwrap().trim();
            let (call, expected) = run.split_once("->").unwrap();
            let mut parts = expected.split('|');
            let expected = parts.next().unwrap();
            let (f, args) = call.trim().trim_end_matches(')').split_once('(').unwrap();
            let f = FuncId(f.trim_start_matches('f').parse().unwrap());
            let sig = &m.signatures[m.functions[f.index()].signature.index()];
            let args: Vec<u64> = args
                .split(',')
                .filter(|a| !a.trim().is_empty())
                .zip(&sig.params)
                .map(|(a, p)| match p {
                    Param::Value(t) => eval::scalar(*t, a.trim()),
                    Param::Address(_) => panic!("by-address arguments are FORM v1"),
                })
                .collect();
            let got = eval::run(&m, f, &args, limit);
            let want = match expected.trim() {
                e if e.starts_with("fail") => eval::Outcome::Failed(
                    e["fail".len()..]
                        .split(',')
                        .map(|c| c.trim().to_owned())
                        .filter(|c| !c.is_empty())
                        .collect(),
                ),
                e => eval::Outcome::Returned(
                    e.split(',')
                        .zip(&sig.results)
                        .map(|(v, t)| eval::scalar(*t, v.trim()))
                        .collect(),
                ),
            };
            assert_eq!(got.outcome, want, "{name}: {run}");
            for part in parts {
                let (key, value) = part.trim().split_once(' ').unwrap();
                match key {
                    "used" => assert_eq!(
                        got.used,
                        value.parse::<u64>().unwrap(),
                        "{name}: {run}: used"
                    ),
                    "trace" => {
                        let want: Vec<u64> = value
                            .split(',')
                            .map(|v| v.trim().parse().unwrap())
                            .collect();
                        assert_eq!(got.trace, want, "{name}: {run}: trace");
                    }
                    k => panic!("unknown run check `{k}`"),
                }
            }
            runs += 1;
        }
        assert!(runs > 0, "{name} has no run lines");
    }
}

/// Bug: two instances of one generic share a link name (types or dictionaries left
/// out of the digest, or the boundary between them not encoded).
#[test]
fn instance_identity_separates_types_and_dictionaries() {
    let path = hirsel_core_ids::DefPath::from_full("p@0/m/f").unwrap();
    let key = |types: &[&str], dicts: &[&str]| InstanceKey {
        definition: path.clone(),
        types: types.iter().map(|s| s.to_string()).collect(),
        dictionaries: dicts.iter().map(|s| s.to_string()).collect(),
    };
    let keys = [
        key(&["u32"], &[]),
        key(&["u64"], &[]),
        key(&["u32", "eq"], &[]),
        key(&["u32"], &["eq"]),
        key(&[], &["u32", "eq"]),
    ];
    for (i, a) in keys.iter().enumerate() {
        for b in &keys[i + 1..] {
            assert_ne!(a.digest(), b.digest(), "{a:?} and {b:?} collide");
        }
    }
}

/// Bug: a core crate links old semantic code (LIR, TIR, the old request sum,
/// TypeInterner), so the old and new pipelines stop being separable at M3. Law:
/// the normal-dependency closure of every `hirsel-core-*` crate stays inside the
/// neutral crates below both pipelines.
#[test]
fn coreir_data_crates_depend_only_on_neutral_crates() {
    const NEUTRAL: &[&str] = &[
        "hirsel-core-ids",
        "hirsel-core-layout",
        "hirsel-core-ir",
        "hirsel-intern",
        "hirsel-guard",
    ];
    let deps = |name: &str| -> Vec<String> {
        // Embed the manifest inputs so the law also runs in a hermetic test sandbox.
        let manifest = match name {
            "hirsel-core-ids" => include_str!("../../hirsel-core-ids/Cargo.toml"),
            "hirsel-core-layout" => include_str!("../../hirsel-core-layout/Cargo.toml"),
            "hirsel-core-ir" => include_str!("../Cargo.toml"),
            "hirsel-intern" => include_str!("../../hirsel-intern/Cargo.toml"),
            "hirsel-guard" => include_str!("../../hirsel-guard/Cargo.toml"),
            _ => panic!("non-neutral data dependency {name}"),
        };
        let mut in_deps = false;
        let mut out = Vec::new();
        for line in manifest.lines() {
            if line.starts_with('[') {
                in_deps = line.trim() == "[dependencies]";
            } else if in_deps && line.starts_with("hirsel-") {
                out.push(line.split([' ', '=']).next().unwrap().to_owned());
            }
        }
        out
    };
    // The CoreIR data crates (decision 14); front-end crates such as
    // `hirsel-core-syntax` may depend on them, never the other way round.
    let roots: Vec<String> = ["hirsel-core-ids", "hirsel-core-layout", "hirsel-core-ir"]
        .map(String::from)
        .to_vec();
    let mut stack = roots.clone();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(c) = stack.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        for d in deps(&c) {
            assert!(
                NEUTRAL.contains(&d.as_str()),
                "{c} depends on {d}, outside the neutral crates"
            );
            stack.push(d);
        }
    }
}

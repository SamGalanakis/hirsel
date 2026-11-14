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
    ("providers", include_str!("../samples/providers.core")),
    ("stable-keys", include_str!("../samples/stable-keys.core")),
    ("scalar-rows", include_str!("../samples/scalar-rows.core")),
    ("kernel", include_str!("../samples/kernel.core")),
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
        // an alloc's kind and place disagree (Cleanup classes): ordinary storage named
        // stable gets no prefix or cleanup, region storage named for an edge gets a
        // prefix and an edge's cleanup the region never runs
        (
            "b0(v0: i32, v1: i32):\n  v2: u64 = const 8\n  v3: ptr, v4: u64 = alloc stable v2 align 8 charge v2\n  free v3 charge v2\n  return v0\n}\n",
            "stable storage",
        ),
        (
            "b0(v0: i32, v1: i32):\n  v2: ptr = region_open\n  v3: u64 = const 8\n  v4: ptr, v5: u64, v6: u64 = alloc for seq u8 v3 align 8 charge v3 in v2\n  region_close v2\n  return v0\n}\n",
            "stable storage",
        ),
        // an alloc names an edge no block is owned through, so no class derives
        (
            "b0(v0: i32, v1: i32):\n  v2: u64 = const 8\n  v3: ptr, v4: u64 = alloc for u64 v2 align 8 charge v2\n  free v3 charge v2\n  return v0\n}\n",
            "not an own block's edge",
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
            // `; run [limit N] [args a b]: fK(args) -> results | used N | trace a, b
            // | out text | err text`, or `-> fail Cause, ...`; each check after the
            // results is made when written (`\n` in a text is a newline).
            let Some(run) = line.strip_prefix("; run") else {
                continue;
            };
            let (head, run) = run.split_once(':').unwrap();
            let (head, argv) = match head.split_once("args") {
                Some((h, a)) => (h, a.split_whitespace().collect()),
                None => (head, Vec::new()),
            };
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
                    Param::Address { .. } => panic!("by-address arguments are FORM v1"),
                })
                .collect();
            let got = eval::run_args(&m, f, &args, limit, &argv);
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
                    "out" | "err" => {
                        let want = value.replace("\\n", "\n");
                        let got = if key == "out" {
                            &got.stdout
                        } else {
                            &got.stderr
                        };
                        assert_eq!(String::from_utf8_lossy(got), want, "{name}: {run}: {key}");
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

/// Bug: a cause inside core reports core's own text (or none) instead of the user
/// call that reached it, or a `raise` reports the anchor of the instruction before
/// it instead of its block's own. Law (SITES.md): the site is the raising
/// operation's anchor, unless that is core's or missing, then the anchor of the
/// nearest enclosing non-core activation; a raise uses its block's anchor; with no
/// such activation the site is none. Expected anchors are read off the text below.
#[test]
fn failure_sites_follow_the_contract() {
    let m = load(
        "sites",
        "core v1\nitem i0 = p@0/m/user\nitem i1 = core@1/m/k\nsig g0 = (u64) -> (u64)\ninst n0 = core@1/m/k [] []\ninst n1 = p@0/m/user [] []\ninst n2 = p@0/m/check [] []\nfn f0 = n0 g0 {\nb0(v0: u64):\n  v1: u64 = const 1\n  v2: u64 = add v0, v1 at i1 5..10\n  return v2\n}\nfn f1 = n1 g0 {\nb0(v0: u64):\n  v1: u64 = call f0 g0 (v0) at i0 20..30\n  return v1\n}\nfn f2 = n2 g0 {\nb0(v0: u64):\n  v1: u64 = const 0 at i0 50..55\n  v2: bool = eq v0, v1 at i0 60..65\n  branch v2, b1, b2\nb1:\n  v3: u64 = const 1 at i0 70..75\n  raise unwrap at i0 40..45\nb2:\n  return v0\n}\n",
    );
    let at = |start, end| {
        Some(Anchor {
            item: ItemId(0),
            start,
            end,
        })
    };
    let site = |f: u32, x: u64| eval::run(&m, FuncId(f), &[x], u64::MAX).sites;
    assert_eq!(
        site(1, u64::MAX),
        vec![at(20, 30)],
        "core op: the user call"
    );
    assert_eq!(site(0, u64::MAX), vec![None], "core with no user caller");
    assert_eq!(site(2, 0), vec![at(40, 45)], "raise: its block's anchor");
}

#[test]
fn allocation_overflow_refuses_at_maximum_limit() {
    let source = "core v1\nsig g0 = (bool, u64) -> (bool, u64)\ninst n0 = p@1/m/alloc [] []\nfn f0 = n0 g0 {\nb0(v0: bool, v1: u64):\n  v2: u64 = const 8\n  v3: ptr, v4: u64 = alloc for seq u8 v2 align 8 charge v1 overflow v0\n  v5: ptr = const 0x0\n  v6: bool = ne v3, v5\n  branch v6, b1, b2\nb1:\n  free v3 charge v1\n  jump b2\nb2:\n  return v6, v4\n}\n";
    let module = parse(source).unwrap();
    wf::module(&module).unwrap();
    assert_eq!(parse(&print(&module)).unwrap(), module);
    for (args, expected) in [
        ([1, 0], vec![0, u64::MAX]),
        ([1, u64::MAX], vec![0, u64::MAX]),
        ([0, 8], vec![1, 0]),
    ] {
        let report = eval::run(&module, FuncId(0), &args, u64::MAX);
        assert_eq!(report.outcome, eval::Outcome::Returned(expected));
    }
}

/// Bug: the evaluator lets a slot kernel's mistake through (a double take, a put
/// that overwrites a live value, a read of a slot already taken, storage freed with
/// a value still in it), so a kernel that leaks or double-drops passes every run.
/// Each case is a one-line change to a correct body, with the stop it must cause.
#[test]
fn slot_marks_stop_kernel_mistakes() {
    let head = "core v1\nsig g0 = () -> ()\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  addr a0 = v1 : u64\nb0:\n  v0: u64 = const 8\n  v1: ptr, v2: u64 = alloc for seq u64 v0 align 8 charge v0\n  v3: u64 = const 1\n  address a0\n";
    let run = |body: &str| {
        let m = parse(&format!("{head}{body}  free v1 charge v0\n  return\n}}\n")).unwrap();
        wf::module(&m).unwrap();
        std::panic::catch_unwind(|| eval::run(&m, FuncId(0), &[], u64::MAX))
            .err()
            .map(|e| {
                e.downcast::<String>()
                    .map_or_else(|_| String::new(), |s| *s)
            })
    };
    let ok = "  store a0, v0\n  mark a0, v3, live : u64\n  v4: u64 = read a0\n  mark a0, v3, empty : u64\n";
    assert_eq!(run(ok), None, "the base case runs");
    for (body, stop) in [
        (
            "  mark a0, v3, live : u64\n  mark a0, v3, empty : u64\n  mark a0, v3, empty : u64\n",
            "take of the empty slot",
        ),
        (
            "  mark a0, v3, live : u64\n  mark a0, v3, live : u64\n  mark a0, v3, empty : u64\n",
            "put over the live slot",
        ),
        (
            "  mark a0, v3, live : u64\n  mark a0, v3, empty : u64\n  v4: u64 = read a0\n",
            "read of the empty slot",
        ),
        (
            "  mark a0, v3, live : u64\n",
            "free of a block whose slot at +0 is live",
        ),
        // A kernel hands a hole back to ordinary storage as a value.
        (
            "  mark a0, v3, live : u64\n  mark a0, v3, empty : u64\n  unmark a0, v3, live : u64\n",
            "unmark live of the empty slot",
        ),
        // A kernel hands storage back with a value it claims it took.
        (
            "  store a0, v0\n  mark a0, v3, live : u64\n  unmark a0, v3, empty : u64\n",
            "unmark empty of the live slot",
        ),
    ] {
        let got = run(body).unwrap_or_else(|| panic!("`{stop}` ran to its end"));
        assert!(got.contains(stop), "expected `{stop}`, got `{got}`");
    }
    // Bug: `unmark` leaves the shadow behind, so ordinary use of handed-back storage
    // stops: a free of a slot handed back live, a read of one handed back empty and
    // refilled. A slot never marked passes either way.
    for body in [
        "  store a0, v0\n  mark a0, v3, live : u64\n  unmark a0, v3, live : u64\n",
        "  mark a0, v3, live : u64\n  mark a0, v3, empty : u64\n  unmark a0, v3, empty : u64\n  store a0, v0\n  v4: u64 = read a0\n",
        "  store a0, v0\n  unmark a0, v3, live : u64\n  unmark a0, v3, empty : u64\n",
    ] {
        assert_eq!(run(body), None, "{body}");
    }
    // Bug: a body outside core's kernels unmarks a slot.
    let user = head.replace("core@1/m/k", "p@0/m/k");
    let m = parse(&format!(
        "{user}  unmark a0, v3, live : u64\n  free v1 charge v0\n  return\n}}\n"
    ))
    .unwrap();
    let err = wf::module(&m).expect_err("a user body unmarks");
    assert!(err.contains("only core's slot kernels"), "{err}");
    // Bug: zero-size slots share one address, so shadowing them by address calls
    // a put into slot 1 a put over slot 0. They have no storage and no shadow.
    let unit = parse(
        "core v1\ntype t0 = own \"Unit\" { }\nsig g0 = () -> ()\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  addr a0 = v1 index v3 : t0\n  addr a1 = v1 index v4 : t0\nb0:\n  v0: u64 = const 8\n  v1: ptr, v2: u64 = alloc for seq t0 v0 align 8 charge v0\n  v3: u64 = const 0\n  v4: u64 = const 1\n  address a0\n  address a1\n  mark a0, v4, live : t0\n  mark a1, v4, live : t0\n  mark a0, v4, empty : t0\n  mark a1, v4, empty : t0\n  free v1 charge v0\n  return\n}\n",
    )
    .unwrap();
    wf::module(&unit).unwrap();
    eval::run(&unit, FuncId(0), &[], u64::MAX);
    // Bug: a run move within one block lowers to a copy whose ranges overlap, which
    // a native memcpy may garble (FORM #90 refuses it). Bytes 0..16 onto 8..24.
    let overlap = parse(
        "core v1\nsig g0 = () -> ()\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  addr a0 = v1 : u64\n  addr a1 = v1 byte v3 : u64\nb0:\n  v0: u64 = const 24\n  v1: ptr, v2: u64 = alloc for seq u64 v0 align 8 charge v0\n  v3: u64 = const 8\n  v4: u64 = const 16\n  address a0\n  address a1\n  copy a1, a0, v4\n  free v1 charge v0\n  return\n}\n",
    )
    .unwrap();
    wf::module(&overlap).unwrap();
    let stop = std::panic::catch_unwind(|| eval::run(&overlap, FuncId(0), &[], u64::MAX))
        .err()
        .and_then(|e| e.downcast::<String>().ok())
        .expect("an overlapping copy stops the run");
    assert!(stop.contains("overlapping"), "{stop}");
}

/// `grow` replaces a carrier's owned block (crate docs, "Growth"). A list of
/// u64 with length 1 and capacity 2 (16 bytes, element 42) grows to capacity 4
/// (32 bytes); the hand-derived results are the element, then the room.
#[test]
fn a_grow_moves_its_carrier_or_stops() {
    // v13 is the grow's `moved`, v14 its `refund`, v12 its capacity; `{pre}` runs
    // before it and `{post}` after it succeeds.
    let module = |pre: &str, grow: &str, post: &str| {
        format!(
            "core v1\ntype t0 = own \"List\" {{ items: seq u64 }}\nsig g0 = () -> (u64, u64)\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {{\n  slot s0 = t0\n  addr a0 = s0 field 0 byte v1 : ptr\n  addr a1 = s0 field 0 byte v2 : u64\n  addr a2 = s0 field 0 byte v3 : u64\n  addr a3 = s0 : t0\n  addr a4 = v4 : u64\n  addr a5 = v15 : u64\nb0:\n  live s0\n  v0: u64 = const 0\n  v1: u64 = const 0\n  v2: u64 = const 8\n  v3: u64 = const 16\n  address a0\n  address a1\n  address a2\n  v4: ptr, v5: u64 = alloc for seq u64 v3 align 8 charge v3\n  v6: u64 = const 1\n  v7: u64 = const 2\n  address a4\n  v8: u64 = const 42\n  store a4, v8\n  store a0, v4\n  store a1, v6\n  store a2, v7\n  v9: u64 = const 32\n  v10: u64 = const 0\n  v11: u64 = const 0\n  v12: u64 = const 4\n  v13: u64 = const 8\n  v14: u64 = const 16\n{pre}  address a3\n  {grow}\n  v17: ptr = const 0x0\n  v18: bool = ne v15, v17\n  branch v18, b1, b2\nb1:\n  address a5\n{post}  v19: u64 = read a5\n  drop a3 t0\n  dead s0\n  return v19, v16\nb2:\n  v20: u64 = read a4\n  drop a3 t0\n  dead s0\n  return v20, v16\n}}\n"
        )
    };
    let grow =
        "v15: ptr, v16: u64 = grow a3 v9 align 8 charge v9 capacity v12 moved v13 refund v14";
    let run = |text: &str, limit: u64| {
        let m = load("grow", text);
        wf::module(&m).unwrap();
        std::panic::catch_unwind(|| eval::run(&m, FuncId(0), &[], limit))
            .map(|r| (r.outcome, r.used))
            .map_err(|e| match e.downcast::<String>() {
                Ok(s) => *s,
                Err(e) => e
                    .downcast::<&str>()
                    .map_or_else(|_| String::new(), |s| s.to_string()),
            })
    };
    // Bug: the element stays behind, or the new block is charged other than its
    // size, so dropping the grown list (capacity 4: 32 bytes) leaves `used` off 0.
    assert_eq!(
        run(&module("", grow, ""), u64::MAX),
        Ok((eval::Outcome::Returned(vec![42, 0]), 0))
    );
    // Bug: a refused grow frees or rewires the old block. Under a limit of 40 the
    // 32 new bytes do not fit beside the old 16: null, room 24, the list unchanged.
    assert_eq!(
        run(&module("", grow, ""), 40),
        Ok((eval::Outcome::Returned(vec![42, 24]), 0))
    );
    for (pre, grow, post, stop) in [
        // Bug: the old block outlives the grow, so a pointer read before it still
        // reaches the elements in one engine and garbage in another.
        ("", grow, "  v21: u64 = read a4\n", "use of a freed block"),
        // Bug: a producer passes the capacity's bytes, or zero, as `moved`.
        (
            "",
            &grow.replace("moved v13", "moved v3"),
            "",
            "another byte count",
        ),
        // Bug: a refund computed from the new capacity, so the old limit is
        // credited with what the new block charged.
        (
            "",
            &grow.replace("refund v14", "refund v9"),
            "",
            "another charge",
        ),
        // Bug: a grow below the length drops elements.
        (
            "",
            &grow.replace("capacity v12", "capacity v1"),
            "",
            "does not hold",
        ),
        // Bug: capacity 0 (empty, or static bytes) owns no storage to replace.
        ("  store a2, v1\n", grow, "", "capacity 0"),
        // Bug: the liveness shadow stays with the old block, so the moved live
        // slot reads as empty in the new one and the old one frees a live slot.
        (
            "  mark a4, v6, live : u64\n",
            grow,
            "  mark a5, v6, empty : u64\n  unmark a5, v6, empty : u64\n  store a5, v8\n",
            "",
        ),
    ] {
        let got = run(&module(pre, grow, post), u64::MAX);
        if stop.is_empty() {
            assert_eq!(
                got,
                Ok((eval::Outcome::Returned(vec![42, 0]), 0)),
                "{pre}{post}"
            );
        } else {
            let err = got.expect_err(stop);
            assert!(err.contains(stop), "expected `{stop}`, got `{err}`");
        }
    }
    // Bug: a grow of a place that is not a sequence carrier (an element) would
    // read its words as a carrier's.
    let m = parse(&module("", &grow.replace("grow a3", "grow a4"), "")).unwrap();
    let err = wf::module(&m).expect_err("a grow of an element");
    assert!(err.contains("not a sequence carrier"), "{err}");
}

/// Bug: a producer other than core's kernels emits `mark` or gives an own row a
/// finalizer, so a user type gets cleanup that bypasses its recipe.
#[test]
fn only_core_marks_slots_and_finalizes_own_rows() {
    let kernel = include_str!("../samples/kernel.core");
    let m = load("kernel", kernel);
    wf::module(&m).expect("core's kernels are well formed");
    for (edits, why) in [
        (
            &[(
                "inst n2 = core@1/deque/push_back",
                "inst n2 = samples@0/deque/push_back",
            )][..],
            "only core's slot kernels emit `mark`",
        ),
        (
            &[
                ("own \"Deque[Res]\" drop f1", "own \"Deque[Res]\" drop f3"),
                (
                    "inst n3 = core@1/deque/pop_front",
                    "inst n3 = samples@0/deque/pop_front",
                ),
            ][..],
            "only core declares a finalizer",
        ),
        // An own row's finalizer runs where nothing can resume a failure, so it may
        // not fail itself (its step count checked) or through what it calls (reverse).
        (
            &[(
                "v20: u64 = wrapping_add v15, v19",
                "v20: u64 = add v15, v19",
            )][..],
            "an own row's finalizer may fail",
        ),
        (
            &[("v6: u64 = wrapping_add v3, v5", "v6: u64 = add v3, v5")][..],
            "an own row's finalizer may fail",
        ),
    ] {
        let mut text = kernel.to_string();
        for (from, to) in edits {
            assert!(text.contains(from));
            text = text.replace(from, to);
        }
        let err = wf::module(&load("kernel", &text)).expect_err(why);
        assert!(err.contains(why), "expected `{why}`, got `{err}`");
    }
}

/// Cleanup classes (crate docs, "Cleanup classes"). Bug: a producer names an edge
/// for an `alloc` other than the one that cleans the block, so the native prefix it
/// fixes (8 bytes terminal, 48 otherwise) does not fit the cleanup: a terminal
/// block cleaned as fixed gets a frame written before its payer word, and a fixed
/// block cleaned as terminal is freed at the wrong offset. Here the Node's box owns
/// a box of its own (fixed) and the Leaf's does not (terminal). The true edges run
/// (both boxes charged round8(8) = 8, freed, used 0); either swap stops.
#[test]
fn an_alloc_names_the_edge_that_cleans_it() {
    let module = |node: &str, leaf: &str| {
        format!(
            "core v1\ntype t0 = own \"Leaf\" {{ x: u64 }}\ntype t1 = own \"Node\" {{ b: box t0 }}\ntype t2 = own \"Top\" {{ n: box t1 }}\nsig g0 = () -> (u64)\ninst n0 = p@0/m/main [] []\nfn f0 = n0 g0 {{\n  slot s0 = t2\n  addr a0 = s0 byte v0 : ptr\n  addr a1 = v2 byte v0 : ptr\n  addr a2 = s0 : t2\nb0:\n  live s0\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: ptr, v3: u64 = alloc {node} v1 align 8 charge v1\n  v4: ptr, v5: u64 = alloc {leaf} v1 align 8 charge v1\n  address a0\n  address a1\n  address a2\n  store a1, v4\n  store a0, v2\n  drop a2 t2\n  dead s0\n  return v0\n}}\n"
        )
    };
    let run = |text: &str| {
        let m = load("terminal", text);
        wf::module(&m).unwrap();
        std::panic::catch_unwind(|| eval::run(&m, FuncId(0), &[], u64::MAX))
            .map(|r| r.used)
            .map_err(|e| {
                e.downcast::<String>()
                    .map_or_else(|_| String::new(), |s| *s)
            })
    };
    assert_eq!(run(&module("for box t1", "for box t0")), Ok(0));
    for (node, leaf) in [("for box t0", "for box t0"), ("for box t1", "for box t1")] {
        let err = run(&module(node, leaf)).unwrap_err();
        assert!(err.contains("its edge"), "{node}/{leaf}: {err}");
    }
}

/// Loans (crate docs, "Loans"). Bugs: a producer passes one place as a `mut`
/// argument and as another argument, writes a place it holds as a `view` (both
/// unsound once an emitter marks `mut` noalias and `view` readonly), or a body
/// writes its own `view` parameter.
#[test]
fn loans_stop_producer_aliasing() {
    let module = |main: &str, callee: &str| {
        format!(
            "core v1\ntype t0 = own \"P\" {{ a: u64 }}\nsig g0 = () -> ()\nsig g1 = (&view t0, &mut t0) -> ()\nsig g2 = (&view t0, ptr) -> ()\nsig g3 = (&take t0, &mut t0) -> ()\nsig g4 = (&view t0, &take t0) -> ()\nsig g5 = (&take t0, ptr) -> ()\ninst n0 = p@0/m/main [] []\ninst n1 = p@0/m/callee [] []\nfn f0 = n0 g0 {{\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 : t0\n  addr a1 = s1 : t0\nb0:\n  live s0\n  live s1\n  address a0\n  address a1\n{main}  return\n}}\n{callee}"
        )
    };
    // the callee writes its `mut` place, or a place it was handed as a pointer
    let write = |sig: &str, base: &str| {
        format!(
            "fn f1 = n1 {sig} {{\n  addr a0 = {base} : t0\n  addr a1 = a0 field 0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  address a1\n  v2: u64 = const 1\n  store a1, v2\n  return\n}}\n"
        )
    };
    let (mutate, poke) = (write("g1", "v1"), write("g2", "v1"));
    // the evaluator alone, as the oracle for what wf cannot see
    let eval_only = |text: &str| {
        let m = parse(text).unwrap();
        std::panic::catch_unwind(|| eval::run_checked(&m, FuncId(0), &[], u64::MAX))
            .map(|(_, checked)| checked.loans)
            .map_err(|e| {
                e.downcast::<String>()
                    .map_or_else(|_| String::new(), |s| *s)
            })
    };
    let wf_err = |text: &str| wf::module(&parse(text).unwrap()).expect_err(text);
    let run = |text: &str| {
        let m = parse(text).unwrap();
        wf::module(&m).unwrap();
        std::panic::catch_unwind(|| eval::run_checked(&m, FuncId(0), &[], u64::MAX))
            .map(|(_, checked)| checked.loans)
            .map_err(|e| {
                e.downcast::<String>()
                    .map_or_else(|_| String::new(), |s| *s)
            })
    };
    assert_eq!(
        run(&module("  call f1 g1 (&a0, &a1)\n", &mutate)),
        Ok(2),
        "distinct places run, and both loans are checked"
    );
    // one place lent as mut and as a view or a take: wf refuses it, and the
    // evaluator faults it when wf is bypassed
    for (call, callee) in [
        ("  call f1 g1 (&a0, &a0)\n", mutate.clone()),
        ("  call f1 g3 (&a0, &a0)\n", write("g3", "v1")),
    ] {
        let text = module(call, &callee);
        let err = wf_err(&text);
        assert!(
            err.contains("a mut place overlaps another argument"),
            "got `{err}`"
        );
        let shared = eval_only(&text).unwrap_err();
        assert!(
            shared.contains("overlaps another argument's place"),
            "got `{shared}`"
        );
    }
    // a take lent from a place another argument views: nothing writes it
    let reader = "fn f1 = n1 g4 {\n  addr a0 = v0 : t0\n  addr a1 = a0 field 0 : u64\n  addr a2 = v1 : t0\n  addr a3 = a2 field 0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  address a1\n  address a2\n  address a3\n  v2: u64 = read a1\n  v3: u64 = read a3\n  return\n}\n";
    assert_eq!(run(&module("  call f1 g4 (&a0, &a0)\n", reader)), Ok(2));
    let poked = run(&module(
        "  v0: ptr = pointer a0\n  call f1 g2 (&a0, v0)\n",
        &poke,
    ))
    .unwrap_err();
    assert!(
        poked.contains("into a lent view or take place"),
        "got `{poked}`"
    );
    let taken = run(&module(
        "  v0: ptr = pointer a0\n  call f1 g5 (&a0, v0)\n",
        &write("g5", "v1"),
    ))
    .unwrap_err();
    assert!(
        taken.contains("into a lent view or take place"),
        "got `{taken}`"
    );
    // A value place (`&mut u64`, #88: a `mut` scalar or Region parameter) is lent
    // as any by-address place: its size comes from the place type, so the loan is
    // checked; the callee's write is the caller's (5 becomes 7).
    let cell = |call: &str| {
        format!(
            "core v1\nsig g0 = () -> (u64)\nsig g1 = (&mut u64, &mut u64) -> ()\ninst n0 = p@0/m/main [] []\ninst n1 = p@0/m/callee [] []\nfn f0 = n0 g0 {{\n  slot s0 = u64\n  slot s1 = u64\n  addr a0 = s0 : u64\n  addr a1 = s1 : u64\nb0:\n  live s0\n  live s1\n  address a0\n  address a1\n  v0: u64 = const 5\n  store a0, v0\n  store a1, v0\n  call f1 g1 ({call})\n  v1: u64 = read a0\n  dead s1\n  dead s0\n  return v1\n}}\nfn f1 = n1 g1 {{\n  addr a0 = v0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  v2: u64 = const 7\n  store a0, v2\n  return\n}}\n"
        )
    };
    let m = parse(&cell("&a0, &a1")).unwrap();
    wf::module(&m).unwrap();
    let (report, checked) = eval::run_checked(&m, FuncId(0), &[], u64::MAX);
    assert_eq!(
        (report.outcome, checked.loans),
        (eval::Outcome::Returned(vec![7]), 2)
    );
    let twice = cell("&a0, &a0");
    let err = wf_err(&twice);
    assert!(
        err.contains("a mut place overlaps another argument"),
        "got `{err}`"
    );
    let shared = eval_only(&twice).unwrap_err();
    assert!(
        shared.contains("overlaps another argument's place"),
        "got `{shared}`"
    );
    // wf: a body writes its own take parameter's incoming place
    let err = wf_err(&module("", &write("g5", "v0")));
    assert!(
        err.contains("writes a take parameter's place"),
        "got `{err}`"
    );
    // wf: a body writes its own view parameter, directly or through `pointer`
    for callee in [
        write("g2", "v0"),
        "fn f1 = n1 g2 {\n  addr a0 = v0 : t0\n  addr a1 = v2 : t0\n  addr a2 = a1 field 0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  v2: ptr = pointer a0\n  address a1\n  address a2\n  v3: u64 = const 1\n  store a2, v3\n  return\n}\n".into(),
    ] {
        let m = parse(&module("", &callee)).unwrap();
        let err = wf::module(&m).expect_err("a write into a view parameter");
        assert!(err.contains("writes a view parameter's place"), "got `{err}`");
    }
}
/// Storage roles (crate docs, "Roles"): the evaluator stops a run whose accesses
/// break a role's claim, and the derivation claims nothing a valid body breaks.
#[test]
fn roles_hold_or_stop_the_run() {
    let list = "core v1\ntype t0 = own \"List[u8]\" { items: seq u8 }\n";
    let run = |text: &str, f: u32, args: &[u64]| {
        let m = parse(&format!("{list}{text}")).unwrap();
        wf::module(&m).unwrap_or_else(|e| panic!("{e}"));
        std::panic::catch_unwind(|| eval::run(&m, FuncId(f), args, u64::MAX))
            .map(|r| r.outcome)
            .map_err(|e| {
                e.downcast::<String>()
                    .map_or_else(|_| String::new(), |s| *s)
            })
    };
    // Bug: a kernel points one carrier's data word at another carrier's place, so a
    // buffer store rewrites that carrier's length behind LLVM's alias scopes. The
    // store lands at s0 + 8, the bytes s0's length store touched first.
    let aliased = run(
        "sig g0 = () -> ()\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 byte v0 : u64\n  addr a1 = s0 : t0\n  addr a2 = s1 byte v1 : ptr\n  addr a3 = v3 index v0 : u8\nb0:\n  live s0\n  live s1\n  v0: u64 = const 8\n  v1: u64 = const 0\n  address a0\n  store a0, v0\n  address a1\n  v2: ptr = pointer a1\n  address a2\n  store a2, v2\n  v3: ptr = read a2\n  v4: u8 = const 1\n  address a3\n  store a3, v4\n  dead s1\n  dead s0\n  return\n}\n",
        0,
        &[],
    );
    let stop = aliased.expect_err("a buffer store over a carrier ran to its end");
    assert!(stop.contains("Buffers touches +8..+9"), "{stop}");
    // A caller with one carrier of length 3 in s0, lent as `&a0` to f0.
    let caller = |sig: &str, call: &str| {
        format!(
            "sig g0 = {sig} -> (u64)\nsig g1 = () -> (u64)\ninst n0 = core@1/m/f [] []\ninst n1 = core@1/m/main [] []\n{{f0}}fn f1 = n1 g1 {{\n  slot s0 = t0\n  addr a0 = s0 : t0\n  addr a1 = s0 byte v0 : ptr\n  addr a2 = s0 byte v1 : u64\n  addr a3 = s0 byte v2 : u64\nb0:\n  live s0\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: u64 = const 16\n  v3: ptr = const 0x0\n  v4: u64 = const 3\n  address a0\n  address a1\n  address a2\n  address a3\n  store a1, v3\n  store a2, v4\n  store a3, v0\n  v5: u64 = {call}\n  dead s0\n  return v5\n}}\n"
        )
    };
    // Bug: a `view` parameter gets a carrier role. Two views of one place may be
    // passed together (OWN-8), so reading both is valid: 3 + 3.
    let views = caller("(&view t0, &view t0)", "call f0 g0 (&a0, &a0)").replace(
        "{f0}",
        "fn f0 = n0 g0 {\n  addr a0 = v0 byte v2 : u64\n  addr a1 = v1 byte v2 : u64\nb0(v0: ptr, v1: ptr):\n  v2: u64 = const 8\n  address a0\n  address a1\n  v3: u64 = read a0\n  v4: u64 = read a1\n  v5: u64 = add v3, v4\n  return v5\n}\n",
    );
    assert_eq!(run(&views, 1, &[]), Ok(eval::Outcome::Returned(vec![6])));
    // Bug: the entry block's parameter keeps its parameter's role after an edge
    // rebinds it. f0 reads the length through its parameter (3), sets its own
    // slot's length to 7, re-enters with that slot, and reads 7 through the same
    // address: the parameter role would name the slot's bytes.
    let reentry = caller("(&mut t0)", "call f0 g0 (&a0)").replace(
        "{f0}",
        "fn f0 = n0 g0 {\n  slot s0 = t0\n  addr a0 = v0 byte v1 : u64\n  addr a1 = s0 byte v1 : u64\n  addr a2 = s0 : t0\nb0(v0: ptr):\n  v1: u64 = const 8\n  v3: u64 = const 7\n  address a0\n  v2: u64 = read a0\n  v4: bool = eq v2, v3\n  branch v4, b2, b1\nb1:\n  live s0\n  address a1\n  store a1, v3\n  address a2\n  v5: ptr = pointer a2\n  jump b0(v5)\nb2:\n  return v2\n}\n",
    );
    assert_eq!(run(&reentry, 1, &[]), Ok(eval::Outcome::Returned(vec![7])));
    // Bug: each carrier's buffer gets its own role. A move hands the buffer to
    // another carrier: element 0 is written through s0's data and read back (5)
    // through s1's, the same bytes under two carriers.
    let moved = run(
        "sig g0 = () -> (u8)\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 byte v0 : ptr\n  addr a1 = s0 byte v1 : u64\n  addr a2 = s0 byte v2 : u64\n  addr a3 = v6 index v0 : u8\n  addr a4 = s1 : t0\n  addr a5 = s0 : t0\n  addr a6 = s1 byte v0 : ptr\n  addr a7 = v9 index v0 : u8\nb0:\n  live s0\n  live s1\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: u64 = const 16\n  v3: u64 = const 1\n  v4: ptr, v5: u64 = alloc for seq u8 v1 align 8 charge v1\n  address a0\n  address a1\n  address a2\n  store a0, v4\n  store a1, v3\n  store a2, v1\n  v6: ptr = read a0\n  v7: u8 = const 5\n  address a3\n  store a3, v7\n  v8: u64 = const 24\n  address a4\n  address a5\n  copy a4, a5, v8\n  address a6\n  v9: ptr = read a6\n  address a7\n  v10: u8 = read a7\n  free v4 charge v1\n  dead s1\n  dead s0\n  return v10\n}\n",
        0,
        &[],
    );
    assert_eq!(moved, Ok(eval::Outcome::Returned(vec![5])));

    // f0(&mut a, &mut b) reads a[0], runs `mid`, then reads b[0] and returns the sum.
    // Its caller lends two carriers of length 1 whose buffers hold 5 and 6 (or, with
    // `static`, one static byte 5 for both), and frees what it allocated.
    let two = |head: &str, mid: &str, tail: &str, stat: bool| {
        let (setup, fini) = if stat {
            (
                "  v4: ptr = static d0\n  v5: u64 = const 0\n  v6: ptr = static d0\n  v7: u64 = const 0\n  v8: u8 = const 0\n  v9: u8 = const 0\n  v10: u64 = const 0\n",
                "",
            )
        } else {
            (
                "  v4: ptr, v5: u64 = alloc for seq u8 v1 align 8 charge v1\n  v6: ptr, v7: u64 = alloc for seq u8 v1 align 8 charge v1\n  v8: u8 = const 5\n  v9: u8 = const 6\n  address a8\n  store a8, v8\n  address a9\n  store a9, v9\n  v10: u64 = const 8\n",
                "  free v4 charge v1\n  free v6 charge v1\n",
            )
        };
        let text = format!(
            "static d0 = align 1 hex 05\nsig g0 = (&mut t0, &mut t0) -> (u8)\nsig g1 = () -> (u8)\nsig g2 = (&mut t0, &mut t0) -> ()\ninst n0 = core@1/m/f [] []\ninst n1 = core@1/m/main [] []\ninst n2 = core@1/m/swap [] []\n\
             fn f0 = n0 g0 {{\n  addr a0 = v0 byte v2 : ptr\n  addr a1 = v1 byte v2 : ptr\n  addr a2 = v3 index v2 : u8\n{head}b0(v0: ptr, v1: ptr):\n  v2: u64 = const 0\n  address a0\n  address a1\n  v3: ptr = read a0\n  address a2\n  v4: u8 = read a2\n{mid}{tail}}}\n\
             fn f1 = n1 g1 {{\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 : t0\n  addr a1 = s0 byte v0 : ptr\n  addr a2 = s0 byte v1 : u64\n  addr a3 = s0 byte v2 : u64\n  addr a4 = s1 : t0\n  addr a5 = s1 byte v0 : ptr\n  addr a6 = s1 byte v1 : u64\n  addr a7 = s1 byte v2 : u64\n  addr a8 = v4 index v0 : u8\n  addr a9 = v6 index v0 : u8\nb0:\n  live s0\n  live s1\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: u64 = const 16\n  v3: u64 = const 1\n{setup}  address a0\n  address a1\n  address a2\n  address a3\n  address a4\n  address a5\n  address a6\n  address a7\n  store a1, v4\n  store a2, v3\n  store a3, v10\n  store a5, v6\n  store a6, v3\n  store a7, v10\n  v11: u8 = call f0 g0 (&a0, &a4)\n{fini}  dead s1\n  dead s0\n  return v11\n}}\n\
             fn f2 = n2 g2 {{\n  slot s0 = t0\n  addr a0 = s0 : t0\n  addr a1 = v0 : t0\n  addr a2 = v1 : t0\nb0(v0: ptr, v1: ptr):\n  live s0\n  v2: u64 = const 24\n  address a0\n  address a1\n  address a2\n  copy a0, a1, v2\n  copy a1, a2, v2\n  copy a2, a0, v2\n  dead s0\n  return\n}}\n"
        );
        let r = run(&text, 1, &[]);
        (text, r)
    };
    let sum = |v: u32| {
        let (y, z) = (v + 1, v + 2);
        format!(
            "  v{v}: ptr = read a1\n  address a3\n  v{y}: u8 = read a3\n  v{z}: u8 = add v4, v{y}\n  return v{z}\n"
        )
    };
    let read_b = |v: u32| format!("  addr a3 = v{v} index v2 : u8\n");
    // Two sealed parameters (merge's `a` and `temp`) get two buffer roles, so LLVM
    // gives them two scopes; and they may read one static literal together.
    let (text, both) = two(&read_b(5), "", &sum(5), true);
    assert_eq!(both, Ok(eval::Outcome::Returned(vec![10])));
    let m = parse(&format!("{list}{text}")).unwrap();
    let r = roles::roles(&m, &m.functions[0]);
    assert_eq!(
        (r[2], r[3]),
        (
            Some(roles::Role::Buffer(roles::Root::Param(0))),
            Some(roles::Role::Buffer(roles::Root::Param(1)))
        )
    );
    // Bug: a `mut` lend leaves the carrier sealed. The callee swaps a and b, so a's
    // buffer is read again through b: 5 + 5.
    let (_, swapped) = two(
        &format!("{}  addr a4 = v0 : t0\n  addr a5 = v1 : t0\n", read_b(5)),
        "  address a4\n  address a5\n  call f2 g2 (&a4, &a5)\n",
        &sum(5),
        false,
    );
    assert_eq!(swapped, Ok(eval::Outcome::Returned(vec![10])));
    // Bug: a carrier copied whole stays sealed. b := a, then b[0] is a's 5.
    let (_, copied) = two(
        &format!("{}  addr a4 = v0 : t0\n  addr a5 = v1 : t0\n", read_b(6)),
        "  address a4\n  address a5\n  v5: u64 = const 24\n  copy a5, a4, v5\n",
        &sum(6),
        false,
    );
    assert_eq!(copied, Ok(eval::Outcome::Returned(vec![10])));
    // Bug: a's data pointer stored into b's data word leaves either sealed.
    let (_, escaped) = two(&read_b(5), "  store a1, v3\n", &sum(5), false);
    assert_eq!(escaped, Ok(eval::Outcome::Returned(vec![10])));
    // Bug: one fresh block installed in both data words leaves both sealed: 7 is
    // written through a's data and read through b's, 5 + 7.
    let (_, shared) = two(
        &format!("{}  addr a4 = v8 index v2 : u8\n", read_b(10)),
        "  v5: u64 = const 8\n  v6: ptr, v7: u64 = alloc for seq u8 v5 align 8 charge v5\n  store a0, v6\n  store a1, v6\n  v8: ptr = read a0\n  address a4\n  v9: u8 = const 7\n  store a4, v9\n",
        "  v10: ptr = read a1\n  address a3\n  v11: u8 = read a3\n  free v6 charge v5\n  v12: u8 = add v4, v11\n  return v12\n",
        false,
    );
    assert_eq!(shared, Ok(eval::Outcome::Returned(vec![12])));
}

/// Frame slots end with their activation (crate docs, "Values, slots and
/// addresses"). Bug: a producer keeps an address into a callee's frame past its
/// return, as a scoped environment built in a nested closure body and stored into
/// an outer slot would (SCOPED-ENV). Native code reads a dead stack there; the
/// evaluator must stop rather than hand back the stale 7 (which it did before
/// slots ended at exit).
#[test]
fn a_frame_slot_ends_with_its_activation() {
    let module = |main: &str| {
        format!(
            "core v1\nsig g0 = () -> (u64)\nsig g1 = () -> (ptr)\ninst n0 = p@0/m/main [] []\ninst n1 = p@0/m/leak [] []\nfn f0 = n0 g0 {{\n  addr a0 = v0 : u64\nb0:\n{main}}}\nfn f1 = n1 g1 {{\n  slot s0 = u64\n  addr a0 = s0 : u64\nb0:\n  live s0\n  v0: u64 = const 7\n  address a0\n  store a0, v0\n  v1: ptr = pointer a0\n  return v1\n}}\n"
        )
    };
    let run = |text: &str| {
        let m = load("frame", text);
        wf::module(&m).unwrap();
        std::panic::catch_unwind(|| eval::run(&m, FuncId(0), &[], u64::MAX))
            .map(|r| r.outcome)
            .map_err(|e| match e.downcast::<String>() {
                Ok(s) => *s,
                Err(e) => e
                    .downcast::<&str>()
                    .map_or_else(|_| String::new(), |s| s.to_string()),
            })
    };
    let err = run(&module(
        "  v0: ptr = call f1 g1 ()\n  address a0\n  v1: u64 = read a0\n  return v1\n",
    ))
    .unwrap_err();
    assert!(err.contains("use of a freed block"), "{err}");
}

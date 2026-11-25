//! Evaluator tests over the FORM v0 samples and hand-written modules. Every
//! expected value is the hand-derived `run:` line in the sample file or a value
//! derived in the test, not output of this crate.

mod aliasing;

use crate as eval;
use hirsel_core_ir::text::{parse, print};
use hirsel_core_ir::*;

const SAMPLES: &[(&str, &str)] = &[
    (
        "arith",
        include_str!("../../hirsel-core-ir/samples/arith.core"),
    ),
    (
        "compare-convert",
        include_str!("../../hirsel-core-ir/samples/compare-convert.core"),
    ),
    (
        "control",
        include_str!("../../hirsel-core-ir/samples/control.core"),
    ),
    (
        "slots",
        include_str!("../../hirsel-core-ir/samples/slots.core"),
    ),
    (
        "calls",
        include_str!("../../hirsel-core-ir/samples/calls.core"),
    ),
    (
        "failure",
        include_str!("../../hirsel-core-ir/samples/failure.core"),
    ),
    (
        "cleanup",
        include_str!("../../hirsel-core-ir/samples/cleanup.core"),
    ),
    (
        "admission",
        include_str!("../../hirsel-core-ir/samples/admission.core"),
    ),
    (
        "box-chain",
        include_str!("../../hirsel-core-ir/samples/box-chain.core"),
    ),
    (
        "closure-chain",
        include_str!("../../hirsel-core-ir/samples/closure-chain.core"),
    ),
    (
        "finalizer-chain",
        include_str!("../../hirsel-core-ir/samples/finalizer-chain.core"),
    ),
    (
        "seq-chain",
        include_str!("../../hirsel-core-ir/samples/seq-chain.core"),
    ),
    (
        "seq-box-chain",
        include_str!("../../hirsel-core-ir/samples/seq-box-chain.core"),
    ),
    (
        "resource-chain",
        include_str!("../../hirsel-core-ir/samples/resource-chain.core"),
    ),
    (
        "receiver-chain",
        include_str!("../../hirsel-core-ir/samples/receiver-chain.core"),
    ),
    (
        "counted",
        include_str!("../../hirsel-core-ir/samples/counted.core"),
    ),
    (
        "counted-frames",
        include_str!("../../hirsel-core-ir/samples/counted-frames.core"),
    ),
    (
        "custody",
        include_str!("../../hirsel-core-ir/samples/custody.core"),
    ),
    (
        "variants",
        include_str!("../../hirsel-core-ir/samples/variants.core"),
    ),
    (
        "sequences",
        include_str!("../../hirsel-core-ir/samples/sequences.core"),
    ),
    (
        "strings",
        include_str!("../../hirsel-core-ir/samples/strings.core"),
    ),
    (
        "providers",
        include_str!("../../hirsel-core-ir/samples/providers.core"),
    ),
    (
        "stable-keys",
        include_str!("../../hirsel-core-ir/samples/stable-keys.core"),
    ),
    (
        "scalar-rows",
        include_str!("../../hirsel-core-ir/samples/scalar-rows.core"),
    ),
    (
        "kernel",
        include_str!("../../hirsel-core-ir/samples/kernel.core"),
    ),
    (
        "isolate",
        include_str!("../../hirsel-core-ir/samples/isolate.core"),
    ),
    ("log", include_str!("../../hirsel-core-ir/samples/log.core")),
];

fn load(name: &str, text: &str) -> Module {
    parse(text).unwrap_or_else(|e| panic!("{name}: line {}: {}", e.line, e.message))
}

fn launch_module() -> Module {
    let module = load(
        "launch",
        "core v1\nsig g0 = () -> (u64, u64, u64)\nsig g1 = (u64, u64) -> (u64)\ninst n0 = app@1/main/launch [] []\nfn f0 = n0 g0 {\nb0:\n v0:u64=const 0\n v1:u64=const 1\n v2:u64=const 2\n v3:u64=call rt.launch_word g1 (v0,v0)\n v4:u64=call rt.launch_word g1 (v1,v0)\n v5:u64=call rt.launch_word g1 (v2,v0)\n return v3,v4,v5\n}\n",
    );
    wf::module(&module).unwrap();
    module
}

#[test]
fn evaluator_identity_unavailable() {
    let module = launch_module();
    let source = hirsel_core_ids::launch::Metadata::source("test", 73, false);
    assert_eq!(
        eval::run_launch(&module, FuncId(0), &[], 4096, &[], &source).outcome,
        eval::Outcome::Returned(vec![73, 0, 0])
    );
    let missing = std::env::temp_dir().join(format!("hirsel-eval-missing-{}", std::process::id()));
    let unreadable = hirsel_core_ids::launch::Metadata::artifact(&missing, "test", 73, false);
    assert_eq!(
        eval::run_launch(&module, FuncId(0), &[], 4096, &[], &unreadable).outcome,
        eval::Outcome::Returned(vec![73, 0, 0])
    );
}

#[test]
fn evaluator_identity_stable() {
    let module = launch_module();
    let path = std::env::temp_dir().join(format!("hirsel-eval-image-{}", std::process::id()));
    std::fs::write(&path, b"abc").unwrap();
    let artifact = hirsel_core_ids::launch::Metadata::artifact(&path, "test", 73, false);
    std::fs::write(&path, b"replacement image").unwrap();
    assert_eq!(
        eval::run_launch(&module, FuncId(0), &[], 4096, &[], &artifact).outcome,
        eval::Outcome::Returned(vec![73, 32, 0xba])
    );
    let loaded = hirsel_core_ids::launch::Metadata::loaded(b"abc", "test", 73, false);
    assert_eq!(loaded.digest(), artifact.digest());
    std::fs::remove_file(path).unwrap();
    let embedded = hirsel_core_ids::launch::Metadata::embedded(Some([42; 32]), "test", 419, true);
    assert_eq!(
        eval::run_launch(&module, FuncId(0), &[], 4096, &[], &embedded).outcome,
        eval::Outcome::Returned(vec![419, 32, 42])
    );
}

/// A host destination receives the returned bytes without admission. A loop
/// with no instructions still spends its explicit allowance on terminators.
#[test]
fn host_return_place_and_instruction_allowance() {
    let module = load(
        "host destination",
        "core v1\ntype t0 = own \"Out\" { word: u64 }\nsig g0 = () -> () dest t0\ninst n0 = app@1/main/result [] []\nfn f0 = n0 g0 {\n addr a0 = v0 : t0\n addr a1 = a0 field 0 : u64\nb0(v0: ptr):\n address a0\n address a1\n v1: u64 = const 73\n store a1, v1\n return\n}\n",
    );
    wf::module(&module).unwrap();
    let mut destination = [0; 8];
    let (report, checked) = eval::run_nans_into(
        &module,
        FuncId(0),
        &[],
        0,
        eval::Nans::Scrambled,
        Some(&mut destination),
        100,
        &eval::Revisions::new(),
    );
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![]));
    assert_eq!(u64::from_le_bytes(destination), 73);
    assert_eq!(report.used, 0);
    // Two address instructions, a constant, a store and the return.
    assert_eq!(checked.instructions, 5);
    let looping = load(
        "budget",
        "core v1\nsig g0 = () -> ()\ninst n0 = app@1/main/looping [] []\nfn f0 = n0 g0 {\nb0:\n jump b0\n}\n",
    );
    wf::module(&looping).unwrap();
    let fault = std::panic::catch_unwind(|| {
        eval::run_nans_into(
            &looping,
            FuncId(0),
            &[],
            0,
            eval::Nans::Canonical,
            None,
            3,
            &eval::Revisions::new(),
        )
    })
    .err()
    .expect("the explicit allowance stops an empty loop");
    assert_eq!(
        fault.downcast_ref::<&str>().copied(),
        Some("evaluator instruction budget exhausted")
    );
}

/// Bug: a sample's lowering, or this crate's statement of an op's meaning, differs
/// from the hand-derived behaviour written beside the source.
#[test]
fn samples_behave_as_derived_by_hand() {
    for (name, text) in SAMPLES {
        let mut runs = 0;
        let m = load(name, text);
        for line in text.lines() {
            // `; run [limit N] [refuse K[+]] [args a b]: fK(args) -> results | used N | trace a, b
            // | out text | err text`, or `-> fail Cause, ...`; each check after the
            // results is made when written (`\n` in a text is a newline). `refuse K`
            // refuses the run's K-th admission, `K+` that one and every later one, as
            // the native agreement harnesses' injection (K, later) does.
            let Some(run) = line.strip_prefix("; run") else {
                continue;
            };
            let (head, run) = run.split_once(':').unwrap();
            let (head, argv) = match head.split_once("args") {
                Some((h, a)) => (h, a.split_whitespace().collect()),
                None => (head, Vec::new()),
            };
            let (head, injection) = match head.split_once("refuse") {
                Some((h, k)) => {
                    let k = k.trim();
                    let later = k.ends_with('+');
                    (
                        h,
                        Some((k.trim_end_matches('+').parse::<usize>().unwrap(), later)),
                    )
                }
                None => (head, None),
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
            let got = match injection {
                Some(_) => {
                    assert!(argv.is_empty(), "{name}: refuse with args");
                    eval::run_injected(&m, f, &args, limit, injection)
                }
                None => eval::run_args(&m, f, &args, limit, &argv),
            };
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

/// Bug: a cause inside core reports core's own text (or none) instead of the user
/// call that reached it, or a `raise` reports the anchor of the instruction before
/// it instead of its block's own. Law (SITES.md): the site is the raising
/// operation's anchor, unless that is core's or missing, then the anchor of the
/// nearest enclosing non-core activation; a raise uses its block's anchor; with no
/// such activation the site is none. Expected anchors are read off the text below.
/// SITES.md 6(b): the walk passes only through bodies that inherit their caller's
/// site (core@1, or a generated name with `$`). A user body that leaves a failure
/// unanchored is a producer mistake the evaluator stops on, since engines pass it
/// no site; a user body that anchors a failure in core is refused statically.
#[test]
fn failure_sites_follow_the_contract() {
    let m = load(
        "sites",
        "core v1\nitem i0 = p@0/m/user\nitem i1 = core@1/m/k\nsig g0 = (u64) -> (u64)\ninst n0 = core@1/m/k [] []\ninst n1 = p@0/m/user [] []\ninst n2 = p@0/m/check [] []\nfn f0 = n0 g0 {\nb0(v0: u64):\n  v1: u64 = const 1\n  v2: u64 = add v0, v1 at i1 5..10\n  return v2\n}\nfn f1 = n1 g0 {\nb0(v0: u64):\n  v1: u64 = call f0 g0 (v0) at i0 20..30\n  return v1\n}\nfn f2 = n2 g0 {\nb0(v0: u64):\n  v1: u64 = const 0 at i0 50..55\n  v2: bool = eq v0, v1 at i0 60..65\n  branch v2, b1, b2\nb1:\n  v3: u64 = const 1 at i0 70..75\n  raise unwrap at i0 40..45\nb2:\n  return v0\n}\ninst n3 = p@0/m/helper [] []\ninst n4 = p@0/m/user.$glue [] []\nfn f3 = n3 g0 {\nb0(v0: u64):\n  v1: u64 = const 1\n  v2: u64 = add v0, v1\n  return v2\n}\nfn f4 = n1 g0 {\nb0(v0: u64):\n  v1: u64 = call f3 g0 (v0) at i0 80..90\n  return v1\n}\nfn f5 = n4 g0 {\nb0(v0: u64):\n  v1: u64 = const 1\n  v2: u64 = add v0, v1\n  return v2\n}\nfn f6 = n1 g0 {\nb0(v0: u64):\n  v1: u64 = call f5 g0 (v0) at i0 90..95\n  return v1\n}\n",
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
    assert_eq!(
        site(6, u64::MAX),
        vec![at(90, 95)],
        "generated glue: the user call"
    );
    let stop = std::panic::catch_unwind(|| site(4, u64::MAX))
        .err()
        .map(|e| match e.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => e
                .downcast::<&str>()
                .map_or_else(|_| String::new(), |s| s.to_string()),
        });
    assert!(
        stop.as_deref()
            .is_some_and(|s| s.contains("does not inherit its caller's site")),
        "an unanchored failure in a user body falls back to its caller: {stop:?}"
    );
    // The same core-anchored add as f0's, in a user body: no engine passes it a site.
    let user_core = print(&m).replacen("fn f0 = n0 g0", "fn f0 = n2 g0", 1);
    let e = wf::module(&parse(&user_core).unwrap()).unwrap_err();
    assert!(e.contains("f0: a failure point anchored in core"), "{e}");
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

/// Retiring a buffer must keep its address identity and all stale-pointer faults,
/// including after another allocation. Native engines trust lifetime checking;
/// these deliberately malformed runs test the evaluator's oracle itself.
#[test]
fn retired_blocks_keep_stale_read_write_and_free_faults() {
    for (operation, expected) in [
        ("  v5: u64 = read a0\n", "use of a freed block"),
        ("  store a0, v0\n", "use of a freed block"),
        ("  free v1 charge v0\n", "free of a non-block"),
        ("  mark a0, v0, live : u64\n", "mark of a freed block"),
        ("  unmark a0, v0, empty : u64\n", "unmark of a freed block"),
    ] {
        let source = format!(
            "core v1\nsig g0 = () -> ()\ninst n0 = core@1/m/probe [] []\nfn f0 = n0 g0 {{\n  addr a0 = v1 : u64\nb0:\n  v0: u64 = const 8\n  v1: ptr, v2: u64 = alloc for seq u64 v0 align 8 charge v0\n  address a0\n  store a0, v0\n  free v1 charge v0\n  v3: ptr, v4: u64 = alloc for seq u64 v0 align 8 charge v0\n{operation}  free v3 charge v0\n  return\n}}\n"
        );
        let module = load("retired", &source);
        wf::module(&module).unwrap();
        let fault = std::panic::catch_unwind(|| eval::run(&module, FuncId(0), &[], 8))
            .err()
            .expect("a stale pointer must fault");
        let message = fault
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| fault.downcast_ref::<String>().map(String::as_str))
            .unwrap();
        assert_eq!(message, expected, "{operation}");
    }
}

/// FORM #97: the conformance harnesses run [`eval::Nans::Scrambled`] so that a producer
/// reading float-holding bytes at another type misses its hand-derived run line. The
/// bug this catches is a scramble that never reaches memory: such a pun would then
/// read the canonical NaN, and every corpus would pass. f0 stores `inf - inf`, reads
/// the slot back as a u64 through a pointer (the pun), then as an f64 through
/// `reinterpret` (the one observer).
#[test]
fn scrambled_nans_reach_a_pun_but_not_reinterpret() {
    let m = parse(
        "core v1\nsig g0 = (u64) -> (u64, u64)\ninst n0 = p@0/m/pun [] []\nfn f0 = n0 g0 {\n  slot s0 = f64\n  addr a0 = s0 : f64\n  addr a1 = v3 : u64\nb0(v0: u64):\n  live s0\n  address a0\n  v1: f64 = reinterpret v0\n  v2: f64 = sub v1, v1\n  store a0, v2\n  v3: ptr = pointer a0\n  address a1\n  v4: u64 = read a1\n  v5: f64 = read a0\n  v6: u64 = reinterpret v5\n  dead s0\n  return v4, v6\n}\n",
    )
    .unwrap();
    wf::module(&m).unwrap();
    const CANONICAL: u64 = 0x7ff8_0000_0000_0000;
    let run = |nans| match eval::run_nans(&m, FuncId(0), &[0x7ff0_0000_0000_0000], 0, nans)
        .0
        .outcome
    {
        eval::Outcome::Returned(xs) => (xs[0], xs[1]),
        other => panic!("{other:?}"),
    };
    assert_eq!(run(eval::Nans::Canonical), (CANONICAL, CANONICAL));
    let (pun, observed) = run(eval::Nans::Scrambled);
    assert!(f64::from_bits(pun).is_nan() && pun != CANONICAL, "{pun:#x}");
    assert_eq!(observed, CANONICAL);
}

/// A spawned task stops the run before exhausting its small host stack. The
/// same recursive body still fits the root's unchanged stack.
#[test]
fn spawned_evaluator_stack_stops_without_cleanup() {
    let module = load(
        "task stack",
        r#"
core v1
type t0 = own "In" { n: u64 }
type t1 = own "Out" { n: u64 }
sig g0 = (u64) -> (u64)
sig g1 = (ptr, &take t0) -> () dest t1
sig g2 = (u64, &take t0, ptr, u64) -> (ptr, u64)
sig g3 = (ptr, &mut t1) -> (u8, ptr)
inst n0 = app@1/stack/main [] []
inst n1 = app@1/stack/worker [] []
inst n2 = app@1/stack/recurse [] []
fn f0 = n0 g0 {
 slot s0 = t0
 slot s1 = t1
 addr a0 = s0 : t0
 addr a1 = s0 field 0 : u64
 addr a2 = s1 : t1
 addr a3 = s1 field 0 : u64
b0(v0: u64):
 live s0
 live s1
 address a0
 address a1
 address a2
 address a3
 store a1, v0
 v1: u64 = const 65536
 v2: ptr = fnref f1
 v3: ptr, v4: u64 = call rt.spawn[t0, t1] g2 (v1, &a0, v2, v1)
 v5: u8, v6: ptr = call rt.join[t1] g3 (v3, &a2)
 v7: u64 = read a3
 return v7
}
fn f1 = n1 g1 {
 addr a0 = v0 : t1
 addr a1 = a0 field 0 : u64
 addr a2 = v2 : t0
 addr a3 = a2 field 0 : u64
b0(v0: ptr, v1: ptr, v2: ptr):
 address a0
 address a1
 address a2
 address a3
 v3: u64 = read a3
 v4: u64 = call f2 g0 (v3)
 store a1, v4
 drop a2 t0
 return
}
fn f2 = n2 g0 {
b0(v0: u64):
 v1: u64 = const 0
 v2: bool = eq v0, v1
 branch v2, b1, b2
b1:
 return v1
b2:
 v3: u64 = const 1
 v4: u64 = sub v0, v3
 v5: u64 = call f2 g0 (v4)
 v6: u64 = add v5, v3
 return v6
}
"#,
    );
    wf::module(&module).unwrap();
    assert_eq!(
        eval::run(&module, FuncId(0), &[6], 1 << 20).outcome,
        eval::Outcome::Returned(vec![6])
    );
    let stopped = eval::run(&module, FuncId(0), &[4096], 1 << 20);
    assert_eq!(stopped.outcome, eval::Outcome::Stopped("Stack"));
    assert_eq!(stopped.stderr, b"run stop: Stack\n");
    assert!(stopped.trace.is_empty());
    assert_eq!(
        eval::run(&module, FuncId(2), &[4096], 1 << 20).outcome,
        eval::Outcome::Returned(vec![4096])
    );
}

/// Moved from the body-encoding tests (D7): a cleanup-only edit that calls an
/// observer on the overflow edge leaves normal input alone and runs the observer,
/// which admits 37 bytes, when the add fails.
#[test]
fn cleanup_observer_runs_only_on_its_failure_edge() {
    let before = "core v1
            sig g0 = (u64) -> (u64)
            sig g1 = () -> ()
            inst n0 = app@1/main/f [] []
            inst n1 = app@1/main/observe [] []
            fn f0 = n0 g0 {
            b0(v0: u64):
                v1: u64 = const 1
                v2: u64 = add v0, v1 fail b1
                return v2
            b1:
                propagate
            }
            fn f1 = n1 g1 {
            b0:
                v0: u64 = const 37
                v1: bool, v2: u64 = limit_enter v0
                branch v1, b1, b2
            b1:
                limit_exit
                return
            b2:
                return
            }
        ";
    let after = before.replace("propagate", "call f1 g1 ()\n propagate");
    let (a, b) = (load("before", before), load("after", &after));
    wf::module(&a).unwrap();
    wf::module(&b).unwrap();
    let run = |m, arg| eval::run(m, FuncId(0), &[arg], 4096);
    let (a_ok, b_ok) = (run(&a, 7), run(&b, 7));
    assert_eq!(a_ok.outcome, eval::Outcome::Returned(vec![8]));
    assert_eq!(b_ok.outcome, a_ok.outcome);
    assert_eq!(
        b_ok.peak_reserved, a_ok.peak_reserved,
        "normal input skips the observer"
    );
    let (a_failed, b_failed) = (run(&a, u64::MAX), run(&b, u64::MAX));
    assert_eq!(a_failed.outcome, b_failed.outcome);
    assert!(matches!(b_failed.outcome, eval::Outcome::Failed(_)));
    assert_eq!(
        b_failed.peak_reserved,
        a_failed.peak_reserved + 37,
        "overflow cleanup ran the observer"
    );
}

/// Moved from the body-encoding tests (D11): a logical admission grants what the
/// limit has left, so seven requested of six available returns the six.
#[test]
fn logical_admission_grants_what_is_left() {
    let before = "core v1
            sig g0 = () -> (u64)
            inst n0 = app@1/main/admit [] []
            fn f0 = n0 g0 {
            b0:
                v0: u64 = const 5
                v1: u64 = const 7
                v2: bool, v3: u64 = limit_enter v0
                branch v2, b1, b2
            b1:
                limit_exit
                v4: u64 = const 1
                return v4
            b2:
                return v3
            }
        ";
    let a = load("five", before);
    let b = load("seven", &before.replace("limit_enter v0", "limit_enter v1"));
    let run = |module, limit| eval::run(module, FuncId(0), &[], limit).outcome;
    let returned = eval::Outcome::Returned(vec![1]);
    assert_eq!(run(&a, 8), returned);
    assert_eq!(run(&b, 8), returned);
    assert_eq!(run(&a, 6), returned);
    assert_eq!(
        run(&b, 6),
        eval::Outcome::Returned(vec![6]),
        "seven requested, six available"
    );
}

#[test]
fn ended_byte_limits_release_their_records() {
    use crate::{Limit, Machine, WorldInput};
    use hirsel_guard::admission::{Owner, OwnerKind};

    let module = Module::default();
    let mut machine = Machine::new(&module, vec![], 64, &WorldInput::default());
    let meter = Owner::new(OwnerKind::Ledger);
    let measure = meter.enter();
    let mut peak = 0;
    // MEM-20: each zero-byte scope has ended with no surviving reference.
    // Only the root and the current scope need records, regardless of count.
    let root = machine.root().clone();
    for _ in 0..4096 {
        machine.admit(&root, 0).unwrap();
        let scope = Limit::new(0, Some(root.clone()), 0);
        peak = peak.max(meter.bytes());
        machine.end_limit(scope, None);
    }
    assert_eq!(root.used(), 0);
    assert_eq!(meter.bytes(), 0);
    eprintln!("peak live byte-limit record allocations: {peak} bytes");
    assert!(peak <= 1024, "limit records used {peak} bytes");

    // A live allocation pins both ended records through hand-over. Freeing the
    // block refunds exactly its surviving charge to the root.
    drop(measure);
    machine.admit(&root, 64).unwrap();
    let outer = Limit::new(64, Some(root.clone()), 0);
    machine.admit(&outer, 48).unwrap();
    let inner = Limit::new(48, Some(outer.clone()), 0);
    machine.admit(&inner, 8).unwrap();
    let block = machine.block(8, Some(inner.clone()), 8, None);
    machine.end_limit(inner, None);
    machine.end_limit(outer, None);
    assert_eq!(root.used(), 8);
    machine.free(block);
    assert_eq!(root.used(), 0);
}

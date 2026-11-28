//! Tests over the CoreIR samples. Every expected value is the hand-derived `run:`
//! line in the sample file, not output of this crate.

mod frame;
mod metering;
mod own;

use crate::text::{parse, print};
use crate::*;

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
    ("box-chain", include_str!("../samples/box-chain.core")),
    (
        "closure-chain",
        include_str!("../samples/closure-chain.core"),
    ),
    (
        "finalizer-chain",
        include_str!("../samples/finalizer-chain.core"),
    ),
    ("seq-chain", include_str!("../samples/seq-chain.core")),
    (
        "seq-box-chain",
        include_str!("../samples/seq-box-chain.core"),
    ),
    (
        "resource-chain",
        include_str!("../samples/resource-chain.core"),
    ),
    (
        "receiver-chain",
        include_str!("../samples/receiver-chain.core"),
    ),
    ("counted", include_str!("../samples/counted.core")),
    (
        "counted-frames",
        include_str!("../samples/counted-frames.core"),
    ),
    ("custody", include_str!("../samples/custody.core")),
    ("variants", include_str!("../samples/variants.core")),
    ("sequences", include_str!("../samples/sequences.core")),
    ("strings", include_str!("../samples/strings.core")),
    ("providers", include_str!("../samples/providers.core")),
    ("stable-keys", include_str!("../samples/stable-keys.core")),
    ("scalar-rows", include_str!("../samples/scalar-rows.core")),
    ("kernel", include_str!("../samples/kernel.core")),
    ("isolate", include_str!("../samples/isolate.core")),
    ("log", include_str!("../samples/log.core")),
    ("steps", include_str!("../samples/steps.core")),
    ("int-widths", include_str!("../samples/int-widths.core")),
];

fn load(name: &str, text: &str) -> Module {
    parse(text).unwrap_or_else(|e| panic!("{name}: line {}: {}", e.line, e.message))
}

/// A slot gets storage markers only if every use is under a begin on all paths
/// in and its address leaves only on a loan that lasts the call; it then ends
/// where no path reaches a use before its next begin or end. Expected answers
/// are read off each body by hand: a use reached on a path with no begin, a
/// failure edge leaving before the begin that follows its instruction, and a
/// `mut` loan to a direct call with a result (Ruling 7) are refused; a begin
/// round a loop with or without an end, and a channel receive's destination,
/// are admitted. The ends are listed as (block, instruction after which the
/// slot ends, or `None` at the block's entry); a slot lent `view` to a call
/// with a pointer result, which may be anchored to it (OWN-20), ends only at its
/// `StorageDead`. An end is kept only where a `StorageLive` may follow, since
/// only a later begin can take the bytes; elsewhere the return ends the slot.
#[test]
fn slot_markers_follow_liveness_round_loops_joins_and_failure_edges() {
    let begin =
        |v: u32| format!("  live s0\n  address a0\n  store a0, v0\n  v{v}: ptr = pointer a0\n");
    /// A case's name, body, whether the slot is scoped, and its ends.
    type Case = (
        &'static str,
        String,
        bool,
        &'static [(usize, Option<usize>)],
    );
    let cases: [Case; 10] = [
        (
            "straight",
            format!(
                "fn f0 = n0 g0 {{\nb0(v0: i32, v1: i32):\n{}  dead s0\n  return v0\n",
                begin(2)
            ),
            true,
            &[],
        ),
        (
            "loop with an end",
            format!(
                "fn f0 = n0 g0 {{\nb0(v0: i32, v1: i32):\n  jump b1\nb1:\n  v2: bool = lt v0, v1\n  branch v2, b2, b3\nb2:\n{}  dead s0\n  jump b1\nb3:\n  return v0\n",
                begin(3)
            ),
            true,
            &[(2, Some(3))],
        ),
        (
            "begun again round a loop",
            format!(
                "fn f0 = n0 g0 {{\nb0(v0: i32, v1: i32):\n  jump b1\nb1:\n  v2: bool = lt v0, v1\n  branch v2, b2, b3\nb2:\n{}  jump b1\nb3:\n  return v0\n",
                begin(3)
            ),
            true,
            &[(2, Some(3))],
        ),
        (
            "begun round a loop whose failure edge reaches a use",
            format!(
                "fn f0 = n0 g0 {{\nb0(v0: i32, v1: i32):\n  jump b1\nb1:\n  v2: bool = lt v0, v1\n  branch v2, b2, b3\nb2:\n{}  v4: i32 = add v0, v1 fail b4\n  jump b1\nb3:\n  return v0\nb4:\n  address a0\n  v5: ptr = pointer a0\n  propagate\n",
                begin(3)
            ),
            true,
            &[(2, Some(4))],
        ),
        (
            "used after a join with one unbegun path",
            format!(
                "fn f0 = n0 g0 {{\nb0(v0: i32, v1: i32):\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n{}  jump b3\nb2:\n  jump b3\nb3:\n  address a0\n  v4: ptr = pointer a0\n  return v0\n",
                begin(3)
            ),
            false,
            &[],
        ),
        (
            "used where a failure before the begin lands",
            format!(
                "fn f0 = n0 g0 {{\nb0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n{}  return v2\nb1:\n  address a0\n  v4: ptr = pointer a0\n  propagate\n",
                begin(3)
            ),
            false,
            &[],
        ),
        (
            "received into",
            "fn f0 = n0 g3 {\nb0(v0: i32, v1: i32, v2: ptr):\n  live s0\n  address a0\n  v3: bool = call rt.receive[t0] g2 (v2, &a0)\n  address a0\n  v4: ptr = pointer a0\n  return v0\n".into(),
            true,
            &[],
        ),
        (
            "lent mut to a direct call with a result",
            "fn f0 = n0 g0 {\nb0(v0: i32, v1: i32):\n  live s0\n  address a0\n  v2: i32 = call f1 g1 (v0, &a0)\n  address a0\n  v3: ptr = pointer a0\n  return v0\n".into(),
            false,
            &[],
        ),
        (
            "needed on one arm only, begun again after the join",
            "fn f0 = n0 g0 {\nb0(v0: i32, v1: i32):\n  live s0\n  address a0\n  store a0, v0\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  v3: ptr = pointer a0\n  dead s0\n  jump b3\nb2:\n  jump b3\nb3:\n  live s0\n  dead s0\n  return v0\n".into(),
            true,
            &[(1, Some(0)), (2, None)],
        ),
        (
            "lent view to a call with a pointer result",
            "fn f0 = n0 g0 {\nb0(v0: i32, v1: i32):\n  live s0\n  address a0\n  store a0, v0\n  v2: ptr = call f2 g4 (&a0)\n  v3: i32 = add v0, v1\n  dead s0\n  return v0\n".into(),
            true,
            &[],
        ),
    ];
    for (case, body, scoped, ends) in cases {
        let text = format!(
            "core v1\nsig g0 = (i32, i32) -> (i32)\nsig g1 = (i32, &mut i32) -> (i32)\nsig g2 = (ptr, &mut i32) -> (bool)\nsig g3 = (i32, i32, ptr) -> (i32)\nsig g4 = (&view i32) -> (ptr)\ninst n0 = p@0/m/f [] []\n{}}}\nfn f1 = n0 g1 {{\nb0(v0: i32, v1: ptr):\n  return v0\n}}\nfn f2 = n0 g4 {{\nb0(v0: ptr):\n  return v0\n}}\n",
            body.replacen("{\n", "{\n  slot s0 = i32\n  addr a0 = s0 : i32\n", 1)
        );
        let m = parse(&text).unwrap_or_else(|e| panic!("{case}: {e:?}"));
        let b = m.functions[0].body.as_ref().unwrap();
        let answer = crate::extent::scoped_slots(b, &m.signatures);
        assert_eq!(answer.slots[0], scoped, "{case}");
        let mut placed = vec![];
        for (bi, blk) in b.blocks.iter().enumerate() {
            if answer.ends_at_entry(bi)[0] & 1 != 0 {
                placed.push((bi, None));
            }
            for j in 0..blk.insts.get(&b.insts).unwrap_or_default().len() {
                if answer
                    .ends_after(blk.insts.start as usize + j)
                    .any(|s| s == 0)
                {
                    placed.push((bi, Some(j)));
                }
            }
        }
        assert_eq!(placed, ends, "{case}");
    }
}

#[test]
fn closed_type_identity_has_one_row_and_keeps_its_package() {
    let mut m = load(
        "types",
        "core v1\ntype t0 = own \"a@1/m/Value\" { x: u8 }\n",
    );
    wf::module(&m).unwrap();
    // A producer creating the same row twice is a malformed module, even when
    // no call has yet exposed the unequal TypeIds.
    m.types.push(m.types[0].clone());
    assert!(
        wf::module(&m)
            .unwrap_err()
            .contains("duplicate closed type rows")
    );
    std::sync::Arc::make_mut(&mut m.types[1]).name = "b@1/m/Value".into();
    wf::module(&m).unwrap();
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
fn direct_calls_agree_with_their_callee_by_signature_value() {
    // Bug (m2b-table-tx-951): evaluator assembly demands one instance from two
    // callers, so a call site may spell its callee's signature with another, equal
    // row (rows are not interned); wf refused that program though the engines,
    // which agree across units by the value alone, ran it. A call through a
    // different signature is still refused: it would be an ABI disagreement.
    let module = |site: &str| {
        format!(
            "core v1\nsig g0 = (u64) -> (u64)\nsig g1 = (u64) -> (u64)\nsig g2 = (u64) -> (u32)\n\
             inst n0 = p@0/m/f [] []\ninst n1 = p@0/m/g [] []\n\
             fn f0 = n0 g0 {{\nb0(v0: u64):\n  return v0\n}}\n\
             fn f1 = n1 g0 {{\nb0(v0: u64):\n  v1: {site}\n  return v0\n}}\n"
        )
    };
    wf::module(&parse(&module("u64 = call f0 g1 (v0)")).unwrap())
        .expect("an equal signature row agrees");
    let e = wf::module(&parse(&module("u32 = call f0 g2 (v0)")).unwrap())
        .expect_err("a different signature disagrees");
    assert!(e.contains("direct call through another signature"), "{e}");
}

/// Bug: two slots share a home and the `dead` of one still ends it, so the other,
/// reached without its own `live` on that path, is used after the home's end.
/// Law: a body well formed before `share_slots` is well formed after it. s1 is
/// begun on b1 and s0 ends on b2. Their shared home stays live for s1's use in b3.
#[test]
fn shared_slots_keep_every_use_inside_its_storage() {
    let text = "core v1\nsig g0 = (i32, i32) -> (i32)\ninst n0 = p@0/m/f [] []\nfn f0 = n0 g0 {\n  slot s0 = i32\n  slot s1 = i32\n  addr a0 = s0 : i32\n  addr a1 = s1 : i32\nb0(v0: i32, v1: i32):\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  live s1\n  jump b3\nb2:\n  live s0\n  address a0\n  store a0, v0\n  v3: i32 = read a0\n  dead s0\n  jump b3\nb3:\n  address a1\n  store a1, v1\n  v4: i32 = read a1\n  dead s1\n  return v4\n}\n";
    let mut m = parse(text).unwrap();
    wf::module(&m).expect("the unshared body is well formed");
    let signatures = m.signatures.clone();
    crate::extent::share_slots(m.functions[0].body.as_mut().unwrap(), &signatures);
    wf::module(&m).expect("sharing keeps it well formed");
}

#[test]
fn sharing_preserves_conditional_initialization() {
    let mut m = load(
        "flags",
        r#"core v1
sig g0 = (bool) -> (u64)
inst n0 = p@0/m/f [] []
fn f0 = n0 g0 {
  slot s0 = bool
  slot s1 = bool
  slot s2 = u64
  addr a0 = s0 : bool
  addr a1 = s1 : bool
  addr a2 = s2 : u64
b0(v0: bool):
  live s0
  address a0
  store a0, v0
  dead s0
  live s1
  live s2
  address a1
  address a2
  v1: bool = const false
  store a1, v1
  v2: u64 = const 5
  branch v0, b1, b2
b1:
  store a2, v2
  v3: bool = const true
  store a1, v3
  jump b2
b2:
  v4: bool = read a1
  branch v4, b3, b4
b3:
  v5: u64 = read a2
  return v5
b4:
  return v2
}
"#,
    );
    wf::module(&m).unwrap();
    crate::extent::share_slots(m.functions[0].body.as_mut().unwrap(), &m.signatures);
    wf::module(&m).unwrap();
}

#[test]
fn well_formedness_rejects_producer_mistakes() {
    let head = "core v1\nsig g0 = (i32, i32) -> (i32)\nsig g1 = (u64) -> (ptr)\nsig g2 = (u64) -> never\nsig g3 = () -> (ptr)\ninst n0 = p@0/m/f [] []\ninst n1 = p@0/m/stop [] []\nfn f0 = n0 g0 {\n";
    let tail = "fn f1 = n1 g2 extern\n";
    let ok =
        "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  return v2\nb1:\n  propagate\n}\n";
    wf::module(&parse(&format!("{head}{ok}{tail}")).unwrap())
        .expect("the base case is well formed");
    // s0 written on b1 or b2, s1 its drop flag, read under the flag after the join.
    let flagged = |arms: &str| {
        format!(
            "slot s0 = i32\nslot s1 = bool\naddr a0 = s0 : i32\naddr a1 = s1 : bool\nb0(v0: i32, v1: i32):\n  live s0\n  live s1\n  address a0\n  address a1\n  v2: bool = const false\n  store a1, v2\n  v3: bool = lt v0, v1\n  branch v3, b1, b2\n{arms}b3:\n  v5: bool = read a1\n  branch v5, b4, b5\nb4:\n  v6: i32 = read a0\n  return v6\nb5:\n  return v0\n}}\n"
        )
    };
    let unguarded = flagged(
        "b1:\n  store a0, v0\n  jump b3\nb2:\n  v4: bool = const true\n  store a1, v4\n  jump b3\n",
    );
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
            "b0(v0: i32, v1: i32):\n  v2: u64 = const 8\n  v3: ptr, v4: u64 = alloc region v2 align 8 charge v2\n  free v3 charge v2\n  return v0\n}\n",
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
        // a catch past a block's first instruction runs after normal code there, so a
        // block entered by a normal edge would catch nothing (FAIL-16)
        (
            "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  return v2\nb1:\n  v3: u64 = const 0\n  v4: ptr = call rt.isolate_catch g3 ()\n  return v0\n}\n",
            "not the block's first instruction",
        ),
        // a handler reached by a normal edge would catch with no failure in the chain
        (
            "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  jump b1\nb1:\n  v3: ptr = call rt.isolate_catch g3 ()\n  return v0\n}\n",
            "a handler entered by an edge",
        ),
        // the entry block is reached by the call, never by a failure
        (
            "b0(v0: i32, v1: i32):\n  v2: ptr = call rt.isolate_catch g3 ()\n  return v0\n}\n",
            "the entry block is a handler",
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
        // Engines read a value or address as computed on the path taken. Each use is
        // defined on every path to it, not just on some path that reaches it.
        // b0 -> b2 skips b1, where v3 is defined.
        (
            "b0(v0: i32, v1: i32):\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  v3: i32 = add v0, v1 fail b3\n  jump b2\nb2:\n  return v3\nb3:\n  propagate\n}\n",
            "b2: v3 is used on a path that does not define it",
        ),
        // b0 -> b2 skips b1, the only evaluation of a0 (the evaluator then reads 0x0)
        (
            "slot s0 = i32\naddr a0 = s0 : i32\nb0(v0: i32, v1: i32):\n  live s0\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  address a0\n  store a0, v0\n  jump b2\nb2:\n  v3: i32 = read a0\n  return v3\n}\n",
            "b2: a0 is used on a path that does not define it",
        ),
        // a failing instruction defines no result on its failure edge
        (
            "b0(v0: i32, v1: i32):\n  v2: i32 = add v0, v1 fail b1\n  return v2\nb1:\n  v3: i32 = add v2, v0 fail b2\n  propagate\nb2:\n  propagate\n}\n",
            "b1: v2 is used on a path that does not define it",
        ),
        // A slot's contents are read only where every path wrote them: engines give
        // an unwritten home's stack bytes (Cranelift), undef (LLVM) or 0 (evaluator).
        // b0 -> b2 skips b1, the only store to s0.
        (
            "slot s0 = i32\naddr a0 = s0 : i32\nb0(v0: i32, v1: i32):\n  live s0\n  address a0\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  store a0, v0\n  jump b2\nb2:\n  v3: i32 = read a0\n  return v3\n}\n",
            "b2: s0 is read on a path that does not write it",
        ),
        // a drop flag set on the arm that does not write s0 guards nothing
        (
            unguarded.as_str(),
            "b4: s0 is read on a path that does not write it",
        ),
        // `dead` ends a slot's storage on every path through it: engines may give the
        // home to another slot from there. b0 -> b1 ends s0 before b2 writes it.
        (
            "slot s0 = i32\naddr a0 = s0 : i32\nb0(v0: i32, v1: i32):\n  live s0\n  address a0\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  dead s0\n  jump b2\nb2:\n  store a0, v1\n  v3: i32 = read a0\n  return v3\n}\n",
            "b2: s0 is used after its storage ends",
        ),
    ];
    // An address evaluated on each arm before the join is defined on every path.
    let joined = "slot s0 = i32\naddr a0 = s0 : i32\nb0(v0: i32, v1: i32):\n  live s0\n  v2: bool = lt v0, v1\n  branch v2, b1, b2\nb1:\n  address a0\n  store a0, v0\n  jump b3\nb2:\n  address a0\n  store a0, v1\n  jump b3\nb3:\n  v3: i32 = read a0\n  return v3\n}\n";
    wf::module(&parse(&format!("{head}{joined}{tail}")).unwrap())
        .expect("an address evaluated on both arms is defined at the join");
    // A drop flag stored true only where s0 is written: its yes edge reads s0.
    let guarded = flagged(
        "b1:\n  store a0, v0\n  v4: bool = const true\n  store a1, v4\n  jump b3\nb2:\n  jump b3\n",
    );
    wf::module(&parse(&format!("{head}{guarded}{tail}")).unwrap())
        .expect("a read under the drop flag of its writes is written");
    for (body, why) in cases {
        let m = parse(&format!("{head}{body}{tail}")).unwrap();
        let err = wf::module(&m).expect_err(why);
        assert!(err.contains(why), "expected `{why}`, got `{err}`");
    }
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
            "core v1\nsig g0 = () -> (u8)\ninst n0 = p@0/m/f [] []\nfn f0 = n0 g0 {{\nb0:\n  v0: u8 = const {c}\n  return v0\n}}\n"
        );
        parse(&t)
    };
    assert!(m("255").is_ok());
    assert!(m("256").is_err());
    assert!(m("-1").is_err());
}

/// Bug: two instances of one generic share a link name (closures or dictionaries left
/// out of the digest, or the boundary between them not encoded).
#[test]
fn instance_identity_separates_closures_and_dictionaries() {
    let path = hirsel_core_ids::DefPath::from_full("p@0/m/f").unwrap();
    let closure = |b: u8| hirsel_core_ids::ContentHash([b; 32]);
    let id = |closures: &[u8], dicts: &[&str]| Identity {
        definition: path.clone(),
        closures: closures.iter().map(|&b| closure(b)).collect(),
        dictionaries: dicts.iter().map(|s| s.to_string()).collect(),
    };
    let ids = [
        id(&[1], &[]),
        id(&[2], &[]),
        id(&[1, 2], &[]),
        id(&[1], &["eq"]),
        id(&[], &["eq"]),
        id(&[], &["eq", "eq"]),
    ];
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
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
        "hirsel-core-net",
        "hirsel-intern",
        "hirsel-guard",
    ];
    let deps = |name: &str| -> Vec<String> {
        // Embed the manifest inputs so the law also runs in a hermetic test sandbox.
        let manifest = match name {
            "hirsel-core-ids" => include_str!("../../hirsel-core-ids/Cargo.toml"),
            "hirsel-core-layout" => include_str!("../../hirsel-core-layout/Cargo.toml"),
            "hirsel-core-ir" => include_str!("../Cargo.toml"),
            "hirsel-core-net" => include_str!("../../hirsel-core-net/Cargo.toml"),
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

use super::*;

/// FAIL-12: a drop whose finalizer fails has cleaned its place before its failure
/// edge, so cleanup on that edge must not drop the place again. This is the shape of
/// the front end's double drops (7827b7a8 reverted, and a failed omitted-field drop
/// that re-dropped its subject): the failure block cleans the same owner once more.
#[test]
fn ownership_rejects_a_drop_repeated_on_its_failure_edge() {
    let module = |cleanup: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
fn f0 = n0 g0 {{
  addr a0 = v1 : t0
b0(v0: u64, v1: ptr):
  address a0
  drop a0 t0 fail b1
  return v0
b1:
{cleanup}  propagate
}}
fn f1 = n1 g1 extern
"#
        )
    };
    wf::module(&load("once", &module(""))).expect("one drop is well formed");
    let err = wf::module(&load("twice", &module("  drop a0 t0\n"))).unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
}

/// A pointer parameter joined from two slots, each begun on its own edge, names
/// one of them on each path: a drop through it on one trip round a loop must not
/// make the other slot, begun afresh on the next trip, read as dropped. This is
/// the shape of `store@1/store/put_impl`'s `match` (a field taken into one slot,
/// `list.new()` into the other). A second drop through it on one trip is still a
/// double drop, and a use through it after a drop of one of its slots is still a use
/// after drop.
#[test]
fn ownership_follows_a_joined_pointer_along_its_edge() {
    let module = |entry: &str, join: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  slot s1 = t0
  addr a0 = s0 : t0
  addr a1 = v1 : t0
  addr a2 = s1 : t0
  addr a3 = v4 : t0
b0(v0: u64, v1: ptr):
  v6: u64 = const 0
  v2: bool = eq v0, v6
  v7: u64 = held_size t0
{entry}  jump b1
b1:
  branch v2, b2, b3
b2:
  live s0
  address a0
  address a1
  copy a0, a1, v7
  v3: ptr = pointer a0
  jump b4(v3)
b3:
  live s1
  address a2
  address a1
  copy a2, a1, v7
  v5: ptr = pointer a2
  jump b4(v5)
b4(v4: ptr):
{join}
b5:
  return v0
}}
fn f1 = n1 g1 extern
"#
        )
    };
    let drop = "  address a3\n  drop a3 t0\n";
    let again = "  branch v2, b1, b5\n";
    let once = format!("{drop}{again}");
    wf::module(&load("loop", &module("", &once))).expect("each trip drops what it began");
    let twice = format!("{drop}  drop a3 t0\n{again}");
    let err = wf::module(&load("twice", &module("", &twice))).unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
    let write = "  live s0\n  address a0\n  address a1\n  copy a0, a1, v7\n";
    let after =
        "  address a0\n  drop a0 t0\n  address a3\n  address a1\n  copy a1, a3, v7\n  jump b5\n";
    let err = wf::module(&load("after", &module(write, after))).unwrap_err();
    assert!(err.contains("is used where it may be dropped"), "{err}");
}

/// OWN-20: a call whose result is a view anchored on an argument leaves its dest
/// borrowing that argument's place, so a use of the dest after the anchor's drop is a
/// use after drop. This is B1 (`break me(mk(3))` before d92bdeec, lowered): the break
/// edge dropped the temporary `mk(3)` in s0 and then read the view `me` left in s1.
/// Without the mark, nothing links s1 to s0.
#[test]
fn ownership_rejects_a_view_used_after_its_anchor_is_dropped() {
    let module = |mark: &str| {
        format!(
            r#"core v1
item i0 = app@1/main/g
type t0 = resource "app@1/main/Mark" drop f4 {{ id: u64 }}
sig g0 = () -> (u64)
sig g1 = (ptr) -> ()
sig g2 = (u64) -> () dest t0
sig g3 = (&view t0) -> () dest {mark}
sig g4 = (&view t0) -> (u64)
inst n0 = app@1/main/g [] []
inst n1 = app@1/main/Mark [] []
inst n2 = app@1/main/mk [] []
inst n3 = app@1/main/me [] []
inst n4 = app@1/main/id_of [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  slot s1 = t0
  addr a0 = s0 : t0
  addr a1 = s1 : t0
  addr a2 = v1 : t0
  addr a3 = v1 : t0
  addr a4 = v1 : t0
  addr a5 = v3 : t0
b0:
  jump b1
b1:
  v0: u64 = const 3
  step
  live s0
  address a0
  call f1 g2 (&a0, v0)
  v1: ptr = pointer a0
  step fail b2
  live s1
  address a1
  address a2
  call f2 g3 (&a1, &a2) fail b2
  v2: ptr = pointer a1
  address a3
  drop a3 t0 fail b4
  jump b5(v2)
b2:
  address a4
  drop a4 t0 fail b3
  jump b3
b3:
  propagate
b4:
  propagate
b5(v3: ptr):
  step
  address a5
  v4: u64 = call f3 g4 (&a5)
  return v4
}}
fn f1 = n2 g2 extern
fn f2 = n3 g3 extern
fn f3 = n4 g4 extern
fn f4 = n1 g1 extern
"#
        )
    };
    wf::module(&load("owned", &module("t0"))).expect("an owned dest borrows nothing");
    let err = wf::module(&load("view", &module("view t0 in (0)"))).unwrap_err();
    assert!(err.contains("s1 is used where it may be dropped"), "{err}");
}

/// A pointer joined from two slots is bound to one of them on each path, and a drop
/// through it drops only that one. This is `store@1/store/admit_claim`'s
/// `match take stored { Some(take fixed) => take fixed, None => take offered }`:
/// `offered` (s1) has drop flag s2, the `None` edge clears it and binds the join to
/// s1, and the `Some` edge binds it to `fixed` (s0) with s2 still set. Dropping the
/// join and then, under s2, `offered` drops each slot once. If the `None` edge leaves
/// s2 set, that second drop is a double drop.
#[test]
fn ownership_drops_a_joined_place_only_where_the_join_is_bound() {
    let module = |clear: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  slot s1 = t0
  slot s2 = bool
  addr a0 = s0 : t0
  addr a1 = v1 : t0
  addr a2 = s1 : t0
  addr a3 = v4 : t0
  addr a4 = s2 : bool
b0(v0: u64, v1: ptr):
  v6: u64 = const 0
  v2: bool = eq v0, v6
  v7: u64 = held_size t0
  live s1
  address a2
  address a1
  copy a2, a1, v7
  v5: ptr = pointer a2
  live s2
  address a4
  v8: bool = const true
  store a4, v8
  branch v2, b1, b2
b1:
  live s0
  address a0
  address a1
  copy a0, a1, v7
  v3: ptr = pointer a0
  jump b3(v3)
b2:
{clear}  jump b3(v5)
b3(v4: ptr):
  address a3
  drop a3 t0
  address a4
  v10: bool = read a4
  branch v10, b4, b5
b4:
  address a2
  drop a2 t0
  jump b5
b5:
  return v0
}}
fn f1 = n1 g1 extern
"#
        )
    };
    let cleared = "  v9: bool = const false\n  address a4\n  store a4, v9\n";
    wf::module(&load("cleared", &module(cleared))).expect("each slot is dropped once");
    let kept = "  v9: bool = const false\n";
    let err = wf::module(&load("kept", &module(kept))).unwrap_err();
    assert!(
        err.contains("drops s1 where it may be dropped already"),
        "{err}"
    );
}

/// Soundness regressions surfaced by the Lean 4 formalization of `wf::own` (lane 3390):
/// 1. Back-edges into entry block `b0` must merge may-drop state into `b0`.
/// 2. A drop-flag read invalidated by a later write before `branch` must not refine edges.
/// 3. Non-`store` writes (`copy`, `move`, etc.) to a drop-flag slot must invalidate it.
/// 4. A partial field write must not re-begin a dropped parent place.
#[test]
fn ownership_rejects_entry_backedge_stale_flag_nonstore_flag_and_partial_write() {
    let head = r#"core v1
type t0 = resource "p@0/m/R" drop f1 { id: u64 }
type t1 = own "p@0/m/P" { a: t0, b: t0 }
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
"#;
    let tail = "fn f1 = n1 g1 extern\n";
    let check = |name: &str, body: &str| {
        let text = format!("{head}fn f0 = n0 g0 {{\n{body}}}\n{tail}");
        wf::module(&load(name, &text))
    };

    // 1. Entry-block back-edge double-drop.
    let err = check(
        "entry_edge",
        r#"  addr a0 = v1 : t0
b0(v0: u64, v1: ptr):
  v2: u64 = const 0
  v3: bool = eq v0, v2
  address a0
  drop a0 t0
  branch v3, b1, b2
b1:
  jump b0(v0, v1)
b2:
  return v0
"#,
    )
    .unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");

    let flag = |mid: &str, yes: &str, no: &str| {
        format!(
            r#"  slot s0 = t0
  slot s1 = bool
  slot s2 = bool
  addr a0 = s0 : t0
  addr a1 = v1 : t0
  addr a2 = s1 : bool
  addr a3 = s2 : bool
b0(v0: u64, v1: ptr):
  v2: u64 = held_size t0
  v3: bool = const true
  v4: bool = const false
  v5: u64 = const 1
  live s0
  address a0
  address a1
  copy a0, a1, v2
  live s1
  address a2
  live s2
  address a3
{mid}  branch v6, b1, b2
b1:
{yes}  jump b3
b2:
{no}  jump b3
b3:
  return v0
"#
        )
    };
    let drop_s0 = "  address a0\n  drop a0 t0\n";

    // 2. Stale flag read across a write must be refused; fresh read stays accepted.
    let err = check(
        "stale_flag_read",
        &flag(
            &format!("  store a2, v3\n  v6: bool = read a2\n{drop_s0}  store a2, v4\n"),
            drop_s0,
            "",
        ),
    )
    .unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
    check(
        "fresh_flag_read",
        &flag(
            &format!("  store a2, v3\n{drop_s0}  store a2, v4\n  v6: bool = read a2\n"),
            drop_s0,
            "",
        ),
    )
    .expect("fresh cleared flag guards the second drop");

    // 3. Non-store write (`copy`) into a flag slot must invalidate its known value.
    let err = check(
        "copy_into_flag",
        &flag(
            &format!(
                "  store a2, v4\n  store a3, v3\n  copy a2, a3, v5\n{drop_s0}  v6: bool = read a2\n"
            ),
            drop_s0,
            "",
        ),
    )
    .unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");

    // 4. Partial write to `s0.0` must not re-begin the dropped parent `s0`.
    let err = check(
        "partial_write",
        r#"  slot s0 = t1
  addr a0 = s0 : t1
  addr a1 = v1 : t1
  addr a2 = s0 field 0 : t0
  addr a3 = s0 field 1 : t0
  addr a4 = a1 field 0 : t0
b0(v0: u64, v1: ptr):
  v2: u64 = held_size t1
  v3: u64 = held_size t0
  live s0
  address a0
  address a1
  copy a0, a1, v2
  drop a0 t1
  address a2
  address a4
  copy a2, a4, v3
  address a3
  drop a3 t0
  return v0
"#,
    )
    .unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
}

/// Ending a local anchor's storage (`dead s0`) invalidates views anchored on it,
/// and returning either a local slot pointer or a dest view anchored on a local slot
/// is refused.
#[test]
fn ownership_rejects_dead_anchor_and_escaping_local_returns() {
    let dead_anchor = r#"core v1
item i0 = app@1/main/g
type t0 = resource "app@1/main/Mark" drop f4 { id: u64 }
sig g0 = () -> (u64)
sig g1 = (ptr) -> ()
sig g2 = (u64) -> () dest t0
sig g3 = (&view t0) -> () dest view t0 in (0)
sig g4 = (&view t0) -> (u64)
inst n0 = app@1/main/g [] []
inst n1 = app@1/main/Mark [] []
inst n2 = app@1/main/mk [] []
inst n3 = app@1/main/me [] []
inst n4 = app@1/main/id_of [] []
fn f0 = n0 g0 {
  slot s0 = t0
  slot s1 = t0
  addr a0 = s0 : t0
  addr a1 = s1 : t0
  addr a2 = v1 : t0
  addr a3 = v3 : t0
b0:
  v0: u64 = const 3
  live s0
  address a0
  call f1 g2 (&a0, v0)
  v1: ptr = pointer a0
  live s1
  address a1
  address a2
  call f2 g3 (&a1, &a2)
  v2: ptr = pointer a1
  dead s0
  jump b1(v2)
b1(v3: ptr):
  address a3
  v4: u64 = call f3 g4 (&a3)
  return v4
}
fn f1 = n2 g2 extern
fn f2 = n3 g3 extern
fn f3 = n4 g4 extern
fn f4 = n1 g1 extern
"#;
    let err = wf::module(&load("dead_anchor", dead_anchor)).unwrap_err();
    assert!(err.contains("s1 is used where it may be dropped"), "{err}");

    let escape_ptr = r#"core v1
sig g0 = () -> (ptr)
inst n0 = p@0/m/leak [] []
fn f0 = n0 g0 {
  slot s0 = u64
  addr a0 = s0 : u64
b0:
  live s0
  address a0
  v0: u64 = const 1
  store a0, v0
  v1: ptr = pointer a0
  return v1
}
"#;
    let err = wf::module(&load("escape_ptr", escape_ptr)).unwrap_err();
    assert!(err.contains("returns a pointer into local slot"), "{err}");

    let escape_dest_view = r#"core v1
type t0 = resource "p@0/m/R" drop f3 { id: u64 }
sig g0 = () -> () dest t0
sig g1 = (u64) -> () dest t0
sig g2 = (&view t0) -> () dest view t0 in (0)
sig g3 = (ptr) -> ()
inst n0 = p@0/m/leak [] []
inst n1 = p@0/m/mk [] []
inst n2 = p@0/m/me [] []
inst n3 = p@0/m/R [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = v0 : t0
  addr a1 = s0 : t0
b0(v0: ptr):
  v1: u64 = const 1
  live s0
  address a1
  call f1 g1 (&a1, v1)
  address a0
  call f2 g2 (&a0, &a1)
  return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
fn f3 = n3 g3 extern
"#;
    let err = wf::module(&load("escape_dest_view", escape_dest_view)).unwrap_err();
    assert!(
        err.contains("returns a view anchored on a local slot"),
        "{err}"
    );
}

#[test]
fn wf_module_with_budget_bounds_scratch_and_deadline() {
    use crate::decode::{DecodeBudget, DecodeLimit, DecodeLimits};
    use std::time::Instant;

    let module = load(
        "budgeted",
        r#"core v1
type t0 = resource "p@0/m/R" drop f1 { id: u64 }
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = v1 : t0
b0(v0: u64, v1: ptr):
  v2: u64 = held_size t0
  live s0
  address a0
  address a1
  copy a0, a1, v2
  drop a0 t0
  dead s0
  return v0
}
fn f1 = n1 g1 extern
"#,
    );
    let limits = DecodeLimits {
        bytes: 65536,
        members: 16,
        slots: 16,
        depth: 16,
        deadline: None,
    };
    let tiny = DecodeBudget::new(DecodeLimits {
        bytes: 16,
        ..limits
    });
    assert_eq!(
        wf::module_with_budget(&module, &tiny.debit()),
        Err(wf::Error::Limit(DecodeLimit::Bytes))
    );
    let expired = DecodeBudget::new(DecodeLimits {
        deadline: Some(Instant::now()),
        ..limits
    });
    assert_eq!(
        wf::module_with_budget(&module, &expired.debit()),
        Err(wf::Error::Limit(DecodeLimit::Deadline))
    );
    let ok = DecodeBudget::new(limits);
    wf::module_with_budget(&module, &ok.debit()).unwrap();
    ok.debit()
        .charge_bytes(limits.bytes)
        .expect("all wf scratch is released");
}

#[test]
fn memmove_requires_plain_operands_and_preserves_plain_sources() {
    let body = |repr: &str, to: &str, from: &str| {
        format!(
            r#"core v1
type t0 = {repr} "p@0/m/Row" {{ n: u64 }}
sig g0 = (ptr, ptr) -> ()
inst n0 = p@0/m/f [] []
fn f0 = n0 g0 {{
  addr a0 = v0 : {to}
  addr a1 = v1 : {from}
b0(v0: ptr, v1: ptr):
  v2: u64 = const 0
  address a0
  address a1
  move a0, a1, v2
  move a0, a1, v2
  return
}}
"#
        )
    };
    wf::module(&load("plain", &body("own", "t0", "t0"))).unwrap();
    for (to, from) in [("u64", "t0"), ("t0", "u64"), ("ptr", "u64")] {
        let err = wf::module(&load("resource", &body("resource", to, from))).unwrap_err();
        assert!(err.contains("move requires plain operands"), "{err}");
    }
}

#[test]
fn mutable_call_checks_the_places_carried_by_block_parameters() {
    let body = |field| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Pair" {{ a: u64, b: u64 }}
sig g0 = (ptr) -> ()
sig g1 = (&mut u64, &view u64) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/observe [] []
fn f0 = n0 g0 {{
  addr a0 = v0 : t0
  addr a1 = a0 field {field} : u64
  addr a2 = v2 : u64
  addr a3 = a0 field 0 : u64
b0(v0: ptr):
  address a0
  address a1
  v1: ptr = pointer a1
  jump b1(v1)
b1(v2: ptr):
  address a3
  address a2
  call f1 g1 (&a3, &a2)
  return
}}
fn f1 = n1 g1 extern
"#
        )
    };
    wf::module(&load("siblings", &body(1))).unwrap();
    let err = wf::module(&load("same", &body(0))).unwrap_err();
    assert!(err.contains("mut place overlaps"), "{err}");
}

#[test]
fn storage_dead_leaves_a_dropped_place_dropped() {
    let body = |before: &str, after: &str| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Empty" {{ }}
sig g0 = (ptr, bool) -> ()
inst n0 = p@0/m/f [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = v0 : t0
b0(v0: ptr, v1: bool):
  v2: u64 = held_size t0
  jump b1
b1:
  live s0
  address a0
  address a1
{before}  drop a0 t0
  dead s0
{after}  branch v1, b1, b2
b2:
  return
}}
"#
        )
    };
    let init = "  copy a0, a1, v2\n";
    wf::module(&load("reuse", &body(init, ""))).unwrap();
    let err = wf::module(&load("dead", &body(init, "  drop a0 t0\n"))).unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
}

#[test]
fn rewriting_an_anchor_ends_the_view_at_the_write() {
    let body = |before: &str, after: &str| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Row" {{ n: u64 }}
sig g0 = (ptr) -> ()
sig g1 = (&view t0) -> () dest view t0 in (0)
sig g2 = (&view t0) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/borrow [] []
inst n2 = p@0/m/observe [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  addr a0 = v0 : t0
  addr a1 = s0 : t0
  addr a2 = a0 field 0 : u64
b0(v0: ptr):
  v1: u64 = const 1
  live s0
  address a0
  address a1
  address a2
  call f1 g1 (&a1, &a0)
{before}  store a2, v1
{after}  return
}}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#
        )
    };
    let observe = "  call f2 g2 (&a1)\n";
    wf::module(&load("before", &body(observe, ""))).unwrap();
    let err = wf::module(&load("after", &body("", observe))).unwrap_err();
    assert!(err.contains("s0 is used where it may be dropped"), "{err}");
}

/// Bug: a block parameter that is a slot on one edge and a heap pointer on another
/// was read as the slot on both: a write through it revived the dropped slot, and a
/// drop through it of a value built field by field was refused.
#[test]
fn a_parameter_that_may_be_a_slot_or_a_heap_pointer_is_a_join() {
    let body = |relive: &str, tail: &str| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Row" {{ n: u64, k: u64 }}
sig g0 = (ptr, bool) -> ()
sig g1 = () -> (ptr)
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/spare [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = v0 : t0
  addr a2 = v5 : t0
  addr a3 = a2 field 0 : u64
  addr a4 = a2 field 1 : u64
b0(v0: ptr, v1: bool):
  v2: u64 = held_size t0
  v6: u64 = const 1
  live s0
  address a0
  address a1
  copy a0, a1, v2
  drop a0 t0
  branch v1, b1, b2
b1:
{relive}  v3: ptr = pointer a0
  jump b3(v3)
b2:
  v4: ptr = call f1 g1 ()
  jump b3(v4)
b3(v5: ptr):
  address a2
  address a3
  address a4
{tail}  return
}}
fn f1 = n1 g1 extern
"#
        )
    };
    let built = "  store a3, v6\n  store a4, v6\n  drop a2 t0\n";
    wf::module(&load("built", &body("  live s0\n", built))).unwrap();
    let revived = "  copy a2, a1, v2\n  drop a0 t0\n";
    let err = wf::module(&load("revived", &body("", revived))).unwrap_err();
    assert!(
        err.contains("drops s0 where it may be dropped already"),
        "{err}"
    );
}

/// Bug (lane 3407): an edge back into the entry block that gave a pointer parameter
/// another pointer left it read as its caller's place alone, so a write through it
/// revived that place, dropped on the round before.
#[test]
fn an_edge_into_the_entry_block_keeps_each_pointer_parameter() {
    let body = |back: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr, ptr, ptr) -> (u64)
sig g1 = (ptr) -> ()
sig g2 = () -> (ptr)
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
inst n2 = p@0/m/spare [] []
fn f0 = n0 g0 {{
  addr a0 = v1 : t0
  addr a1 = v2 : t0
  addr a2 = v3 : t0
b0(v0: u64, v1: ptr, v2: ptr, v3: ptr):
  v4: u64 = const 0
  v5: bool = eq v0, v4
  v6: u64 = held_size t0
  address a0
  address a2
  copy a0, a2, v6
  branch v5, b1, b2
b1:
  drop a0 t0
  v7: ptr = call f2 g2 ()
  jump b0({back})
b2:
  address a1
  drop a1 t0
  return v0
}}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#
        )
    };
    wf::module(&load("same", &body("v0, v1, v2, v3"))).unwrap();
    let err = wf::module(&load("rebound", &body("v0, v7, v1, v3"))).unwrap_err();
    assert!(
        err.contains("an edge into b0 gives pointer parameter v1 v7"),
        "{err}"
    );
}

/// Bugs (lane 3407): a join's pointer remade by `pointer` reached a parameter read
/// as its one slot, so a write through it revived the slot on the heap path; and a
/// join that only forwards its pointer left the next join no state to read, so a
/// use through that one after a drop of one of its slots passed.
#[test]
fn a_join_keeps_its_places_through_pointer_and_through_another_join() {
    let body = |a3: &str, other: &str, mid: &str, fwd: &str, tail: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  slot s1 = t0
  addr a0 = s0 : t0
  addr a1 = v1 : t0
  addr a2 = s1 : t0
  addr a3 = {a3}
  addr a4 = v6 : t0
  addr a5 = s0 : ptr
b0(v0: u64, v1: ptr):
  v8: u64 = const 0
  v2: bool = eq v0, v8
  v7: u64 = held_size t0
  live s0
  address a0
  address a1
  copy a0, a1, v7
  live s1
  address a2
  copy a2, a1, v7
  address a5
  drop a2 t0
  branch v2, b1, b2
b1:
  v3: ptr = pointer a2
  jump b3(v3)
b2:
{other}  jump b3(v5)
b3(v4: ptr):
{mid}  jump b4({fwd})
b4(v6: ptr):
  address a4
{tail}  return v0
}}
fn f1 = n1 g1 extern
"#
        )
    };
    let heap = "  v5: ptr = read a5\n";
    let remade = "  address a3\n  v9: ptr = pointer a3\n";
    let revive = "  copy a4, a1, v7\n  drop a2 t0\n";
    let laundered = body("v4 : t0", heap, remade, "v9", revive);
    let err = wf::module(&load("laundered", &laundered)).unwrap_err();
    assert!(
        err.contains("drops s1 where it may be dropped already"),
        "{err}"
    );
    let slot = "  v5: ptr = pointer a0\n";
    let forwarded = body("s0 : ptr", slot, "", "v4", "  copy a1, a4, v7\n");
    let err = wf::module(&load("forwarded", &forwarded)).unwrap_err();
    assert!(err.contains("is used where it may be dropped"), "{err}");
}

/// Bug (lane 3407): a view read through a join that may be its dest outlived the
/// drop of its anchor, since only the dest itself was marked.
#[test]
fn a_view_reached_through_a_join_ends_with_its_anchor() {
    let body = |before: &str, after: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
sig g2 = (&view t0) -> () dest view t0 in (0)
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
inst n2 = p@0/m/borrow [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  slot s1 = t0
  slot s2 = t0
  addr a0 = s0 : t0
  addr a1 = v1 : t0
  addr a2 = s1 : t0
  addr a3 = s2 : t0
  addr a4 = v4 : t0
b0(v0: u64, v1: ptr):
  v6: u64 = const 0
  v2: bool = eq v0, v6
  v7: u64 = held_size t0
  live s0
  address a0
  address a1
  copy a0, a1, v7
  live s1
  address a2
  call f2 g2 (&a2, &a0)
  live s2
  address a3
  copy a3, a1, v7
  branch v2, b1, b2
b1:
  v3: ptr = pointer a2
  jump b3(v3)
b2:
  v5: ptr = pointer a3
  jump b3(v5)
b3(v4: ptr):
  address a4
{before}  drop a0 t0
{after}  return v0
}}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#
        )
    };
    let read = "  copy a1, a4, v7\n";
    wf::module(&load("before", &body(read, ""))).unwrap();
    let err = wf::module(&load("after", &body("", read))).unwrap_err();
    assert!(err.contains("is used where it may be dropped"), "{err}");
}

/// Bug (store's `bucket_cache_samples`): a value chosen among slots (one join) moves
/// into a second join with its drop flag cleared; cleanup drops the second, then the
/// first under the flag. The drop through the second marked the first dropped whatever
/// the flag, refusing the guarded drop. With the flag left set the value drops twice.
#[test]
fn a_drop_through_a_join_follows_the_flag_of_the_join_moved_into_it() {
    let body = |clear: &str| {
        format!(
            r#"core v1
type t0 = resource "p@0/m/R" drop f1 {{ id: u64 }}
sig g0 = (u64, ptr) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/R [] []
fn f0 = n0 g0 {{
  slot s0 = t0
  slot s1 = t0
  slot s2 = t0
  slot s3 = bool
  addr a0 = s0 : t0
  addr a1 = v1 : t0
  addr a2 = s1 : t0
  addr a3 = s2 : t0
  addr a4 = v4 : t0
  addr a5 = v6 : t0
  addr a6 = s3 : bool
b0(v0: u64, v1: ptr):
  v8: u64 = const 0
  v2: bool = eq v0, v8
  v7: u64 = held_size t0
  v9: bool = const true
  v10: bool = const false
  address a0
  address a1
  address a2
  address a3
  address a6
  branch v2, b1, b2
b1:
  live s0
  copy a0, a1, v7
  v3: ptr = pointer a0
  jump b3(v3)
b2:
  live s1
  copy a2, a1, v7
  v5: ptr = pointer a2
  jump b3(v5)
b3(v4: ptr):
  address a4
  live s3
  store a6, v9
  branch v2, b4, b5
b4:
{clear}  jump b6(v4)
b5:
  live s2
  copy a3, a1, v7
  v11: ptr = pointer a3
  jump b6(v11)
b6(v6: ptr):
  address a5
  drop a5 t0
  v12: bool = read a6
  branch v12, b7, b8
b7:
  drop a4 t0
  jump b8
b8:
  return v0
}}
fn f1 = n1 g1 extern
"#
        )
    };
    wf::module(&load("moved", &body("  store a6, v10\n"))).unwrap();
    let err = wf::module(&load("kept", &body(""))).unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
}

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

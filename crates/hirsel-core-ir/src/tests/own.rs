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
/// double drop.
#[test]
fn ownership_follows_a_joined_pointer_along_its_edge() {
    let module = |again: &str| {
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
  jump b1
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
  address a3
  drop a3 t0
{again}  branch v2, b1, b5
b5:
  return v0
}}
fn f1 = n1 g1 extern
"#
        )
    };
    wf::module(&load("loop", &module(""))).expect("each trip drops what it began");
    let err = wf::module(&load("twice", &module("  drop a3 t0\n"))).unwrap_err();
    assert!(err.contains("may be dropped already"), "{err}");
}

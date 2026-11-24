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

use super::*;

/// A returnable variant (`returnable.rs`) passes a leaf wait its child word at
/// record byte 8 and a `mut` result homed at a later byte of the same record:
/// the shape of `std@1/task/cancel_join`'s `join_rec` call, which wf refused
/// as `a mut place overlaps argument 0` in every release build that reached it.
/// The child word is `[8, 16)` and a `u64` home at `at` is `[at, at + 8)`, so
/// by hand the home at 16 shares no byte with it, and the homes at 12 and 4
/// share bytes 12..16 and 8..12.
#[test]
fn mutable_call_compares_byte_windows_of_one_record() {
    let body = |at: u64| {
        format!(
            r#"core v1
sig g0 = (ptr) -> ()
sig g1 = (ptr, &mut u64) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/park [] []
fn f0 = n0 g0 {{
  addr a0 = v0 byte v1 : u64
  addr a1 = v0 byte v3 : u8
  addr a2 = v4 : u64
b0(v0: ptr):
  v1: u64 = const 8
  address a0
  v2: ptr = pointer a0
  v3: u64 = const {at}
  address a1
  v4: ptr = pointer a1
  address a2
  call f1 g1 (v2, &a2)
  return
}}
fn f1 = n1 g1 extern
"#
        )
    };
    wf::module(&load("apart", &body(16))).expect("disjoint record windows");
    for at in [12, 4] {
        let err = wf::module(&load("shared", &body(at))).unwrap_err();
        assert!(
            err.contains("a2: a mut place overlaps argument 0"),
            "{at}: {err}"
        );
    }
}

/// The view a variant passes a callee may be a field of a record homed at one
/// record byte while the callee's `mut` result is homed at another: the shape
/// of `store@1/store/export_copy`'s call, refused as `a mut place overlaps
/// argument 1`. Field `b` of a `Pair` homed at 16 lies inside `[16, 32)`, so
/// by hand a `u64` result at 32 or at 8 shares no byte with it, and one at 24
/// shares all of `b`.
#[test]
fn mutable_call_keeps_a_field_inside_its_byte_window() {
    let body = |at: u64| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Pair" {{ a: u64, b: u64 }}
sig g0 = (ptr) -> ()
sig g1 = (&mut u64, &view u64) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/observe [] []
fn f0 = n0 g0 {{
  addr a0 = v0 byte v1 : u8
  addr a1 = v2 : t0
  addr a2 = a1 field 1 : u64
  addr a3 = v0 byte v3 : u8
  addr a4 = v4 : u64
b0(v0: ptr):
  v1: u64 = const 16
  address a0
  v2: ptr = pointer a0
  v3: u64 = const {at}
  address a3
  v4: ptr = pointer a3
  address a1
  address a2
  address a4
  call f1 g1 (&a4, &a2)
  return
}}
fn f1 = n1 g1 extern
"#
        )
    };
    for at in [32, 8] {
        wf::module(&load("apart", &body(at))).unwrap_or_else(|e| panic!("{at}: {e}"));
    }
    let err = wf::module(&load("shared", &body(24))).unwrap_err();
    assert!(err.contains("a4: a mut place overlaps argument 1"), "{err}");
}

/// A pointer block parameter whose edges carry record pieces at bytes 32 and
/// 48 points at one of them: the shape of `std@1/wsock/server_with_site_report`'s
/// call, refused as `a mut place overlaps argument 3`. A `u64` through it lies
/// inside `[32, 56)`, so by hand a `u64` result at 8 or at 56 shares no byte
/// with it, and one at 48 is the second piece itself.
#[test]
fn mutable_call_follows_byte_windows_through_a_join() {
    let body = |at: u64| {
        format!(
            r#"core v1
sig g0 = (ptr) -> ()
sig g1 = (&mut u64, &view u64) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/observe [] []
fn f0 = n0 g0 {{
  addr a0 = v0 byte v1 : u8
  addr a1 = v0 byte v3 : u8
  addr a2 = v0 byte v5 : u8
  addr a3 = v6 : u64
  addr a4 = v8 : u64
b0(v0: ptr):
  v1: u64 = const 32
  address a0
  v2: ptr = pointer a0
  v3: u64 = const 48
  address a1
  v4: ptr = pointer a1
  v5: u64 = const {at}
  address a2
  v6: ptr = pointer a2
  v7: bool = const true
  branch v7, b1, b2
b1:
  jump b3(v2)
b2:
  jump b3(v4)
b3(v8: ptr):
  address a3
  address a4
  call f1 g1 (&a3, &a4)
  return
}}
fn f1 = n1 g1 extern
"#
        )
    };
    for at in [8, 56] {
        wf::module(&load("apart", &body(at))).unwrap_or_else(|e| panic!("{at}: {e}"));
    }
    let err = wf::module(&load("shared", &body(48))).unwrap_err();
    assert!(err.contains("a3: a mut place overlaps argument 1"), "{err}");
}

/// A view a call homes at one record byte dangles once its anchor is written,
/// and a place homed at another record byte is still whole: the shape of
/// `std@1/wsock/flush`, refused as `parameter 4[..] is used where it may be
/// dropped`. The view is `[16, 32)`, so by hand a `u64` read at 32 or at 8
/// touches none of it, and one at 24 reads its second word.
#[test]
fn a_dangling_view_spoils_only_its_byte_window() {
    let body = |at: u64| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Pair" {{ a: u64, b: u64 }}
sig g0 = (&mut t0, ptr) -> ()
sig g1 = (&view t0) -> () dest view t0 in (0)
sig g2 = (&mut t0) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/lend [] []
inst n2 = p@0/m/poke [] []
fn f0 = n0 g0 {{
  addr a0 = v0 : t0
  addr a1 = v1 byte v2 : u8
  addr a2 = v3 : t0
  addr a3 = v1 byte v4 : u8
  addr a4 = v5 : u64
b0(v0: ptr, v1: ptr):
  v2: u64 = const 16
  address a1
  v3: ptr = pointer a1
  v4: u64 = const {at}
  address a3
  v5: ptr = pointer a3
  address a0
  address a2
  call f1 g1 (&a2, &a0)
  call f2 g2 (&a0)
  address a4
  v6: u64 = read a4
  return
}}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
"#
        )
    };
    for at in [32, 8] {
        wf::module(&load("apart", &body(at))).unwrap_or_else(|e| panic!("{at}: {e}"));
    }
    let err = wf::module(&load("shared", &body(24))).unwrap_err();
    assert!(
        err.contains("a4: parameter 1[16..32]:t0[..] is used where it may be dropped"),
        "{err}"
    );
}

/// Two fields of one record homed at a record byte are two places: the shape of
/// `std@1/object/renewal_loop`'s call, refused as `a mut place overlaps argument
/// 1`. By hand field `a` of a `Pair` is its bytes 0..8 and field `b` its bytes
/// 8..16, so `&mut a` and `&mut b` share none, and `&mut a` and the record's
/// first word read as a `u64` share all of `a`.
#[test]
fn mutable_call_tells_fields_of_one_homed_record_apart() {
    let body = |other: &str| {
        format!(
            r#"core v1
type t0 = own "p@0/m/Pair" {{ a: u64, b: u64 }}
sig g0 = (ptr) -> ()
sig g1 = (&mut u64, &mut u64) -> ()
inst n0 = p@0/m/f [] []
inst n1 = p@0/m/swap [] []
fn f0 = n0 g0 {{
  addr a0 = v0 byte v1 : u8
  addr a1 = v2 : t0
  addr a2 = a1 field 0 : u64
  addr a3 = {other} : u64
b0(v0: ptr):
  v1: u64 = const 16
  address a0
  v2: ptr = pointer a0
  address a1
  address a2
  address a3
  call f1 g1 (&a2, &a3)
  return
}}
fn f1 = n1 g1 extern
"#
        )
    };
    wf::module(&load("apart", &body("a1 field 1"))).expect("two fields");
    let err = wf::module(&load("shared", &body("v2"))).unwrap_err();
    assert!(err.contains("a2: a mut place overlaps"), "{err}");
}

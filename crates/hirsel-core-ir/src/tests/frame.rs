use super::*;

#[test]
fn frame_refunds_consume_only_the_admitted_charge() {
    let fixture = r#"core v1
sig g0 = () -> ()
inst n0 = p@0/m/f [] []
fn f0 = n0 g0 {
  slot s0 = u64
  slot s1 = u64
  addr a0 = s0 : u64
  addr a1 = s1 : u64
b0:
  live s0
  live s1
  address a0
  address a1
  v0: u64 = const 8
  v1: u64 = const 16
  v2: bool, v3: u64 = frame_admit a0 charge v0
  branch v2, b1, b3
b1:
  v4: bool, v5: u64 = frame_admit a0 charge v1 replace
  branch v4, b2, b2
b2:
  frame_refund a0
  return
b3:
  return
}
"#;
    let module = parse(fixture).unwrap();
    wf::module(&module).expect("either growth outcome leaves exactly one obligation");

    let wrong_amount = fixture.replace("frame_refund a0", "frame_refund v1");
    assert!(
        parse(&wrong_amount).is_err(),
        "a refund cannot name an amount"
    );

    for (refund, expected) in [
        ("", "exit loses a live frame obligation"),
        (
            "frame_refund a0\n  frame_refund a0",
            "refund requires a live frame obligation",
        ),
        ("frame_refund a1", "refund requires a live frame obligation"),
        (
            "step\n  frame_refund a0",
            "implicit failure loses a live frame obligation",
        ),
    ] {
        let malformed = parse(&fixture.replace("frame_refund a0", refund)).unwrap();
        let rejection = wf::module(&malformed).unwrap_err();
        assert!(rejection.contains(expected), "{rejection}");
    }
}

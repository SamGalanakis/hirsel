use super::*;

#[test]
fn compact_preserves_the_frame_obligation_storage() {
    let mut module = hirsel_core_ir::text::parse(
        r#"core v1
sig g0 = () -> ()
inst n0 = app@1/main/f [] []
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
  v1: bool, v2: u64 = frame_admit a1 charge v0
  branch v1, b1, b1
b1:
  v3: bool = frame_active a1
  branch v3, b2, b3
b2:
  frame_refund a1
  return
b3:
  return
}
"#,
    )
    .unwrap();
    let mut body = module.functions[0].body.take().unwrap();
    let aliases: Vec<_> = (0..body.values.len()).map(|v| ValueId(v as u32)).collect();
    compact(
        &module,
        &mut body,
        &aliases,
        &vec![false; aliases.len()],
        &[false; 2],
        true,
    );
    let frames = body.frame_obligations();
    assert_eq!(frames.len(), 1);
    let storage = frames.first().unwrap();
    assert_eq!(
        body.addresses[storage.index()].base,
        AddressBase::Slot(SlotId(1))
    );
    module.functions[0].body = Some(body);
    hirsel_core_ir::wf::module(&module).unwrap();
}

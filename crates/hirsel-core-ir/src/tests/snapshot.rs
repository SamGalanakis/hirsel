use crate::release_recipe::{Backend, GrantBinding, GrantKind, Launch};
use crate::snapshot::Snapshot;
use crate::{FuncId, StepFree, source, text};
use hirsel_core_layout::{Layout, Target};
use serde_json::Value;

const TEXT: &str = "core v1
item i0 = app@1/main/run
type t0 = own \"Pair\" { left: seq u8, right: seq u8 }
sig g0 = (u64) -> (u64)
inst n0 = app@1/main/run [] []
inst n1 = app@1/main/first [] []
inst n2 = app@1/main/second [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = call f1 g0 (v0) at i0 4..5
  jump b1(v1)
b1(v2: u64):
  return v2
}
fn f1 = n1 g0 {
b0(v0: u64):
  return v0
}
fn f2 = n2 g0 {
b0(v0: u64):
  v1: u64 = const 9
  return v1
}
fn f3 = n1 g0 {
b0(v0: u64):
  return v0
}
";

/// Lost or altered layout rows, code operands and source positions are all
/// observable even when both the original and the damaged wire decode.
#[test]
fn checked_snapshot_preserves_recipes_control_flow_and_sites() {
    let mut module = text::parse(TEXT).unwrap();
    module.step_free.insert(
        FuncId(1),
        StepFree {
            body: FuncId(3),
            dispatch: true,
        },
    );
    module
        .code_variants
        .insert(FuncId(3), hirsel_core_ids::ContentHash([7; 32]));
    module.external_call_sites = true;
    let sources = source::module_sources(&module);
    let launch = Launch {
        backend: Backend::Cranelift,
        target: Target::X86_64Linux.triple().into(),
        root_limit: 4096,
        n_place: 512,
        exit_status: false,
        grants: vec![GrantBinding {
            ordinal: 0,
            kind: GrantKind::Args,
            values: vec!["example".into()],
            archive: None,
            policy: None,
        }],
    };
    let capture = || {
        Snapshot::capture(
            module.clone(),
            vec![FuncId(0)],
            Target::X86_64Linux,
            Some(launch.clone()),
            &sources,
        )
        .unwrap()
    };
    let mut bytes = Vec::new();
    capture().write(&mut bytes).unwrap();
    let mut second = Vec::new();
    capture().write(&mut second).unwrap();
    assert_eq!(bytes, second);

    let decoded = Snapshot::read(bytes.as_slice()).unwrap();
    let expected_text = text::print(&module);
    assert_eq!(decoded.print(), expected_text);
    assert_eq!(decoded.module, module);
    assert_eq!(decoded.launch.as_ref(), Some(&launch));
    let Layout::Fixed(layout) = &decoded.layout.recipes[0].layout else {
        panic!("Pair must have a fixed layout");
    };
    assert_eq!(layout.offsets, [vec![0, 24]]);
    let drops = &decoded.layout.recipes[0].drop.as_ref().unwrap().fields[0];
    assert_eq!(
        drops.iter().map(|field| field.offset).collect::<Vec<_>>(),
        [24, 0]
    );
    let annex = decoded.manifest.definitions[0].as_ref().unwrap();
    assert_eq!(annex.instance.definition.to_string(), "app@1/main/run");
    assert_eq!(annex.source.instructions[0].as_ref().unwrap().column, 5);
    let normal = crate::normal::encode(&module, FuncId(0));
    assert_eq!(annex.body, normal.hash());
    let canonical =
        crate::normal::decode(&normal.bytes, &crate::decode::Debit::unlimited()).unwrap();
    assert_eq!(
        canonical
            .identity(FuncId(0), &crate::decode::Debit::unlimited())
            .unwrap(),
        annex.instance
    );

    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    let decode = |damaged: &Value| {
        let bytes = serde_json::to_vec(damaged).unwrap();
        Snapshot::read(bytes.as_slice()).unwrap()
    };

    let mut damaged = wire.clone();
    damaged["layout"]["recipes"][0]["layout"]["Fixed"]["offsets"][0][1] = 25.into();
    assert_ne!(decode(&damaged).layout, decoded.layout, "recipe offset");

    let mut damaged = wire.clone();
    damaged["layout"]["recipes"][0]["drop"]["fields"][0]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    assert_ne!(decode(&damaged).layout, decoded.layout, "drop order");

    let mut damaged = wire.clone();
    let body = &mut damaged["module"]["functions"][0]["body"];
    body["calls"][0]["callee"]["Direct"] = 2.into();
    assert_ne!(decode(&damaged).print(), expected_text, "callee");

    let mut damaged = wire.clone();
    let body = &mut damaged["module"]["functions"][0]["body"];
    let arg = body["edges"][0]["args"]["start"].as_u64().unwrap() as usize;
    body["pools"]["values"][arg] = 0.into();
    assert_ne!(decode(&damaged).print(), expected_text, "edge argument");

    let mut damaged = wire;
    damaged["module"]["functions"][0]["body"]["inst_anchors"][0]["start"] = 3.into();
    assert_ne!(decode(&damaged).print(), expected_text, "site");
}

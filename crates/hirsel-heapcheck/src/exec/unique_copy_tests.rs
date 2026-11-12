use super::*;
use crate::build::{Layouts, fixture_unit, path};
use crate::program::NoOperations;
use hirsel_lir::layout_types::TargetSpec;
use hirsel_lir::stage::{Ready, StageKind};

#[test]
fn unique_inline_copy_accepts_only_initialized_absent_environments() {
    let mut layouts = Layouts::new(TargetSpec::x86_64_unknown_linux_gnu());
    let callable = layouts.callable(None);
    let outer = layouts.record(&[callable]);
    let unit = layouts.unit();
    let counted = layouts.block(unit);
    let (target, layouts) = layouts.finish();
    let mut body = fixture_unit::<Ready>(path("unique_inline"));
    body.header.target = target.digest();
    body.header.stage = StageKind::Ready;
    let program = Program {
        target,
        layouts,
        units: vec![body],
        statics: vec![],
    };
    let config = Config::default();
    let mut ops = NoOperations;
    let mut machine = Machine::new(&program, &config, &mut ops);
    let frame = machine.frame(0, Role::Entry, None);
    let mut valid = Extent::new(16);
    valid.put_cell(0, 8, Cell::Code(Some(0)));
    valid.write_bytes(8, &0u64.to_le_bytes());
    machine
        .copy_tree_image(
            &frame,
            (None, false),
            outer,
            &mut valid.clone(),
            &mut vec![],
        )
        .unwrap();
    for (layout, mut forged) in [
        (outer, {
            let mut image = valid.clone();
            image.write_bytes(8, &1u64.to_le_bytes());
            image
        }),
        (outer, {
            let mut image = Extent::new(16);
            image.put_cell(0, 8, Cell::Code(Some(0)));
            image
        }),
        (outer, {
            let mut image = Extent::new(16);
            image.write_bytes(0, &[0; 16]);
            image
        }),
        (outer, {
            let mut image = valid.clone();
            image.put_cell(8, 8, Cell::Code(Some(0)));
            image
        }),
        (counted, {
            let mut image = Extent::new(8);
            image.write_bytes(0, &0u64.to_le_bytes());
            image
        }),
    ] {
        let error = machine
            .copy_tree_image(&frame, (None, false), layout, &mut forged, &mut vec![])
            .unwrap_err();
        assert_eq!(error.kind, V::ForgedPointer);
    }
}

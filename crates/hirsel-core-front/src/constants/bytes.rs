use super::*;
use crate::layout::{ShapeNames, Shapes, carried};
use hirsel_core_layout::{Carried, compute};

impl ConstCx<'_, '_> {
    pub(super) fn bytes_of(&mut self, n: NodeId) -> Option<ConstValue> {
        let ty = self.p.lower_ty(self.at, n, &[]);
        if mentions_error(&self.p.types, ty) {
            return None;
        }
        let Some(size) = const_logical_size(self.p, ty) else {
            return self.fail(n, "the type has no representable logical size");
        };
        Some(ConstValue {
            ty: scalar(Scalar::U64),
            data: ConstData::Bits(size),
        })
    }
}

/// Constants read the same held layout as the body emitter, without emitting IR.
fn const_logical_size(p: &mut Program, ty: TyId) -> Option<u64> {
    match carried(&p.decls, &p.types, ty)? {
        Carried::Nothing => return Some(0),
        Carried::Value(Ty::Scalar(s)) => return Some(s.size()),
        Carried::Value(Ty::Vector(v)) => return Some(v.size()),
        Carried::Value(Ty::Ptr) => return Some(hirsel_core_layout::POINTER),
        Carried::Address => {}
    }
    let mut shapes = vec![];
    let row = Shapes {
        decls: &p.decls,
        types: &mut p.types,
        shapes: &mut shapes,
        of: &mut ShapeNames::default(),
        module: false,
        hooks: vec![],
        codecs: &crate::StructuralCodecs::NONE,
        tuple_codecs: vec![],
        key_codecs: vec![],
    }
    .shape(ty)?;
    let recipes = compute(&shapes).ok()?;
    hirsel_core_layout::charge::held(shapes[row.index()].repr, &recipes[row.index()])
}

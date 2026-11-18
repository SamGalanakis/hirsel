use crate::{Inst, LayoutQuery, Module};
use hirsel_core_layout::{FieldTy, Layout, Recipe, Shape, TypeRef, field_size};
use std::borrow::Cow;

impl LayoutQuery {
    pub fn map_types(&mut self, mut map: impl FnMut(TypeRef) -> TypeRef) {
        match self {
            Self::FieldSize(field) | Self::FrameCapacity(field) => field.map_refs(&mut map),
            Self::HeldSize(ty) | Self::BlockSize { ty, .. } => ty.0 = map(TypeRef(ty.0)).0,
        }
    }

    pub fn native(&self, shapes: &[Shape], recipes: &[Recipe]) -> u64 {
        match self {
            Self::FieldSize(field) => field_size(shapes, recipes, field).0,
            Self::HeldSize(ty) if shapes[ty.index()].repr == hirsel_core_layout::Repr::Region => {
                hirsel_core_layout::POINTER
            }
            Self::HeldSize(ty) => field_size(shapes, recipes, &FieldTy::Inline(TypeRef(ty.0))).0,
            Self::BlockSize { ty, prefix } => {
                let Layout::Fixed(layout) = &recipes[ty.index()].layout else {
                    unreachable!("block size of a compact type");
                };
                let size = layout.size.saturating_add(*prefix);
                if *prefix == 0 {
                    size
                } else {
                    size.saturating_add(7) & !7
                }
            }
            Self::FrameCapacity(field) => {
                let stride = field_size(shapes, recipes, field).0.max(1);
                let capacity = hirsel_core_layout::FRAME_LIST_BYTES / stride;
                if capacity >= 4 { capacity } else { 0 }
            }
        }
    }
}

/// Resolve physical operands only after the checked module has been published
/// and its revision computed. The existing constant-based optimizations then
/// work with the same values as the evaluator.
pub fn resolve<'a>(module: &'a Module, recipes: &[Recipe]) -> Cow<'a, Module> {
    if !module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .any(|b| {
            b.insts
                .iter()
                .any(|i| matches!(i, Inst::LayoutConst { .. }))
        })
    {
        return Cow::Borrowed(module);
    }
    let mut native = module.clone();
    for body in native.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        for inst in &mut body.insts {
            if let Inst::LayoutConst { result, query } = inst {
                *inst = Inst::Const {
                    result: *result,
                    bits: query.native(&module.types, recipes),
                };
            }
        }
    }
    Cow::Owned(native)
}

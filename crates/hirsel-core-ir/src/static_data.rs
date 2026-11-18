use crate::{Static, StaticValue};
use hirsel_core_layout::{FieldTy, Layout, Recipe, Shape, TypeRef, case_word, field_size};
use std::borrow::Cow;

impl Static {
    /// Bytes interpreted as literal data, including function descriptor identities.
    pub fn literal_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes { bytes, .. } => Some(bytes),
            Self::Aggregate { .. } => None,
        }
    }

    /// Materialize a read-only object using the same recipes as field projection.
    pub fn native<'a>(&'a self, shapes: &[Shape], recipes: &[Recipe]) -> (u32, Cow<'a, [u8]>) {
        match self {
            Self::Bytes { align, bytes } => (*align, Cow::Borrowed(bytes)),
            Self::Aggregate { ty, value } => {
                let Layout::Fixed(layout) = &recipes[ty.index()].layout else {
                    unreachable!("an aggregate constant has a fixed layout");
                };
                let mut bytes = vec![0; layout.size as usize];
                write(
                    value,
                    &FieldTy::Inline(TypeRef(ty.0)),
                    &mut bytes,
                    shapes,
                    recipes,
                );
                (layout.align as u32, Cow::Owned(bytes))
            }
        }
    }
}

fn write(
    value: &StaticValue,
    ty: &FieldTy,
    bytes: &mut [u8],
    shapes: &[Shape],
    recipes: &[Recipe],
) {
    match (value, ty) {
        (StaticValue::Bits(bits), _) => {
            let size = field_size(shapes, recipes, ty).0 as usize;
            bytes[..size].copy_from_slice(&bits.to_le_bytes()[..size]);
        }
        (StaticValue::Fields { case, fields }, FieldTy::Inline(t)) => {
            let shape = &shapes[t.index()];
            if let Some(array @ FieldTy::Array(..)) = &shape.held {
                write(value, array, bytes, shapes, recipes);
                return;
            }
            let Layout::Fixed(layout) = &recipes[t.index()].layout else {
                unreachable!("an aggregate constant has a fixed layout");
            };
            if let Some(case) = case {
                let tag = layout.tag.unwrap();
                if let Some(word) = case_word(&tag, *case) {
                    let at = tag.offset as usize;
                    let width = tag.width as usize;
                    bytes[at..at + width].copy_from_slice(&word.to_le_bytes()[..width]);
                }
            }
            let offsets = &layout.offsets[case.unwrap_or(0) as usize];
            for ((value, field), &offset) in
                fields.iter().zip(shape.fields(*case).unwrap()).zip(offsets)
            {
                write(
                    value,
                    &field.ty,
                    &mut bytes[offset as usize..],
                    shapes,
                    recipes,
                );
            }
        }
        (StaticValue::Fields { fields, .. }, FieldTy::Array(item, _)) => {
            let stride = field_size(shapes, recipes, item).0 as usize;
            for (i, value) in fields.iter().enumerate() {
                write(value, item, &mut bytes[i * stride..], shapes, recipes);
            }
        }
        _ => unreachable!("a checked constant has its type's shape"),
    }
}

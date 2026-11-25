use crate::{Static, StaticValue};
use hirsel_core_layout::{FieldTy, Layout, RecipeRow, Row, TypeRef, case_word, field_size};
use std::borrow::Cow;

impl Static {
    /// Bytes interpreted as literal data, including function descriptor identities.
    pub fn literal_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes { bytes, .. } => Some(bytes),
            Self::Aggregate { .. } | Self::Computed { .. } => None,
        }
    }

    /// Materialize a read-only object using the same recipes as field projection.
    pub fn native<'a>(&'a self, shapes: &[Row], recipes: &[RecipeRow]) -> (u32, Cow<'a, [u8]>) {
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
            Self::Computed { constant, .. } => {
                unreachable!("computed constant {constant} reached emission unbound")
            }
        }
    }
}

/// A computed constant's value (LANG-20) as its slot binds it: the static a
/// reader's [`Static::Computed`] of type `ty` becomes.
pub fn bound_static(ty: &FieldTy, value: StaticValue) -> Static {
    match (ty, value) {
        (FieldTy::Inline(t), value) => Static::Aggregate {
            ty: crate::TypeId(t.0),
            value,
        },
        (FieldTy::Scalar(s), StaticValue::Bits(bits)) => {
            let size = s.size() as usize;
            Static::Bytes {
                align: size as u32,
                bytes: bits.to_le_bytes()[..size].to_vec(),
            }
        }
        _ => unreachable!("a computed constant is a scalar or a declared row"),
    }
}

impl crate::Module {
    /// The computed constants this module's statics name, in static order and
    /// without repeats: a body's constant slots ([`Static::Computed`]).
    pub fn constant_slots(&self) -> Vec<hirsel_core_ids::DefPath> {
        let mut out: Vec<hirsel_core_ids::DefPath> = Vec::new();
        for data in &self.statics {
            if let Static::Computed { constant, .. } = data
                && !out.contains(constant)
            {
                out.push(constant.clone());
            }
        }
        out
    }

    /// Bind every constant slot to its value; the first constant `value` has
    /// none for, if any, is left unbound and returned.
    pub fn bind_constants(
        &mut self,
        mut value: impl FnMut(&hirsel_core_ids::DefPath) -> Option<StaticValue>,
    ) -> Result<(), hirsel_core_ids::DefPath> {
        for data in &mut self.statics {
            if let Static::Computed { ty, constant } = data {
                let Some(v) = value(constant) else {
                    return Err(constant.clone());
                };
                *data = bound_static(ty, v);
            }
        }
        Ok(())
    }
}

/// The value a value of type `ty` holds in `bytes`, laid out by `recipes`: the
/// inverse of [`Static::native`].
pub fn read_value(
    ty: &FieldTy,
    bytes: &[u8],
    shapes: &[Row],
    recipes: &[RecipeRow],
) -> StaticValue {
    match ty {
        FieldTy::Inline(t) => {
            let shape = &shapes[t.index()];
            if let Some(array @ FieldTy::Array(..)) = &shape.held {
                return read_value(array, bytes, shapes, recipes);
            }
            let Layout::Fixed(layout) = &recipes[t.index()].layout else {
                unreachable!("a computed constant has a fixed layout");
            };
            let case = match &layout.tag {
                Some(tag) => {
                    let at = tag.offset as usize;
                    let mut word = [0u8; 8];
                    word[..tag.width as usize].copy_from_slice(&bytes[at..at + tag.width as usize]);
                    Some(hirsel_core_layout::case_of(tag, u64::from_le_bytes(word)))
                }
                None => matches!(shape.body, hirsel_core_layout::Body::Variant(_)).then_some(0),
            };
            let offsets = &layout.offsets[case.unwrap_or(0) as usize];
            let fields = shape
                .fields(case)
                .unwrap()
                .iter()
                .zip(offsets)
                .map(|(field, &offset)| {
                    read_value(&field.ty, &bytes[offset as usize..], shapes, recipes)
                })
                .collect();
            StaticValue::Fields { case, fields }
        }
        FieldTy::Array(item, n) => {
            let stride = field_size(shapes, recipes, item).0 as usize;
            StaticValue::Fields {
                case: None,
                fields: (0..*n as usize)
                    .map(|i| read_value(item, &bytes[i * stride..], shapes, recipes))
                    .collect(),
            }
        }
        _ => {
            let size = field_size(shapes, recipes, ty).0 as usize;
            let mut word = [0u8; 8];
            word[..size].copy_from_slice(&bytes[..size]);
            StaticValue::Bits(u64::from_le_bytes(word))
        }
    }
}

/// PROG-10 value bytes: each scalar little-endian at its width, fields in
/// declaration order, an array element by element, and a value with cases as
/// its case index (32-bit little-endian) followed by that case's fields.
pub fn value_bytes(
    ty: &FieldTy,
    value: &StaticValue,
    shapes: &[Row],
    recipes: &[RecipeRow],
    out: &mut Vec<u8>,
) {
    match (ty, value) {
        (FieldTy::Inline(t), _) if matches!(&shapes[t.index()].held, Some(FieldTy::Array(..))) => {
            let array = shapes[t.index()].held.as_ref().unwrap();
            value_bytes(array, value, shapes, recipes, out);
        }
        (FieldTy::Inline(t), StaticValue::Fields { case, fields }) => {
            if let Some(case) = case {
                out.extend_from_slice(&case.to_le_bytes());
            }
            let shape = &shapes[t.index()];
            for (field, value) in shape.fields(*case).unwrap().iter().zip(fields) {
                value_bytes(&field.ty, value, shapes, recipes, out);
            }
        }
        (FieldTy::Array(item, _), StaticValue::Fields { fields, .. }) => {
            for value in fields {
                value_bytes(item, value, shapes, recipes, out);
            }
        }
        (ty, StaticValue::Bits(bits)) => {
            let size = field_size(shapes, recipes, ty).0 as usize;
            out.extend_from_slice(&bits.to_le_bytes()[..size]);
        }
        _ => unreachable!("a computed constant has its type's shape"),
    }
}

fn write(
    value: &StaticValue,
    ty: &FieldTy,
    bytes: &mut [u8],
    shapes: &[Row],
    recipes: &[RecipeRow],
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

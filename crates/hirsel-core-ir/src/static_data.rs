use crate::Static;
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::{FieldTy, Layout, RecipeRow, Row, TypeRef, case_word, field_size};
use std::borrow::Cow;

/// A plain constant in declaration order. The enclosing type supplies each
/// field's type, and the engine supplies its native layout.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StaticValue {
    Bits(u64),
    Fields {
        case: Option<u32>,
        fields: Vec<StaticValue>,
    },
    /// A static computed constant's value (LANG-20): the bytes of a `String` or
    /// `FrozenBytes`. Its slot binds it as [`Static::Bytes`] holding the length,
    /// 64-bit little-endian, then the bytes; no aggregate holds one.
    Bytes(Vec<u8>),
}

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
        (_, StaticValue::Bytes(bytes)) => Static::Bytes {
            align: 8,
            bytes: static_bytes(&bytes),
        },
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

/// A static computed constant's value bytes (PROG-10), which are also its slot's
/// bytes: the length, 64-bit little-endian, then the bytes.
pub fn static_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
    out
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
        (_, StaticValue::Bytes(bytes)) => out.extend_from_slice(&static_bytes(bytes)),
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

/// The value hash domain (PROG-10).
pub const VALUE_HASH_DOMAIN: &str = "hirsel.code-value.v1";
/// The closure hash domain of a constant type that reaches no row: a scalar or `()`.
pub const SCALAR_CLOSURE_DOMAIN: &str = "hirsel.core-closure.v1";
/// The hash domain of a value object, the code-store object that carries a
/// computed constant's value to a host assembling from a release recipe.
pub const VALUE_OBJECT_DOMAIN: &str = "hirsel.code-value-object.v1";
const VALUE_MAGIC: &[u8] = b"HIRVALUE\0\x01\x00";

/// The closure hash `c` of a computed constant held in a slot of type `ty`
/// in `module` (PROG-10): its row's closure hash, or a scalar's name hash.
/// None for a type no constant slot holds.
pub fn value_closure(module: &crate::Module, ty: &FieldTy) -> Option<ContentHash> {
    match ty {
        FieldTy::Inline(t) if t.index() < module.types.len() => {
            Some(module.closure(crate::TypeId(t.0)).hash)
        }
        FieldTy::Scalar(s) => Some(ContentHash::of_parts(
            SCALAR_CLOSURE_DOMAIN,
            [s.name().as_bytes()],
        )),
        _ => None,
    }
}

/// A value's PROG-10 value hash `H("hirsel.code-value.v1", c ‖ v)`, with `c`
/// its type's closure hash and `v` its value bytes.
pub fn value_hash(
    closure: ContentHash,
    ty: &FieldTy,
    value: &StaticValue,
    shapes: &[Row],
    recipes: &[RecipeRow],
) -> ContentHash {
    let mut bytes = Vec::new();
    value_bytes(ty, value, shapes, recipes, &mut bytes);
    ContentHash::of_parts(VALUE_HASH_DOMAIN, [closure.0.as_slice(), &bytes])
}

/// Whether `value` has the shape a slot of type `ty` binds, so that
/// [`value_bytes`] and [`bound_static`] read it: a scalar's bits, a fixed
/// row's fields of one of its cases, an array's elements, or a static
/// constant's bytes in a row.
pub fn fits(ty: &FieldTy, value: &StaticValue, shapes: &[Row], recipes: &[RecipeRow]) -> bool {
    match (ty, value) {
        (FieldTy::Scalar(_), StaticValue::Bits(_)) => true,
        (FieldTy::Inline(t), _) if t.index() >= shapes.len() || t.index() >= recipes.len() => false,
        (FieldTy::Inline(_), StaticValue::Bytes(_)) => true,
        (FieldTy::Inline(t), _) => {
            let shape = &shapes[t.index()];
            if let Some(array @ FieldTy::Array(..)) = &shape.held {
                return fits(array, value, shapes, recipes);
            }
            let (Layout::Fixed(layout), StaticValue::Fields { case, fields }) =
                (&recipes[t.index()].layout, value)
            else {
                return false;
            };
            let Some(declared) = shape.fields(*case) else {
                return false;
            };
            layout.offsets.len() > case.unwrap_or(0) as usize
                && declared.len() == fields.len()
                && declared
                    .iter()
                    .zip(fields)
                    .all(|(field, value)| fits(&field.ty, value, shapes, recipes))
        }
        (FieldTy::Array(item, n), StaticValue::Fields { case: None, fields }) => {
            fields.len() as u64 == *n && fields.iter().all(|v| fits(item, v, shapes, recipes))
        }
        _ => false,
    }
}

/// A value object's bytes: the value hash its readers' groups bind, then the
/// value as a tree (`0` bits as a 64-bit little-endian word; `1` an optional
/// case index and the fields; `2` the bytes, counted).
pub fn encode_value_object(hash: ContentHash, value: &StaticValue) -> Vec<u8> {
    fn tree(value: &StaticValue, out: &mut Vec<u8>) {
        match value {
            StaticValue::Bits(bits) => {
                out.push(0);
                out.extend_from_slice(&bits.to_le_bytes());
            }
            StaticValue::Fields { case, fields } => {
                out.push(1);
                match case {
                    None => out.push(0),
                    Some(case) => {
                        out.push(1);
                        out.extend_from_slice(&case.to_le_bytes());
                    }
                }
                crate::normal::uleb(out, fields.len() as u64);
                for field in fields {
                    tree(field, out);
                }
            }
            StaticValue::Bytes(bytes) => {
                out.push(2);
                crate::normal::uleb(out, bytes.len() as u64);
                out.extend_from_slice(bytes);
            }
        }
    }
    let mut bytes = VALUE_MAGIC.to_vec();
    bytes.extend_from_slice(&hash.0);
    tree(value, &mut bytes);
    bytes
}

/// Decode a value object completely: the value hash it claims and its value.
/// The claim is checked where a slot binds it, against the slot's type.
pub fn decode_value_object(
    bytes: &[u8],
    budget: &crate::decode::Debit<'_>,
) -> crate::decode::DecodeResult<(ContentHash, StaticValue)> {
    use crate::normal::wire::{Read, Wire, invalid};
    fn tree(input: &mut Read<'_, '_>) -> crate::decode::DecodeResult<StaticValue> {
        match u8::read(input)? {
            0 => Ok(StaticValue::Bits(u64::read(input)?)),
            1 => {
                let case = Option::<u32>::read(input)?;
                let count = input.count()?;
                let fields = input.read_vec(count, tree)?;
                Ok(StaticValue::Fields { case, fields })
            }
            2 => {
                let count = input.count()?;
                input.budget.allocation::<u8>(count)?;
                Ok(StaticValue::Bytes(input.take(count)?.to_vec()))
            }
            _ => Err(invalid("unknown value tag")),
        }
    }
    let mut input = Read::new(bytes, VALUE_MAGIC, budget)?;
    let hash = ContentHash::read(&mut input)?;
    let value = input.nested(tree)?;
    input.end()?;
    Ok((hash, value))
}

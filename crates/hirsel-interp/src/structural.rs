//! Iterative structural equality, order and hashing (MEM-3).
use crate::{
    Value,
    scalar::{self, ScalarError},
};
use hirsel_syntax::BinaryOp;
use std::cmp::Ordering;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructuralError {
    InvalidValue,
}
fn children(v: &Value) -> Option<&[Option<Value>]> {
    match v {
        Value::Map { entries: f, .. }
        | Value::Set { entries: f, .. }
        | Value::Sequence { elements: f, .. } => Some(f),
        Value::Tuple(f)
        | Value::Box(f)
        | Value::ArenaBox { fields: f, .. }
        | Value::Record { fields: f, .. }
        | Value::Variant { fields: f, .. }
        | Value::Distinct { fields: f, .. } => Some(f),
        _ => None,
    }
}
fn identity(a: &Value, b: &Value) -> Result<Ordering, StructuralError> {
    match (a, b) {
        (Value::Map { .. }, Value::Map { .. })
        | (Value::Set { .. }, Value::Set { .. })
        | (Value::Tuple(_), Value::Tuple(_))
        | (Value::Box(_) | Value::ArenaBox { .. }, Value::Box(_) | Value::ArenaBox { .. }) => {
            Ok(Ordering::Equal)
        }
        (Value::Record { definition: a, .. }, Value::Record { definition: b, .. })
        | (Value::Distinct { definition: a, .. }, Value::Distinct { definition: b, .. })
            if a == b =>
        {
            Ok(Ordering::Equal)
        }
        (Value::Variant { variant: a, .. }, Value::Variant { variant: b, .. }) => Ok(a.cmp(b)),
        (Value::Sequence { kind: a, .. }, Value::Sequence { kind: b, .. }) if a == b => {
            Ok(Ordering::Equal)
        }
        _ => Err(StructuralError::InvalidValue),
    }
}
/// None is IEEE unordered; language equality never uses Rust bit equality.
pub fn compare(a: &Value, b: &Value) -> Result<Option<Ordering>, StructuralError> {
    compare_at(a, b, false)
}
fn compare_at(
    a: &Value,
    b: &Value,
    equality_only: bool,
) -> Result<Option<Ordering>, StructuralError> {
    enum Work<'a> {
        Pair(&'a Value, &'a Value),
        Length(usize, usize),
    }
    let mut work = vec![Work::Pair(a, b)];
    while let Some(w) = work.pop() {
        match w {
            Work::Length(a, b) => {
                if a != b {
                    return Ok(Some(a.cmp(&b)));
                }
            }
            Work::Pair(a, b) => {
                // SEM-5: an `ordered of` token (an Instant's ticks, a Level's
                // `authority * 256 + ordinal` word) orders by its unsigned word. The
                // checker established one domain, so no identity check happens here.
                if let (Value::Handle { kind: ka, id: x }, Value::Handle { kind: kb, id: y }) =
                    (a, b)
                {
                    if ka != kb || !matches!(&**ka, "Instant" | "Level") {
                        return Err(StructuralError::InvalidValue);
                    }
                    if x != y {
                        return Ok(Some(x.cmp(y)));
                    }
                    continue;
                }
                if matches!(
                    a,
                    Value::Resource { .. }
                        | Value::ResourceVariant { .. }
                        | Value::Closure { .. }
                        | Value::FunctionReference(_)
                        | Value::Handle { .. }
                ) || matches!(
                    b,
                    Value::Resource { .. }
                        | Value::ResourceVariant { .. }
                        | Value::Closure { .. }
                        | Value::FunctionReference(_)
                        | Value::Handle { .. }
                ) {
                    return Err(StructuralError::InvalidValue);
                }
                if matches!((a, b), (Value::Set { .. }, Value::Set { .. })) {
                    // Canonical encoding proves equality even for hand-built
                    // heterogeneous sets; its byte order is not structural order.
                    if equality_only {
                        let order = encoding(a)?.cmp(&encoding(b)?);
                        if order != Ordering::Equal {
                            return Ok(Some(order));
                        }
                        continue;
                    }
                    // SEM-5, spec #93: canonical structural element order,
                    // independent of insertion order and encoding byte artifacts.
                    let (Value::Set { entries: ac, .. }, Value::Set { entries: bc, .. }) = (a, b)
                    else {
                        unreachable!()
                    };
                    fn canonical(fields: &[Option<Value>]) -> Result<Vec<&Value>, StructuralError> {
                        let mut values: Vec<_> = fields.iter().filter_map(Option::as_ref).collect();
                        let mut failure = None;
                        values.sort_by(|a, b| match compare_at(a, b, false) {
                            Ok(Some(order)) => order,
                            Ok(None) => {
                                failure = Some(StructuralError::InvalidValue);
                                Ordering::Equal
                            }
                            Err(error) => {
                                failure = Some(error);
                                Ordering::Equal
                            }
                        });
                        if let Some(error) = failure {
                            return Err(error);
                        }
                        Ok(values)
                    }
                    let (ac, bc) = (canonical(ac)?, canonical(bc)?);
                    let mut order = Ordering::Equal;
                    for (a, b) in ac.iter().zip(&bc) {
                        order = compare_at(a, b, false)?.ok_or(StructuralError::InvalidValue)?;
                        if order != Ordering::Equal {
                            break;
                        }
                    }
                    if order == Ordering::Equal {
                        order = ac.len().cmp(&bc.len());
                    }
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                    continue;
                }
                if let (Some(ac), Some(bc)) = (children(a), children(b)) {
                    let ord = identity(a, b)?;
                    if ord != Ordering::Equal {
                        return Ok(Some(ord));
                    }
                    let keyed = matches!(a, Value::Map { .. });
                    let live = |v: &&Option<Value>| !keyed || v.is_some();
                    let an = ac.iter().filter(live).count();
                    let bn = bc.iter().filter(live).count();
                    work.push(Work::Length(an, bn));
                    let first = work.len();
                    for (a, b) in ac.iter().filter(live).zip(bc.iter().filter(live)) {
                        work.push(Work::Pair(
                            a.as_ref().ok_or(StructuralError::InvalidValue)?,
                            b.as_ref().ok_or(StructuralError::InvalidValue)?,
                        ));
                    }
                    work[first..].reverse();
                } else {
                    let result = scalar::binary(BinaryOp::Eq, a.clone(), b.clone())
                        .map_err(|_| StructuralError::InvalidValue)?;
                    if result == Value::Bool(true) {
                        continue;
                    }
                    let result = scalar::binary(BinaryOp::Lt, a.clone(), b.clone())
                        .map_err(|_| StructuralError::InvalidValue)?;
                    if result == Value::Bool(true) {
                        return Ok(Some(Ordering::Less));
                    }
                    let result = scalar::binary(BinaryOp::Gt, a.clone(), b.clone())
                        .map_err(|_| StructuralError::InvalidValue)?;
                    if result == Value::Bool(true) {
                        return Ok(Some(Ordering::Greater));
                    }
                    return Ok(None);
                }
            }
        }
    }
    Ok(Some(Ordering::Equal))
}
pub fn binary(op: BinaryOp, a: &Value, b: &Value) -> Result<Value, StructuralError> {
    let order = compare_at(a, b, matches!(op, BinaryOp::Eq | BinaryOp::Ne))?;
    use BinaryOp::*;
    Ok(Value::Bool(match op {
        Eq => order == Some(Ordering::Equal),
        Ne => order != Some(Ordering::Equal),
        Lt => order == Some(Ordering::Less),
        Le => matches!(order, Some(Ordering::Less | Ordering::Equal)),
        Gt => order == Some(Ordering::Greater),
        Ge => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
        _ => return Err(StructuralError::InvalidValue),
    }))
}
/// Stable length-framed encoding, using SipHash-1-3. Equal keys hash identically.
pub fn hash(value: &Value, seed: u64) -> Result<u64, StructuralError> {
    Ok(hirsel_kernels::siphash::siphash13(
        &encoding(value)?,
        seed,
        0,
    ))
}
fn encoding(value: &Value) -> Result<Vec<u8>, StructuralError> {
    enum Work<'a> {
        Enter(&'a Value),
        Finish(Vec<u8>, usize, bool),
    }
    let mut work = vec![Work::Enter(value)];
    let mut output: Vec<Vec<u8>> = Vec::new();
    while let Some(item) = work.pop() {
        match item {
            Work::Finish(mut prefix, count, sort) => {
                let start = output.len() - count;
                let mut children = output.split_off(start);
                if sort {
                    children.sort();
                }
                for child in children {
                    framed(&mut prefix, &child);
                }
                output.push(prefix);
            }
            Work::Enter(v) => {
                let tag = match v {
                    Value::Unit => 0,
                    Value::Bool(_) => 1,
                    Value::Integer(_) => 2,
                    Value::String(_) | Value::ArenaString { .. } => 3,
                    Value::Bytes(_) | Value::ArenaBytes { .. } => 4,
                    Value::Tuple(_) => 5,
                    Value::Variant { .. } => 6,
                    Value::Record { .. } => 7,
                    Value::Distinct { .. } => 8,
                    Value::Box(_) | Value::ArenaBox { .. } => 9,
                    Value::Sequence { .. } => 10,
                    Value::Map { .. } => 11,
                    Value::Set { .. } => 12,
                    Value::Char(_) => 13,
                    _ => return Err(StructuralError::InvalidValue),
                };
                let mut bytes = vec![tag];
                match v {
                    Value::Bool(b) => bytes.push(u8::from(*b)),
                    Value::Char(c) => bytes.extend_from_slice(&u32::from(*c).to_le_bytes()),
                    Value::Integer(i) => {
                        bytes.extend_from_slice(&i.width().to_le_bytes());
                        bytes.push(u8::from(i.is_signed()));
                        bytes.extend_from_slice(&i.bits().to_le_bytes());
                    }
                    Value::String(s) | Value::ArenaString { text: s, .. } => {
                        framed(&mut bytes, s.as_bytes())
                    }
                    Value::Bytes(b) | Value::ArenaBytes { bytes: b, .. } => framed(&mut bytes, b),
                    Value::Variant { variant, .. } => {
                        bytes.extend_from_slice(&variant.to_le_bytes())
                    }
                    Value::Record { definition, .. } | Value::Distinct { definition, .. } => {
                        let definition = definition.path();
                        framed(&mut bytes, definition.0.package.name.as_bytes());
                        framed(&mut bytes, definition.0.package.version.as_bytes());
                        bytes.extend_from_slice(&(definition.0.modules.len() as u64).to_le_bytes());
                        for m in &definition.0.modules {
                            framed(&mut bytes, m.as_bytes());
                        }
                        framed(&mut bytes, definition.0.name.as_bytes());
                    }
                    _ => (),
                }
                if let Some(fields) = children(v) {
                    let keyed = matches!(v, Value::Map { .. } | Value::Set { .. });
                    let count = fields.iter().filter(|v| !keyed || v.is_some()).count();
                    bytes.extend_from_slice(&(count as u64).to_le_bytes());
                    work.push(Work::Finish(bytes, count, matches!(v, Value::Set { .. })));
                    for f in fields.iter().rev().filter(|v| !keyed || v.is_some()) {
                        work.push(Work::Enter(
                            f.as_ref().ok_or(StructuralError::InvalidValue)?,
                        ));
                    }
                } else {
                    output.push(bytes);
                }
            }
        }
    }
    output.pop().ok_or(StructuralError::InvalidValue)
}
fn framed(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_le_bytes());
    out.extend_from_slice(b);
}
impl From<ScalarError> for StructuralError {
    fn from(_: ScalarError) -> Self {
        Self::InvalidValue
    }
}

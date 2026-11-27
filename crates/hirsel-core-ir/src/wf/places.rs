//! An address's steps: each index is an integer, a carrier word is a leaf, and
//! a field step lands on a place whose declared row the walk from its root
//! knows, as both native engines walk it. A record field typed as a holder
//! (`arc t`, a box) is a counted or boxed pointer, not `t`'s row: a field of
//! the payload follows a `deref` step, or a place rooted at the payload itself.

use super::{Ctx, R, need};
use crate::*;
use hirsel_core_layout::{FieldTy, Repr};

/// What the walk knows about the place an address prefix names.
#[derive(Clone)]
pub(super) enum At {
    Declared(TypeId),
    Pointer(TypeId),
    Counted(TypeId),
    Array(FieldTy),
    Other,
}

fn held(m: &Module, t: TypeId) -> At {
    match m.types.get(t.index()).map(|r| &r.repr) {
        Some(Repr::Resource | Repr::Region) => At::Pointer(t),
        _ => At::Declared(t),
    }
}

fn field_at(m: &Module, f: &FieldTy) -> At {
    match f {
        FieldTy::Inline(t) => held(m, TypeId(t.0)),
        FieldTy::Box(t) => At::Pointer(TypeId(t.0)),
        FieldTy::Rc(t) | FieldTy::Arc(t) | FieldTy::Shared(t) => At::Counted(TypeId(t.0)),
        FieldTy::Array(e, _) => At::Array((**e).clone()),
        _ => At::Other,
    }
}

/// Check address `i`'s steps and record what its place is in `ats[i]`; every
/// base address precedes it (checked by the caller).
pub(super) fn address(c: &Ctx, ats: &mut Vec<At>, i: usize, a: &AddressInfo) -> R {
    let (m, b) = (c.m, c.b);
    let steps = a.steps.get(&b.pools.steps).ok_or("steps out of pool")?;
    let declared = |p: PlaceTy| match p {
        PlaceTy::Declared(t) => At::Declared(t),
        _ => At::Other,
    };
    let mut at = match a.base {
        AddressBase::Slot(s) => match b.slots[s.index()] {
            PlaceTy::Declared(t) => held(m, t),
            _ => At::Other,
        },
        AddressBase::Address(x) => ats[x.index()].clone(),
        AddressBase::Value(_) if steps.is_empty() => declared(a.ty),
        AddressBase::Value(_) => At::Other,
    };
    for (k, s) in steps.iter().enumerate() {
        at = match *s {
            Step::Index(v) | Step::Element(v) | Step::Byte(v) => {
                need(c.ty(v)?.int().is_some(), || {
                    format!("a{i}: index is not an integer")
                })?;
                match (*s, at) {
                    (Step::Byte(_), _) => declared(a.ty),
                    (Step::Index(_), _) => match a.ty {
                        PlaceTy::Declared(t) => held(m, t),
                        _ => At::Other,
                    },
                    (_, At::Array(e)) => field_at(m, &e),
                    _ => return Err(format!("a{i}: element step on a place that is no array")),
                }
            }
            // FORM #113: a carrier word is a leaf of its word's type.
            Step::Seq(w) => {
                need(
                    k + 1 == steps.len() && a.ty == PlaceTy::Value(w.ty()),
                    || format!("a{i}: a seq word must be the last step, typed as the word"),
                )?;
                At::Other
            }
            Step::Deref => match at {
                At::Pointer(t) => At::Declared(t),
                At::Counted(t) => held(m, t),
                _ => At::Other,
            },
            Step::Field(f) | Step::CaseField { field: f, .. } => {
                let case = match *s {
                    Step::CaseField { case, .. } => Some(case),
                    _ => None,
                };
                let At::Declared(t) = at else {
                    return Err(format!("a{i}: field step on a place with no declared row"));
                };
                let field = m
                    .types
                    .get(t.index())
                    .and_then(|r| r.fields(case))
                    .and_then(|fs| fs.get(f as usize))
                    .ok_or_else(|| format!("a{i}: t{} has no such field", t.0))?;
                field_at(m, &field.ty)
            }
        };
    }
    ats.push(at);
    Ok(())
}

pub(super) fn check_byte_backing(place: AllocPlace, align: u64, edge: Option<&FieldTy>) -> R {
    if let Some(required) = match place {
        AllocPlace::ByteHead | AllocPlace::ByteTail(_) => Some(8),
        AllocPlace::FileBuffer | AllocPlace::FilePartner(_) => Some(4096),
        _ => None,
    } {
        need(
            align == required
                && matches!(edge, Some(FieldTy::Seq(item)) if **item == FieldTy::Scalar(Scalar::U8)),
            || format!("byte backing requires a {required}-aligned byte sequence"),
        )?;
    }
    Ok(())
}

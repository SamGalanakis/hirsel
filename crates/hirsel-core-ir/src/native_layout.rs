use crate::{Inst, LayoutQuery, Loan, Module, Param, PlaceTy, Signature, TypeId};
use hirsel_core_layout::{
    Body, FieldTy, Handle, Layout, RecipeRow, Repr, Row, Scalar, Tag, Ty, TypeRef, field_size,
};
use std::borrow::Cow;

pub mod copies;

impl LayoutQuery {
    pub fn map_types(&mut self, mut map: impl FnMut(TypeRef) -> TypeRef) {
        match self {
            Self::FieldSize(field) | Self::FrameCapacity(field) => field.map_refs(&mut map),
            Self::HeldSize(ty) | Self::BlockSize { ty, .. } => ty.0 = map(TypeRef(ty.0)).0,
            Self::Target => {}
        }
    }

    pub fn native(&self, shapes: &[Row], recipes: &[RecipeRow]) -> u64 {
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
            Self::Target => unreachable!("a targeted selection is retained before layout"),
        }
    }
}

/// Resolve physical operands only after the checked module has been published
/// and its revision computed. The existing constant-based optimizations then
/// work with the same values as the evaluator.
pub fn resolve<'a>(mut module: Cow<'a, Module>, recipes: &[RecipeRow]) -> Cow<'a, Module> {
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
        return module;
    }
    let native = module.to_mut();
    for body in native.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        for inst in &mut body.insts {
            if let Inst::LayoutConst { result, query } = inst {
                *inst = Inst::Const {
                    result: *result,
                    bits: query.native(&native.types, recipes),
                };
            }
        }
    }
    module
}

/// A fixed record carried in the internal native ABI by its scalar leaves.
/// The offsets describe its original place, including any intervening padding.
#[derive(Clone, Copy)]
pub struct ScalarRecord {
    pub size: u64,
    fields: [(u64, Ty); 2],
    len: usize,
}

impl ScalarRecord {
    pub fn fields(&self) -> &[(u64, Ty)] {
        &self.fields[..self.len]
    }
}

/// A `core.Result` destination whose `Ok` payload is at most two scalar leaves.
/// A private CoreIR body returns that payload in registers with an outcome byte
/// ([`OUTCOME_OK`], [`OUTCOME_RAISED`], [`OUTCOME_OTHER`]) and writes any other case
/// through a cold pointer instead of its caller's destination. Leaf offsets are
/// within the whole result; `ok.size` is its size.
#[derive(Clone, Copy)]
pub struct OkResult {
    pub ok: ScalarRecord,
    pub align: u64,
    pub tag: Tag,
    pub case: u32,
}

/// The `Ok` case is in the returned leaves.
pub const OUTCOME_OK: u8 = 0;
/// A raised failure (the status byte's failure value).
pub const OUTCOME_RAISED: u8 = 1;
/// Another case is in the cold slot.
pub const OUTCOME_OTHER: u8 = 2;

/// Classify native call boundaries using the same recipes as field projection.
pub struct CallLayouts {
    records: Vec<Option<ScalarRecord>>,
    ok_results: Vec<Option<OkResult>>,
}

/// The scalar leaves of fields at `(offset, field)`, at most two, or `None` when
/// one of them is not a scalar.
fn scalar_leaves<'a>(
    shapes: &'a [Row],
    recipes: &[RecipeRow],
    size: u64,
    fields: impl DoubleEndedIterator<Item = (u64, &'a FieldTy)>,
) -> Option<ScalarRecord> {
    let mut record = ScalarRecord {
        size,
        fields: [(0, Ty::Ptr); 2],
        len: 0,
    };
    let mut pending: Vec<(u64, &FieldTy)> = fields.rev().collect();
    while let Some((at, field)) = pending.pop() {
        let ty = match field {
            FieldTy::Scalar(s) => Ty::Scalar(*s),
            FieldTy::Box(_)
            | FieldTy::Rc(_)
            | FieldTy::Arc(_)
            | FieldTy::Shared(_)
            | FieldTy::Fn => Ty::Ptr,
            FieldTy::Handle(Handle::Sender | Handle::Receiver) => return None,
            FieldTy::Handle(_) => Ty::Scalar(Scalar::U64),
            FieldTy::Inline(id) => {
                let nested = &shapes[id.index()];
                if nested.repr == Repr::Resource {
                    Ty::Ptr
                } else {
                    let (Body::Record(fields), Layout::Fixed(layout)) =
                        (&nested.body, &recipes[id.index()].layout)
                    else {
                        return None;
                    };
                    pending.extend(
                        fields
                            .iter()
                            .zip(&layout.offsets[0])
                            .rev()
                            .map(|(field, &offset)| (at + offset, &field.ty)),
                    );
                    continue;
                }
            }
            FieldTy::View(inner) => {
                pending.push((at, inner));
                continue;
            }
            FieldTy::Array(_, 0) => continue,
            FieldTy::Array(element, len) => {
                let stride = field_size(shapes, recipes, element).0;
                if stride == 0 {
                    continue;
                }
                // Each nonempty element needs at least one scalar.
                if *len > (record.fields.len() - record.len) as u64 {
                    return None;
                }
                pending.extend((0..*len).rev().map(|i| (at + i * stride, &**element)));
                continue;
            }
            _ => return None,
        };
        if record.len == record.fields.len() {
            return None;
        }
        record.fields[record.len] = (at, ty);
        record.len += 1;
    }
    Some(record)
}

impl CallLayouts {
    pub fn new(shapes: &[Row], recipes: &[RecipeRow]) -> Self {
        let mut records = vec![None; shapes.len()];
        let mut ok_results = vec![None; shapes.len()];
        for (i, shape) in shapes.iter().enumerate() {
            if matches!(shape.repr, Repr::Resource | Repr::Region) {
                continue;
            }
            let Layout::Fixed(layout) = &recipes[i].layout else {
                continue;
            };
            match &shape.body {
                Body::Record(fields) => {
                    if !(1..=16).contains(&layout.size) {
                        continue;
                    }
                    let fields = fields
                        .iter()
                        .zip(&layout.offsets[0])
                        .map(|(field, &at)| (at, &field.ty));
                    records[i] = scalar_leaves(shapes, recipes, layout.size, fields)
                        .filter(|record| record.len != 0);
                }
                Body::Variant(cases) => {
                    // `pub type Result[T, E] = Ok(T) | Error(E)` (core/core.hirsel).
                    if !shape.name.starts_with("core@1/core/Result[")
                        || cases.len() != 2
                        || cases[0].name != "Ok"
                    {
                        continue;
                    }
                    let Some(tag) = layout.tag else {
                        continue;
                    };
                    let fields = cases[0]
                        .fields
                        .iter()
                        .zip(&layout.offsets[0])
                        .map(|(field, &at)| (at, &field.ty));
                    ok_results[i] =
                        scalar_leaves(shapes, recipes, layout.size, fields).map(|ok| OkResult {
                            ok,
                            align: layout.align,
                            tag,
                            case: 0,
                        });
                }
            }
        }
        Self {
            records,
            ok_results,
        }
    }

    pub fn parameter(&self, param: Param) -> Option<ScalarRecord> {
        match param {
            Param::Address {
                place: PlaceTy::Declared(ty),
                loan: Loan::View | Loan::Take,
            } => self.record(ty),
            _ => None,
        }
    }

    pub fn result(&self, signature: &Signature) -> Option<ScalarRecord> {
        // Source signatures use either a declared destination or immediate
        // results. Explicit CoreIR multi-result signatures keep their ABI.
        if signature.results.is_empty() {
            signature.dest.and_then(|ty| self.record(ty))
        } else {
            None
        }
    }

    pub fn record(&self, ty: TypeId) -> Option<ScalarRecord> {
        self.records[ty.index()]
    }

    /// The private register convention of a body's `Result` destination; its
    /// public symbol, declarations and indirect calls keep the destination.
    pub fn ok_result(&self, signature: &Signature) -> Option<OkResult> {
        if signature.results.is_empty() {
            signature.dest.and_then(|ty| self.ok_results[ty.index()])
        } else {
            None
        }
    }
}

use crate::{Inst, LayoutQuery, Loan, Module, Param, PlaceTy, Signature, TypeId};
use hirsel_core_layout::{
    Body, FieldTy, Handle, Layout, Recipe, Repr, Scalar, Shape, Ty, TypeRef, field_size,
};
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

/// Classify native call boundaries using the same recipes as field projection.
pub struct CallLayouts {
    records: Vec<Option<ScalarRecord>>,
}

impl CallLayouts {
    pub fn new(shapes: &[Shape], recipes: &[Recipe]) -> Self {
        let mut records = vec![None; shapes.len()];
        let mut pending = Vec::new();
        for (i, shape) in shapes.iter().enumerate() {
            if matches!(shape.repr, Repr::Resource | Repr::Region) {
                continue;
            }
            let (Body::Record(fields), Layout::Fixed(layout)) = (&shape.body, &recipes[i].layout)
            else {
                continue;
            };
            if !(1..=16).contains(&layout.size) {
                continue;
            }
            let mut record = ScalarRecord {
                size: layout.size,
                fields: [(0, Ty::Ptr); 2],
                len: 0,
            };
            pending.clear();
            pending.extend(
                fields
                    .iter()
                    .zip(&layout.offsets[0])
                    .rev()
                    .map(|(field, &at)| (at, &field.ty)),
            );
            let accepted = loop {
                let Some((at, field)) = pending.pop() else {
                    break true;
                };
                let ty = match field {
                    FieldTy::Scalar(s) => Ty::Scalar(*s),
                    FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) | FieldTy::Fn => Ty::Ptr,
                    FieldTy::Handle(Handle::Sender | Handle::Receiver) => break false,
                    FieldTy::Handle(_) => Ty::Scalar(Scalar::U64),
                    FieldTy::Inline(id) => {
                        let nested = &shapes[id.index()];
                        if nested.repr == Repr::Resource {
                            Ty::Ptr
                        } else {
                            let (Body::Record(fields), Layout::Fixed(layout)) =
                                (&nested.body, &recipes[id.index()].layout)
                            else {
                                break false;
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
                            break false;
                        }
                        pending.extend((0..*len).rev().map(|i| (at + i * stride, &**element)));
                        continue;
                    }
                    _ => break false,
                };
                if record.len == record.fields.len() {
                    break false;
                }
                record.fields[record.len] = (at, ty);
                record.len += 1;
            };
            if accepted && record.len != 0 {
                records[i] = Some(record);
            }
        }
        Self { records }
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
}

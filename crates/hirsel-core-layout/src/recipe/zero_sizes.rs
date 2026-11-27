use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ZeroSizes(HashMap<TypeRef, bool>);

/// Whether a field of type `ty` has size 0, read from the shapes alone (a list's
/// element need not be computed before its holder): a record whose fields all have
/// size 0, or an array of none or of such. A variant has a tag; anything else holds
/// a scalar, a pointer or a carrier.
impl ZeroSizes {
    pub(super) fn field(&mut self, shapes: &[Row], ty: &FieldTy) -> bool {
        match ty {
            FieldTy::Inline(t) => {
                if let Some(&zero) = self.0.get(t) {
                    return zero;
                }
                // An inline cycle does not prove zero size.
                self.0.insert(*t, false);
                let shape = &shapes[t.index()];
                let zero = !matches!(shape.repr, Repr::Resource | Repr::Compact(_))
                    && matches!(&shape.body, Body::Record(fields)
                        if fields.iter().all(|f| self.field(shapes, &f.ty)));
                self.0.insert(*t, zero);
                zero
            }
            FieldTy::Array(e, n) => *n == 0 || self.field(shapes, e),
            _ => false,
        }
    }
}

impl ZeroSizes {
    pub(super) fn truncate(&mut self, len: usize) {
        self.0.retain(|row, _| row.index() < len);
    }
}

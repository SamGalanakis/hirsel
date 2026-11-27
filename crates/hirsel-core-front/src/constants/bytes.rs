use super::*;

impl ConstCx<'_, '_> {
    pub(super) fn bytes_of(&mut self, n: NodeId) -> Option<ConstValue> {
        let ty = self.p.lower_ty(self.at, n, &[]);
        if mentions_error(&self.p.types, ty) {
            return None;
        }
        let Some(size) = crate::layout::logical_size(&self.p.decls, &mut self.p.types, ty) else {
            return self.fail(n, "the type has no representable logical size");
        };
        Some(ConstValue {
            ty: scalar(Scalar::U64),
            data: ConstData::Bits(size),
        })
    }
}

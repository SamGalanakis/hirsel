//! `list.prefetch` and `inline.prefetch`: a read hint for one element.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// A hint at element `i` of a List (by its header) or an Inline array (by its
    /// data). There is no bounds check: the address may lie past the storage, which
    /// a prefetch never faults on. Elements without storage emit nothing.
    pub(super) fn prefetch(
        &mut self,
        inline: bool,
        ty: TyId,
        carried: Option<ValueId>,
        i: ValueId,
        n: NodeId,
    ) -> Option<()> {
        if !self.live() {
            return None;
        }
        let item = if inline {
            let TyKind::Array(item, len) = *self.t.kind(ty) else {
                return None;
            };
            if len == 0 {
                return Some(());
            }
            item
        } else {
            ty
        };
        if self.list_storage_static(item) == Some(false) {
            return Some(());
        }
        let data = if inline {
            carried?
        } else {
            let carrier = self.list_carrier(item, n)?;
            self.carrier_word(carried?, carrier, SeqWord::Data, n)
        };
        let stride = self.layout_size(item, false, n)?;
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, i, stride, n);
        self.inst(Inst::Prefetch { data, offset }, n);
        Some(())
    }
}

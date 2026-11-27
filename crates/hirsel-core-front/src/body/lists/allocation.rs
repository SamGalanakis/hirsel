use super::*;

impl Cx<'_, '_, '_> {
    pub(in crate::body) fn paired_bytes(
        &mut self,
        file: bool,
        head: ValueId,
        count: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let item = scalar(Scalar::U8);
        let carrier = self.list_carrier(item, n)?;
        let cap = self.carrier_word(head, carrier, SeqWord::Cap, n);
        let data = self.carrier_word(head, carrier, SeqWord::Data, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let owns = self.list_compare(CompareOp::Ne, cap, zero, n);
        let key = self.list_choose(owns, data, null, n);
        let (place, align) = if file {
            (AllocPlace::FilePartner(key), 4096)
        } else {
            (AllocPlace::ByteTail(key), 8)
        };
        self.list_with_capacity_at(item, count, result, place, align, n)
    }

    pub(in crate::body) fn list_allocate(
        &mut self,
        plan: &ListPlan,
        place: AllocPlace,
        n: NodeId,
    ) -> (ValueId, ValueId) {
        self.list_allocate_aligned(plan, place, 8, n)
    }

    pub(super) fn list_allocate_aligned(
        &mut self,
        plan: &ListPlan,
        place: AllocPlace,
        align: u64,
        n: NodeId,
    ) -> (ValueId, ValueId) {
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let kind = AllocKind::own(plan.edge.clone());
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind,
                place,
                charge: plan.charge,
                overflow: Some(plan.overflow),
                size: plan.size,
                align,
            },
            n,
        );
        (block, available)
    }
}

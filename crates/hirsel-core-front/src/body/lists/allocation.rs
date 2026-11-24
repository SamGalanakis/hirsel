use super::*;

impl Cx<'_, '_, '_> {
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
        let kind = AllocKind::Own {
            edge: plan.edge.clone(),
        };
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

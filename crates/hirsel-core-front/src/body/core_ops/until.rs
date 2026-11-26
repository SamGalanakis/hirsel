use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn net_until(
        &mut self,
        ty: TyId,
        value: ValueId,
        n: NodeId,
    ) -> Option<[ValueId; 3]> {
        let (_, at) = self.runtime_place(ty, Some(value), n)?;
        let tag = self.net_case(at, n);
        let never = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let k = Ty::Scalar(Scalar::U8);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[k, u, u]);
        self.runtime_branch(never, yes, no, n);
        self.start(yes);
        let zero = self.konst(u, 0, n)?;
        let off = self.konst(k, 0, n)?;
        self.jump(join, &[off, zero, zero], n);
        self.start(no);
        let mut fields = Vec::new();
        for field in 0..2 {
            let field = self.address(
                AddressBase::Address(at),
                &[Step::CaseField { case: 1, field }],
                PlaceTy::Value(u),
                n,
            );
            fields.push(self.read_bits(field, u, n));
        }
        let on = self.konst(k, 1, n)?;
        self.jump(join, &[on, fields[0], fields[1]], n);
        self.start(join);
        let values = self.block_params(join);
        Some([values[0], values[1], values[2]])
    }
}

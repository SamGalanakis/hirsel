use super::*;

/// `core.net.AddressClass`'s case count: `hirsel_core_net::AddressClass`'s order.
const CLASSES: u32 = 17;

impl Cx<'_, '_, '_> {
    /// `net.class(ip)` (NET-18): the run time's table answers the case number.
    pub(super) fn runtime_net_class(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let ty = self.p.fns[f].params[0].ty;
        let (_, at) = self.runtime_place(ty, vals[0], n)?;
        let args = self.net_ip(at, n)?.map(Arg::Value);
        let [class, ..] = self.runtime_row(RtOp::Host(HostOp::NetClass), &args, n)?;
        let id = self.runtime_type(result, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let join = self.new_block(&[]);
        for case in 0..CLASSES - 1 {
            let on = self.runtime_compare(Ty::Scalar(Scalar::U64), class, u64::from(case), n)?;
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: CLASSES - 1,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    /// `net.denials(n)` (NET-18): the provider's refusal counts. Part 2 (the
    /// provider's own address) comes with S2, so `own_address` is 0.
    pub(super) fn runtime_net_denials(
        &mut self,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let [not_public, ..] =
            self.runtime_row(RtOp::Host(HostOp::NetDenials), &[Arg::Value(vals[0]?)], n)?;
        let id = self.runtime_type(result, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.runtime_store(to, &[Step::Field(0)], u, not_public, n);
        let zero = self.konst(u, 0, n)?;
        self.runtime_store(to, &[Step::Field(1)], u, zero, n);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }
}

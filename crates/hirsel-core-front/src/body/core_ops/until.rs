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

    pub(super) fn runtime_receive_error(
        &mut self,
        item: TyId,
        receiver: ValueId,
        until: Option<[ValueId; 3]>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let receiver = self.runtime_handle(receiver, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let mut args = vec![Arg::Value(receiver), Arg::Address(output)];
        let op = if let Some(until) = until {
            args.extend(until.map(Arg::Value));
            RtOp::ReceiveUntil { item: id }
        } else {
            RtOp::TryReceive { item: id }
        };
        let [status, ..] = self.runtime_row(op, &args, n)?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), status, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                let Some(error_to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                else {
                    return;
                };
                let empty = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n);
                let Some(empty) = empty else { return };
                let yes = cx.new_block(&[]);
                let no = cx.new_block(&[]);
                let join = cx.new_block(&[]);
                cx.runtime_branch(empty, yes, no, n);
                cx.start(yes);
                cx.runtime_case_payload(error_to, 0, UNIT, None, n);
                cx.jump(join, &[], n);
                cx.start(no);
                cx.runtime_case_payload(error_to, 1, UNIT, None, n);
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }
}

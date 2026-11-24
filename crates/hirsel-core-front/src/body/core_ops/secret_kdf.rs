use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn runtime_secret_derive(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let mut operands = vec![Arg::Value(self.runtime_key(vals[0]?, n))];
        for (index, value) in vals.iter().copied().enumerate().skip(1) {
            match (op, index) {
                (HostOp::SecretDeriveArgon2id, 2) => {
                    let cost = self.p.fns[f].params[index].ty;
                    operands.extend(self.runtime_argon2_cost(cost, value?, n)?.map(Arg::Value));
                }
                (HostOp::SecretDeriveDigest, 1 | 3) => {
                    let (_, at) = self.runtime_place(self.p.fns[f].params[index].ty, value, n)?;
                    operands.push(Arg::Value(self.net_case(at, n)));
                }
                _ => operands.push(Arg::Value(value?)),
            }
        }
        let [value, tag, _, available] = self.runtime_row(RtOp::Host(op), &operands, n)?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), tag, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[], Ty::Scalar(Scalar::U64), value, n);
                }
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_secret_error(at, tag, value, available, n);
                }
            },
        )
    }
}

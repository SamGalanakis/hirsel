use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(in crate::body) fn argon2id(
        &mut self,
        cost: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let k = Ty::Scalar(Scalar::U8);
        let mut args = Vec::with_capacity(13);
        for &carrier in &vals[..4] {
            let carrier = carrier?;
            args.extend([
                Arg::Value(self.read_sequence_word(carrier, 0, Ty::Ptr, n)),
                Arg::Value(self.read_sequence_word(carrier, 8, u, n)),
            ]);
        }
        args.extend(self.runtime_argon2_cost(cost, vals[4]?, n)?.map(Arg::Value));
        args.extend([Arg::Value(vals[5]?), Arg::Value(vals[6]?)]);
        let [requested, available, error] = self.runtime_outputs(RtOp::Argon2id, &args, n)?;
        let ok = self.runtime_compare(k, error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |_, _, _| {},
            |cx, to, ty| {
                let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                else {
                    return;
                };
                let params = cx.runtime_compare(k, error, 1, n).unwrap();
                let yes = cx.new_block(&[]);
                let no = cx.new_block(&[]);
                let join = cx.new_block(&[]);
                cx.runtime_branch(params, yes, no, n);
                cx.start(yes);
                cx.inst(
                    Inst::SetCase {
                        address: at,
                        case: 0,
                    },
                    n,
                );
                cx.jump(join, &[], n);
                cx.start(no);
                cx.inst(
                    Inst::SetCase {
                        address: at,
                        case: 1,
                    },
                    n,
                );
                let capacity = cx.capacity_ty(n);
                if let Some(c) =
                    cx.runtime_field(at, &[Step::CaseField { case: 1, field: 0 }], capacity, n)
                {
                    cx.runtime_capacity(c, requested, available, n);
                }
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }
    pub(super) fn runtime_argon2_cost(
        &mut self,
        cost: TyId,
        value: ValueId,
        n: NodeId,
    ) -> Option<[ValueId; 3]> {
        let cost_row = self.runtime_type(cost, n)?;
        let cost_place = self.address(
            AddressBase::Value(value),
            &[],
            PlaceTy::Declared(cost_row),
            n,
        );
        let mut fields = [ValueId(0); 3];
        for (i, field) in fields.iter_mut().enumerate() {
            let (_, at) = self.net_field(cost, cost_place, i, n)?;
            *field = self.read_bits(at, Ty::Scalar(Scalar::U32), n);
        }
        Some(fields)
    }
}

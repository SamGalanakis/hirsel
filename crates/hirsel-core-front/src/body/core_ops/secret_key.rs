use super::*;

impl Cx<'_, '_, '_> {
    /// Secret key operations: the runtime's error tag is the
    /// `KeyError` case plus one; `Capacity` carries requested and available.
    pub(super) fn runtime_secret_key(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let mut operands = vec![self.runtime_key(vals[0]?, n)];
        match op {
            HostOp::SecretWrap => operands.extend([self.runtime_key(vals[1]?, n), vals[2]?]),
            HostOp::SecretUnwrap => operands.extend([vals[1]?, vals[2]?]),
            HostOp::SecretSign | HostOp::SecretAeadSeal | HostOp::SecretAeadOpen => {
                let (_, at) = self.runtime_place(self.p.fns[f].params[1].ty, vals[1], n)?;
                let tag = self.net_case(at, n);
                let alg = self.value(u);
                self.inst(
                    Inst::Convert {
                        result: alg,
                        flag: None,
                        operand: tag,
                        mode: ConvertMode::Widen,
                    },
                    n,
                );
                operands.extend([alg, vals[2]?, vals[3]?]);
                if op != HostOp::SecretSign {
                    operands.extend([vals[4]?, vals[5]?]);
                }
            }
            _ => unreachable!(),
        }
        let args: Vec<_> = operands.into_iter().map(Arg::Value).collect();
        let [value, error, _, written] = self.runtime_row(RtOp::Host(op), &args, n)?;
        let byte = Ty::Scalar(Scalar::U8);
        let ok = self.runtime_compare(byte, error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[], u, value, n);
                }
            },
            |cx, to, ty| {
                let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                else {
                    return;
                };
                let join = cx.new_block(&[]);
                for case in 0..3 {
                    let Some(this) = cx.runtime_compare(byte, error, case as u64 + 1, n) else {
                        return;
                    };
                    let yes = cx.new_block(&[]);
                    let no = cx.new_block(&[]);
                    cx.runtime_branch(this, yes, no, n);
                    cx.start(yes);
                    cx.inst(Inst::SetCase { address: at, case }, n);
                    cx.jump(join, &[], n);
                    cx.start(no);
                }
                cx.inst(
                    Inst::SetCase {
                        address: at,
                        case: 3,
                    },
                    n,
                );
                let capacity = cx.capacity_ty(n);
                if let Some(field) =
                    cx.runtime_field(at, &[Step::CaseField { case: 3, field: 0 }], capacity, n)
                {
                    cx.runtime_capacity(field, value, written, n);
                }
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }
}

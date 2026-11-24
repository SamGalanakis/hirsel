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
        let first = vals[0]?;
        let mut operands = vec![
            if matches!(op, HostOp::SecretP256Key | HostOp::SecretKeyShare) {
                first
            } else {
                self.runtime_key(first, n)
            },
        ];
        match op {
            HostOp::SecretWrap => operands.extend([self.runtime_key(vals[1]?, n), vals[2]?]),
            HostOp::SecretUnwrap | HostOp::SecretP256Key => operands.extend([vals[1]?, vals[2]?]),
            HostOp::SecretSign
            | HostOp::SecretAeadSeal
            | HostOp::SecretAeadOpen
            | HostOp::SecretKeyShare => {
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
                if op == HostOp::SecretKeyShare {
                    operands.extend([alg, vals[2]?]);
                } else {
                    operands.extend([alg, vals[2]?, vals[3]?, vals[4]?]);
                    if op != HostOp::SecretSign {
                        operands.push(vals[5]?);
                    }
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

    pub(super) fn runtime_secret_mac(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let signing = op.secret_mac_signs();
        let jws = matches!(op, HostOp::SecretJwsHmacSign | HostOp::SecretJwsHmacVerify);
        let data = if jws { 2 } else { 1 };
        let third = if signing {
            let TyKind::Adt(_, arguments) = self.t.kind(result).clone() else {
                unreachable!("the MAC builtin returns a Result")
            };
            let id = self.runtime_type(arguments[0], n)?;
            let output = self.new_slot(PlaceTy::Declared(id), n);
            self.pointer(output, n)
        } else {
            vals[data + 1]?
        };
        let secret = self.runtime_key(vals[0]?, n);
        let mut args = vec![
            Arg::Value(secret),
            Arg::Value(vals[data]?),
            Arg::Value(third),
        ];
        if jws {
            let (_, at) = self.runtime_place(self.p.fns[f].params[1].ty, vals[1], n)?;
            let case = self.net_case(at, n);
            let algorithm = self.value(u);
            self.inst(
                Inst::Convert {
                    result: algorithm,
                    flag: None,
                    operand: case,
                    mode: ConvertMode::Widen,
                },
                n,
            );
            args.push(Arg::Value(algorithm));
        }
        let [value, error, ..] = self.runtime_row(RtOp::Host(op), &args, n)?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = if signing {
                    Some(third)
                } else {
                    cx.runtime_nonzero(u, value, n)
                };
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    if jws {
                        let short = cx
                            .runtime_compare(Ty::Scalar(Scalar::U8), error, 2, n)
                            .unwrap();
                        let too_short = cx.new_block(&[]);
                        let denied = cx.new_block(&[]);
                        let join = cx.new_block(&[]);
                        cx.runtime_branch(short, too_short, denied, n);
                        cx.start(too_short);
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 1,
                            },
                            n,
                        );
                        cx.jump(join, &[], n);
                        cx.start(denied);
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 0,
                            },
                            n,
                        );
                        cx.jump(join, &[], n);
                        cx.start(join);
                        return;
                    }
                    cx.inst(
                        Inst::SetCase {
                            address: at,
                            case: 0,
                        },
                        n,
                    );
                }
            },
        )
    }

    pub(super) fn runtime_secret_seal(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let bytes = self.p.fns[f].params[0].ty;
        let rep = self.rep_at(bytes, n)?;
        let source = vals[0]?;
        let [value, tag, _, available] = self.runtime_row(
            RtOp::Host(HostOp::SecretSeal),
            &[Arg::Value(source), Arg::Value(vals[1]?)],
            n,
        )?;
        let byte = Ty::Scalar(Scalar::U8);
        let ok = self.runtime_compare(byte, tag, 0, n)?;
        // Success drops the wiped source. The error arm moves its carrier.
        self.consume(Some(source), n);
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                cx.drop_field(Some(source), bytes, n);
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[], Ty::Scalar(Scalar::U64), value, n);
                }
            },
            |cx, to, pair| {
                let TyKind::Tuple(fields) = cx.t.kind(pair) else {
                    unreachable!("secret.seal returns its error and source as a tuple");
                };
                let error_ty = fields[0];
                let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], pair, n)
                else {
                    return;
                };
                let Some(returned) = cx.runtime_field(to, &[Step::Field(1)], bytes, n) else {
                    return;
                };
                cx.install(returned, rep, Some(source), n);
                let Some(error) = cx.runtime_field(to, &[Step::Field(0)], error_ty, n) else {
                    return;
                };
                cx.runtime_secret_error(error, tag, value, available, n);
            },
        )
    }

    pub(super) fn runtime_secret_error(
        &mut self,
        error: AddressId,
        tag: ValueId,
        value: ValueId,
        available: ValueId,
        n: NodeId,
    ) {
        let Some(invalid) = self.runtime_compare(Ty::Scalar(Scalar::U8), tag, 1, n) else {
            return;
        };
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(invalid, yes, no, n);
        self.start(yes);
        self.runtime_case_payload(error, 0, UNIT, None, n);
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: error,
                case: 1,
            },
            n,
        );
        let capacity = self.capacity_ty(n);
        if let Some(at) =
            self.runtime_field(error, &[Step::CaseField { case: 1, field: 0 }], capacity, n)
        {
            self.runtime_capacity(at, value, available, n);
        }
        self.jump(join, &[], n);
        self.start(join);
    }
}

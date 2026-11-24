use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn runtime_media(
        &mut self,
        op: HostOp,
        f: usize,
        values: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if matches!(
            op,
            HostOp::ScheduleTls13
                | HostOp::ScheduleDtls12
                | HostOp::ScheduleImportTraffic
                | HostOp::ScheduleImportSrtp
        ) {
            return self.runtime_media_constructor(op, f, values, result, n);
        }
        let mut operands: Vec<ValueId> = values.iter().copied().collect::<Option<_>>()?;
        let word = Ty::Scalar(Scalar::U64);
        if op != HostOp::ScheduleQuicInitial {
            operands[0] = self.runtime_key(operands[0], n);
        }
        let tag_index = match op {
            HostOp::ScheduleQuicInitial => Some(0),
            HostOp::ScheduleSealer
            | HostOp::ScheduleSealerPair
            | HostOp::ScheduleSpaceInfo
            | HostOp::ScheduleOpener
            | HostOp::ScheduleHeaderMask
            | HostOp::ScheduleRetire
            | HostOp::ScheduleSrtp
            | HostOp::ScheduleFinished
            | HostOp::ScheduleVerifyFinished => Some(1),
            _ => None,
        };
        if let Some(index) = tag_index {
            let (_, place) =
                self.runtime_place(self.p.fns[f].params[index].ty, values[index], n)?;
            operands[index] = self.case_as_word(place, n);
        }
        if op == HostOp::ScheduleSealerPair {
            let (_, place) = self.runtime_place(self.p.fns[f].params[2].ty, values[2], n)?;
            operands[2] = self.case_as_word(place, n);
        }
        let mut args: Vec<_> = operands.into_iter().map(Arg::Value).collect();
        if op == HostOp::PacketMask {
            let ty = self.runtime_type(result, n)?;
            let out = self.new_slot(PlaceTy::Declared(ty), n);
            let pointer = self.pointer(out, n);
            args.push(Arg::Value(pointer));
            self.runtime_row(RtOp::Host(op), &args, n)?;
            return Some(pointer);
        }
        let [value, tag, _, available] = self.runtime_row(RtOp::Host(op), &args, n)?;
        let byte = Ty::Scalar(Scalar::U8);
        let ok = self.runtime_compare(byte, tag, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if matches!(
                    op,
                    HostOp::ScheduleApplication
                        | HostOp::ScheduleKeyUpdate
                        | HostOp::ScheduleRetire
                ) {
                    return;
                }
                if let Some(out) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    if matches!(op, HostOp::ScheduleSealerPair | HostOp::ScheduleSpaceInfo) {
                        for (i, part) in [value, available].into_iter().enumerate() {
                            if let Some((_, field)) = cx.net_field(ty, out, i, n) {
                                cx.runtime_store(field, &[], word, part, n);
                            }
                        }
                    } else if op == HostOp::ScheduleVerifyFinished {
                        if let Some(valid) = cx.runtime_compare(word, value, 1, n) {
                            cx.runtime_store(out, &[], Ty::Scalar(Scalar::Bool), valid, n);
                        }
                    } else {
                        cx.runtime_store(out, &[], word, value, n);
                    }
                }
            },
            |cx, to, ty| {
                if let Some(out) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.media_error(out, tag, value, available, n);
                }
            },
        )
    }

    pub(super) fn runtime_optional_carrier(
        &mut self,
        ty: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let TyKind::Adt(_, types) = self.t.kind(ty) else {
            return None;
        };
        let item = types[0];
        let (_, at) = self.runtime_place(ty, value, n)?;
        let tag = self.net_case(at, n);
        let some = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 1, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[Ty::Ptr]);
        self.runtime_branch(some, yes, no, n);
        self.start(yes);
        let carrier = self.runtime_field(at, &[Step::CaseField { case: 1, field: 0 }], item, n)?;
        let carrier = self.pointer(carrier, n);
        self.jump(join, &[carrier], n);
        self.start(no);
        let null = self.konst(Ty::Ptr, 0, n)?;
        self.jump(join, &[null], n);
        self.start(join);
        Some(self.block_params(join)[0])
    }

    fn case_as_word(&mut self, place: AddressId, n: NodeId) -> ValueId {
        let case = self.net_case(place, n);
        let word = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Convert {
                result: word,
                flag: None,
                operand: case,
                mode: ConvertMode::Widen,
            },
            n,
        );
        word
    }

    fn runtime_media_constructor(
        &mut self,
        op: HostOp,
        f: usize,
        values: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let profile_ty = self.p.fns[f].params[0].ty;
        let (_, at) = self.runtime_place(profile_ty, values[0], n)?;
        let word = Ty::Scalar(Scalar::U64);
        let profile = self.case_as_word(at, n);
        let mut args = vec![Arg::Value(profile)];
        if matches!(op, HostOp::ScheduleTls13 | HostOp::ScheduleImportTraffic) {
            let suite_join = self.new_block(&[word]);
            for case in 0..4 {
                let otherwise = if case < 3 {
                    let equal = self.runtime_compare(word, profile, u64::from(case), n)?;
                    let yes = self.new_block(&[]);
                    let otherwise = self.new_block(&[]);
                    self.runtime_branch(equal, yes, otherwise, n);
                    self.start(yes);
                    Some(otherwise)
                } else {
                    None
                };
                let (_, suite) = self.net_case_field(profile_ty, at, case, 0, n)?;
                let suite = self.case_as_word(suite, n);
                self.jump(suite_join, &[suite], n);
                if let Some(otherwise) = otherwise {
                    self.start(otherwise);
                }
            }
            self.start(suite_join);
            args.push(Arg::Value(self.block_params(suite_join)[0]));
        }
        let owned_index = if matches!(
            op,
            HostOp::ScheduleImportTraffic | HostOp::ScheduleImportSrtp
        ) {
            1
        } else {
            let (_, at) = self.runtime_place(self.p.fns[f].params[1].ty, values[1], n)?;
            args.push(Arg::Value(self.case_as_word(at, n)));
            if op == HostOp::ScheduleDtls12 {
                args.push(Arg::Value(values[2]?));
                3
            } else {
                2
            }
        };
        let input_ty = self.p.fns[f].params[owned_index].ty;
        let input = values[owned_index]?;
        args.push(Arg::Value(self.runtime_key(input, n)));
        for (i, value) in values.iter().enumerate().skip(owned_index + 1) {
            let value = if op == HostOp::ScheduleDtls12 && i == 7 {
                self.runtime_optional_carrier(self.p.fns[f].params[i].ty, *value, n)?
            } else {
                (*value)?
            };
            args.push(Arg::Value(value));
        }
        // The adapter takes custody at the call boundary and returns the
        // same holder on refusals before issue.
        self.consume(Some(input), n);
        let [handle, tag, returned, available] = self.runtime_row(RtOp::Host(op), &args, n)?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), tag, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[], word, handle, n);
                }
            },
            |cx, to, tuple| {
                let Some(error) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], tuple, n)
                else {
                    return;
                };
                let Some((_, at)) = cx.net_field(tuple, error, 0, n) else {
                    return;
                };
                cx.media_error(at, tag, handle, available, n);
                let Some((_, option)) = cx.net_field(tuple, error, 1, n) else {
                    return;
                };
                let Some(returns) = cx.runtime_compare(Ty::Scalar(Scalar::U32), returned, 1, n)
                else {
                    return;
                };
                let yes = cx.new_block(&[]);
                let no = cx.new_block(&[]);
                let join = cx.new_block(&[]);
                cx.runtime_branch(returns, yes, no, n);
                cx.start(yes);
                cx.runtime_case_payload(option, 1, input_ty, Some(input), n);
                cx.jump(join, &[], n);
                cx.start(no);
                cx.inst(
                    Inst::SetCase {
                        address: option,
                        case: 0,
                    },
                    n,
                );
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }

    fn media_error(
        &mut self,
        out: AddressId,
        tag: ValueId,
        requested: ValueId,
        available: ValueId,
        n: NodeId,
    ) {
        let join = self.new_block(&[]);
        for case in 0..4 {
            let Some(matches) =
                self.runtime_compare(Ty::Scalar(Scalar::U8), tag, (case + 1).into(), n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(matches, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: out, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        let case = 4;
        self.inst(Inst::SetCase { address: out, case }, n);
        let capacity = self.capacity_ty(n);
        if let Some(at) =
            self.runtime_field(out, &[Step::CaseField { case, field: 0 }], capacity, n)
        {
            self.runtime_capacity(at, requested, available, n);
        }
        self.jump(join, &[], n);
        self.start(join);
    }
}

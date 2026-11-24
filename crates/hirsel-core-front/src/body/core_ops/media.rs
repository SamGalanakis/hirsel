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
        let mut operands: Vec<ValueId> = values.iter().copied().collect::<Option<_>>()?;
        let word = Ty::Scalar(Scalar::U64);
        if op != HostOp::ScheduleQuicInitial {
            operands[0] = self.runtime_key(operands[0], n);
        }
        let tag_index = match op {
            HostOp::ScheduleQuicInitial => Some(0),
            HostOp::ScheduleSealer | HostOp::ScheduleOpener | HostOp::ScheduleHeaderMask => Some(1),
            _ => None,
        };
        if let Some(index) = tag_index {
            let (_, place) =
                self.runtime_place(self.p.fns[f].params[index].ty, values[index], n)?;
            let case = self.net_case(place, n);
            let tag = self.value(word);
            self.inst(
                Inst::Convert {
                    result: tag,
                    flag: None,
                    operand: case,
                    mode: ConvertMode::Widen,
                },
                n,
            );
            operands[index] = tag;
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
                if let Some(out) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(out, &[], word, value, n);
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

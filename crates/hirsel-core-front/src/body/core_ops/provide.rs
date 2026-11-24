use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn runtime_host_void(
        &mut self,
        op: HostOp,
        args: &[Arg],
        n: NodeId,
    ) -> Option<ValueId> {
        self.runtime_row(RtOp::Host(op), args, n)?;
        None
    }

    /// `provide.clock/random/process/stdout/stderr`, the hand operations and
    /// the served-port operations (core.md, Provide). A constructor's pair is
    /// (value, written): the capability and its hand or `Served` end.
    pub(super) fn runtime_provide(
        &mut self,
        f: usize,
        op: HostOp,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let hand = matches!(
            op,
            HostOp::ClockAdvance
                | HostOp::ClockSetWall
                | HostOp::ClockIdle
                | HostOp::ProcessTrigger
                | HostOp::PortNext
                | HostOp::PortReply
                | HostOp::PortFill
                | HostOp::UdpFillPackets
        );
        // A resource operand arrives as its key, or as a pointer to it when the
        // caller lends or hands over a place.
        let mut args = Vec::new();
        for (i, v) in vals.iter().enumerate() {
            let v = (*v)?;
            let resource = (i == 0
                && (hand
                    || matches!(
                        op,
                        HostOp::CallWord
                            | HostOp::CallSecret
                            | HostOp::CallPayload
                            | HostOp::CallPayloadPrefix
                    )))
                || (i == 1
                    && matches!(
                        op,
                        HostOp::PortReply | HostOp::PortFill | HostOp::UdpFillPackets
                    ));
            let by_place = resource && self.b.values[v.index()] == Ty::Ptr;
            args.push(Arg::Value(if by_place {
                self.runtime_key(v, n)
            } else {
                v
            }));
        }
        if op == HostOp::PortReply {
            // The commit consumes the Call even when the reply fails the server.
            self.consume(vals[1], n);
        }
        let [value, error, _, written] = self.runtime_row(RtOp::Host(op), &args, n)?;
        if op == HostOp::UdpFillPackets {
            self.consume(vals[2], n);
            self.drop_field(vals[2], self.p.fns[f].params[2].ty, n);
        }
        if matches!(
            op,
            HostOp::ProvideClock | HostOp::ProvideRandom | HostOp::ProvideProcess
        ) {
            // A cell's charge (MEM-24): a refusal answers grant 0 with the
            // available bytes in `written`.
            let requested = self.konst(u, hirsel_core_layout::charge::PROVIDE_CELL, n)?;
            let ok = self.runtime_nonzero(u, value, n)?;
            self.raise_unless(ok, requested, written, n);
        }
        if op == HostOp::UdpFillPackets {
            self.consume(vals[2], n);
            self.drop_field(vals[2], self.p.fns[f].params[2].ty, n);
        }
        if matches!(
            op,
            HostOp::ProvideStdout
                | HostOp::ProvideStderr
                | HostOp::ProvideDir
                | HostOp::ProvideNet
                | HostOp::ProvideExec
        ) {
            // A port's room, with its own record, saturating as the run time's.
            let cell = self.konst(u, hirsel_core_layout::charge::PROVIDE_CELL, n)?;
            let requested = self.value(u);
            self.inst(
                Inst::IntArith {
                    result: requested,
                    flag: None,
                    op: ArithOp::Add,
                    mode: ArithMode::Saturating,
                    left: cell,
                    right: vals[0]?,
                },
                n,
            );
            let ok = self.runtime_nonzero(u, value, n)?;
            self.raise_unless(ok, requested, written, n);
        }
        match op {
            HostOp::ProvideClock
            | HostOp::ProvideProcess
            | HostOp::ProvideStdout
            | HostOp::ProvideStderr
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec => {
                let id = self.runtime_type(result, n)?;
                let to = self.new_slot(PlaceTy::Declared(id), n);
                self.runtime_store(to, &[Step::Field(0)], u, value, n);
                self.runtime_store(to, &[Step::Field(1)], u, written, n);
                Some(self.pointer(to, n))
            }
            HostOp::ProvideRandom => Some(value),
            HostOp::ProvideRandomTape => {
                let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
                self.raise_unless(ok, value, written, n);
                self.consume(vals[0], n);
                self.drop_field(vals[0], self.p.fns[f].params[0].ty, n);
                Some(value)
            }
            HostOp::ClockIdle => {
                let zero = self.konst(u, 0, n)?;
                let some = self.test(CompareOp::Ne, written, zero, n);
                self.runtime_result(
                    result,
                    some,
                    n,
                    |cx, to, ty| cx.runtime_case_payload(to, 1, ty, Some(value), n),
                    |_, _, _| {},
                )
            }
            HostOp::PortNext => {
                // The Call is a resource place holding its key, as `host.open`'s file.
                let zero = self.konst(u, 0, n)?;
                let some = self.test(CompareOp::Ne, written, zero, n);
                self.runtime_result(
                    result,
                    some,
                    n,
                    |cx, to, ty| {
                        if let Some(to) =
                            cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                        {
                            cx.runtime_store(to, &[], u, value, n);
                        }
                    },
                    |_, _, _| {},
                )
            }
            HostOp::CallWord | HostOp::CallPayloadPrefix => Some(value),
            HostOp::CallSecret => {
                // A refused charge answers a nonzero error: the requested
                // bytes in `value` and the available in `written`.
                let u8_ = Ty::Scalar(Scalar::U8);
                let ok = self.runtime_compare(u8_, error, 0, n)?;
                self.raise_unless(ok, value, written, n);
                self.secret_carrier(result, value, n)
            }
            HostOp::PortReply | HostOp::PortFill | HostOp::UdpFillPackets => {
                // A room refusal answers error tag 1: the requested bytes in
                // `value` and the available in `written` (MEM-21).
                let u8_ = Ty::Scalar(Scalar::U8);
                let ok = self.runtime_compare(u8_, error, 0, n)?;
                self.raise_unless(ok, value, written, n);
                Some(value)
            }
            HostOp::ProcessTrigger => {
                self.consume(vals[0], n);
                None
            }
            HostOp::ClockAdvance | HostOp::ClockSetWall | HostOp::CallPayload => None,
            _ => unreachable!(),
        }
    }
}

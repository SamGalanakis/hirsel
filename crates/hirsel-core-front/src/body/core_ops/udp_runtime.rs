use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn runtime_udp(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let key = if matches!(op, HostOp::UdpRecv | HostOp::UdpSend | HostOp::UdpLocalAddr) {
            self.runtime_key(vals[0]?, n)
        } else {
            vals[0]?
        };
        let mut operands = vec![key];
        match op {
            HostOp::UdpOpen | HostOp::UdpBind | HostOp::UdpBindGroup => {
                if op == HostOp::UdpOpen {
                    operands.push(vals[1]?);
                } else {
                    let ty = self.p.fns[f].params[1].ty;
                    let (_, at) = self.runtime_place(ty, vals[1], n)?;
                    let (_, ip) = self.net_field(ty, at, 0, n)?;
                    operands.extend(self.net_ip(ip, n)?);
                    operands.push(self.net_read_field(ty, at, 1, n)?);
                }
                let ty = self.p.fns[f].params[2].ty;
                let (_, at) = self.runtime_place(ty, vals[2], n)?;
                for field in 0..2 {
                    let (_, address) = self.net_case_field(ty, at, 0, field, n)?;
                    operands.push(self.read_bits(address, Ty::Scalar(Scalar::U32), n));
                }
                if op == HostOp::UdpBindGroup {
                    let ty = self.p.fns[f].params[4].ty;
                    let (_, steer) = self.runtime_place(ty, vals[4], n)?;
                    operands.extend([vals[3]?, self.net_case(steer, n), vals[5]?]);
                }
            }
            HostOp::UdpRecv | HostOp::UdpSend => {
                operands.extend([vals[1]?, vals[2]?]);
                operands.extend(self.net_until(self.p.fns[f].params[3].ty, vals[3]?, n)?);
            }
            HostOp::UdpLocalAddr => {}
            _ => unreachable!(),
        }
        let args = operands.into_iter().map(Arg::Value).collect::<Vec<_>>();
        let values = self.runtime_outputs::<8>(RtOp::Host(op), &args, n)?;
        let [value, error, code, ..] = values;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| match op {
                HostOp::UdpRecv | HostOp::UdpSend => {
                    cx.runtime_case_payload(to, 0, ty, Some(value), n);
                }
                _ => {
                    let Some(payload) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    else {
                        return;
                    };
                    match op {
                        HostOp::UdpOpen => cx.runtime_store(payload, &[], u64_, value, n),
                        HostOp::UdpBind => {
                            cx.runtime_store(payload, &[Step::Field(0)], u64_, value, n);
                            if let Some((addr_ty, addr)) = cx.net_field(ty, payload, 1, n) {
                                cx.net_store_address(addr, addr_ty, &values[4..], n);
                            }
                        }
                        HostOp::UdpBindGroup | HostOp::UdpLocalAddr => {
                            cx.net_store_address(payload, ty, &values[4..], n);
                        }
                        _ => unreachable!(),
                    }
                }
            },
            |cx, to, ty| {
                if let Some(payload) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_net_error(payload, ty, error, code, n);
                }
            },
        )
    }
}

use super::*;

impl Function<'_, '_, '_> {
    pub(super) fn conn_receive(
        &mut self,
        op: RtOp,
        args: &[Value],
        site: Value,
        fail: Option<BlockId>,
    ) -> Result<[Value; 3]> {
        let values = if matches!(op, RtOp::ConnReceiveUntil { .. }) {
            self.c_call(
                "hirsel_core_conn_receive_until",
                &[P, P, P, K, U, U, U],
                &[U, U],
                &[
                    self.context,
                    args[0],
                    args[1],
                    args[2],
                    args[3],
                    args[4],
                    site,
                ],
            )?
        } else {
            let wait = matches!(op, RtOp::ConnReceive { .. });
            let wait = self.b.ins().iconst(types::I8, i64::from(wait));
            self.c_call(
                "hirsel_core_conn_receive",
                &[P, P, P, U, B],
                &[U, U],
                &[self.context, args[0], args[1], site, wait],
            )?
        };
        let packed = values[0];
        let status = self.b.ins().ushr_imm_s(packed, 8);
        let status = self.b.ins().ireduce(types::I8, status);
        self.call_failure(status, fail)?;
        Ok([
            self.b.ins().ireduce(types::I8, packed),
            self.b.ins().ushr_imm_s(packed, 16),
            values[1],
        ])
    }
}

use super::*;

impl Derive<'_> {
    pub(in crate::body) fn runtime(
        &mut self,
        c: &mut Code,
        op: RtOp,
        args: &[ValueId],
    ) -> Option<ValueId> {
        self.runtime_with_failure(c, op, args, None)
    }

    pub(in crate::body) fn runtime_with_failure(
        &mut self,
        c: &mut Code,
        op: hirsel_core_ir::RtOp,
        args: &[ValueId],
        fail: Option<BlockId>,
    ) -> Option<ValueId> {
        let sig = op.signature();
        let result = sig.results.first().map(|&ty| c.value(ty));
        let signature = signature_id(self.module, sig);
        let args = PoolRange::push(&mut c.body.pools.args, args.iter().copied().map(Arg::Value));
        let results = PoolRange::push(&mut c.body.pools.values, result);
        let call = CallId(c.body.calls.len() as u32);
        c.body.calls.push(CallSite {
            callee: Callee::Runtime(op),
            signature,
            args,
            results,
            fail,
            cold: false,
        });
        c.inst(Inst::Call { call });
        result
    }
}

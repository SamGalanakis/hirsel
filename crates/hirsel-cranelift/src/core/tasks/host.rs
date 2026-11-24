use super::*;

impl Function<'_, '_, '_> {
    pub(super) fn host_adapter(
        &mut self,
        op: HostOp,
        args: &[Value],
        site: Value,
    ) -> Result<calls::NativeValues> {
        let signature = op_signature(op);
        let count = signature.len();
        let mut params = vec![P];
        params.extend(signature);
        params.push(U);
        let mut operands = vec![self.context];
        operands.extend_from_slice(&args[..count]);
        operands.push(site);
        let mut results = vec![U, K, Ty::Scalar(Scalar::U32), U, K];
        if matches!(op, HostOp::EnvGet | HostOp::EnvName) {
            results[0] = P;
        }
        if op.address_outcome() {
            results.extend([K, U, U, Ty::Scalar(Scalar::U16)]);
        } else if op == HostOp::TlsConnect {
            results.extend([U; 4]);
        } else if op == HostOp::ExecSpawn {
            results.extend([U; 3]);
        }
        self.c_call(op.adapter_symbol(), &params, &results, &operands)
    }
}

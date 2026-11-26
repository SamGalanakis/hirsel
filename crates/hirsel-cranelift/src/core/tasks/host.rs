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
        if matches!(op, HostOp::EnvGet | HostOp::EnvName | HostOp::TlsServerName) {
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

    pub(super) fn host_outputs(
        op: HostOp,
        values: calls::NativeValues,
        outputs: &mut [Value],
    ) -> usize {
        let n = if matches!(op, HostOp::Random | HostOp::Stopped) {
            0
        } else if matches!(op, HostOp::WallMs | HostOp::MonoNs | HostOp::BootNs) {
            1
        } else {
            4
        };
        for i in 0..n {
            outputs[i] = values[i];
        }
        if op.address_outcome() || op == HostOp::TlsConnect {
            for i in 4..8 {
                outputs[i] = values[i + 1];
            }
            8
        } else if op == HostOp::ExecSpawn {
            for i in 4..7 {
                outputs[i] = values[i + 1];
            }
            7
        } else if matches!(op, HostOp::Stat | HostOp::StatFile) {
            outputs[4] = values[5];
            5
        } else {
            n
        }
    }

    /// `host.zero_at(f, at, len)`: the file key, offset and length (§3.3b).
    pub(super) fn host_zero_at(
        &mut self,
        args: &[Value],
        site: Value,
    ) -> Result<calls::NativeValues> {
        self.c_call(
            "hirsel_core_host_zero_at",
            &[P, U, U, U, U],
            &[U, K, Ty::Scalar(Scalar::U32), U, K],
            &[self.context, args[0], args[1], args[2], site],
        )
    }
}

use super::*;

impl Function<'_, '_, '_> {
    /// An explicit stack slot of its own, for a descriptor passed by pointer
    /// beside an indirect result that shares the call scratch.
    pub(super) fn own_slot(&mut self, size: u32) -> Value {
        let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            size.max(8).next_multiple_of(8),
            3,
        ));
        self.b.ins().stack_addr(types::I64, slot, 0)
    }

    pub(super) fn descriptor(&mut self, types: &[TypeId], task_input: bool) -> Result<Value> {
        let storage = self.calls.storage(&mut self.b, (types.len() * 32) as u32);
        self.describe(storage, types, task_input)
    }

    pub(super) fn signature_call(&mut self, op: RtOp, args: &[Value]) -> Result<Value> {
        let (name, params): (&str, &[Ty]) = match op {
            RtOp::Ed25519Verify => ("hirsel_core_ed25519_verify", &[P, U, P, U, P, U]),
            RtOp::P256Verify => ("hirsel_core_p256_verify", &[P, U, P, U, P, U]),
            RtOp::P256VerifyDer => ("hirsel_core_p256_verify_der", &[P, U, P, U, P, U]),
            RtOp::P384Verify => ("hirsel_core_p384_verify", &[P, U, P, U, P, U]),
            RtOp::P384VerifyDer => ("hirsel_core_p384_verify_der", &[P, U, P, U, P, U]),
            RtOp::RsaVerifyPkcs1Sha256 => (
                "hirsel_core_rsa_verify_pkcs1_sha256",
                &[P, U, P, U, P, U, P, U],
            ),
            RtOp::RsaVerifyPssSha256 => (
                "hirsel_core_rsa_verify_pss_sha256",
                &[P, U, P, U, P, U, P, U],
            ),
            _ => unreachable!(),
        };
        Ok(self.c_call(name, params, &[U], &args[..params.len()])?[0])
    }
}

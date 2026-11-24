use super::*;

impl Function<'_, '_, '_> {
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

use super::{Machine, RtOp};

impl Machine<'_> {
    pub(super) fn verify_signature(&self, op: RtOp, a: &[u64]) -> bool {
        use hirsel_core_net as net;
        let range = |index: usize| {
            let (data, len) = (a[index], a[index + 1] as usize);
            if len == 0 { &[] } else { self.get(data, len) }
        };
        let (first, second, third) = (range(0), range(2), range(4));
        match op {
            RtOp::Ed25519Verify => net::verify_ed25519(first, second, third),
            RtOp::P256Verify => net::verify_p256_fixed(first, second, third),
            RtOp::P256VerifyDer => net::verify_p256_asn1(first, second, third),
            RtOp::P384Verify => net::verify_p384_fixed(first, second, third),
            RtOp::P384VerifyDer => net::verify_p384_asn1(first, second, third),
            RtOp::RsaVerifyPkcs1Sha256 => {
                net::verify_rsa_pkcs1_sha256(first, second, third, range(6))
            }
            RtOp::RsaVerifyPssSha256 => net::verify_rsa_pss_sha256(first, second, third, range(6)),
            _ => unreachable!(),
        }
    }
}

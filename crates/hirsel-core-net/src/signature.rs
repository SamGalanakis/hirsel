use ring::signature;

#[derive(Clone, Copy)]
pub enum SignatureAlgorithm {
    Ed25519,
    P256Fixed,
    P256Der,
}

/// Verify a signature over the original message bytes.
pub fn verify_signature(algorithm: SignatureAlgorithm, key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    let algorithm: &dyn signature::VerificationAlgorithm = match algorithm {
        SignatureAlgorithm::Ed25519 => &signature::ED25519,
        SignatureAlgorithm::P256Fixed => &signature::ECDSA_P256_SHA256_FIXED,
        SignatureAlgorithm::P256Der => &signature::ECDSA_P256_SHA256_ASN1,
    };
    signature::UnparsedPublicKey::new(algorithm, key)
        .verify(msg, sig)
        .is_ok()
}

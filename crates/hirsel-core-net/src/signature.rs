use ring::signature;

#[derive(Clone, Copy)]
pub enum SignatureAlgorithm {
    Ed25519,
    P256Fixed,
    P256Der,
}

/// Verify a signature over the original message bytes.
pub fn verify_signature(algorithm: SignatureAlgorithm, key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    match algorithm {
        SignatureAlgorithm::Ed25519 => verify_ed25519(key, msg, sig),
        SignatureAlgorithm::P256Fixed => verify_p256_fixed(key, msg, sig),
        SignatureAlgorithm::P256Der => verify_p256_asn1(key, msg, sig),
    }
}

/// Verify an Ed25519 signature over the original message bytes.
pub fn verify_ed25519(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(msg, sig)
        .is_ok()
}

/// Verify a fixed-width P-256 ECDSA signature over the original message bytes.
pub fn verify_p256_fixed(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, key)
        .verify(msg, sig)
        .is_ok()
}

/// Verify an ASN.1 DER P-256 ECDSA signature over the original message bytes.
pub fn verify_p256_asn1(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, key)
        .verify(msg, sig)
        .is_ok()
}

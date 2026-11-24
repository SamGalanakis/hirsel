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

/// Verify a fixed-width P-384 ECDSA signature over the original message bytes.
pub fn verify_p384_fixed(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_FIXED, key)
        .verify(msg, sig)
        .is_ok()
}

/// Verify an ASN.1 DER P-384 ECDSA signature over the original message bytes.
pub fn verify_p384_asn1(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_ASN1, key)
        .verify(msg, sig)
        .is_ok()
}

/// Verify an RSA PKCS#1 v1.5 SHA-256 signature with unsigned big-endian key components.
pub fn verify_rsa_pkcs1_sha256(n: &[u8], e: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    rsa_key(n, e)
        .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
        .is_ok()
}

/// Verify an RSA-PSS SHA-256 signature with a 32-byte salt.
pub fn verify_rsa_pss_sha256(n: &[u8], e: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    rsa_key(n, e)
        .verify(&signature::RSA_PSS_2048_8192_SHA256, msg, sig)
        .is_ok()
}

fn rsa_key<'a>(n: &'a [u8], e: &'a [u8]) -> signature::RsaPublicKeyComponents<&'a [u8]> {
    fn strip_zeroes(bytes: &[u8]) -> &[u8] {
        let first = bytes
            .iter()
            .position(|&byte| byte != 0)
            .unwrap_or(bytes.len());
        &bytes[first..]
    }
    signature::RsaPublicKeyComponents {
        n: strip_zeroes(n),
        e: strip_zeroes(e),
    }
}

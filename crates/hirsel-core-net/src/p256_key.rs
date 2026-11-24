//! Key generation and signing for TLS-8. Virtual generation and RFC 6979
//! use RustCrypto and do not consult host entropy.
use crate::{key_agreement::draw_p256, key_wrap::KeyFailure};
use p256::{
    ecdsa::{Signature, SigningKey, signature::Signer},
    elliptic_curve::{
        ALGORITHM_OID,
        pkcs8::{ObjectIdentifier, PrivateKeyInfoRef, Version, der::Decode},
        sec1::ToSec1Point,
    },
    pkcs8::DecodePrivateKey,
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING as ALGORITHM, EcdsaKeyPair},
};
use sec1::{EcParameters, EcPrivateKey};
use zeroize::Zeroizing;

pub const PUBLIC_LEN: usize = 65;
pub const DOCUMENT_LEN: usize = 138;
const CURVE: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");

// ring 0.17.14's PKCS#8 v1 template, split at the private scalar and public point.
const PREFIX: [u8; 36] = [
    0x30, 0x81, 0x87, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
    0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x04, 0x6d, 0x30, 0x6b, 0x02,
    0x01, 0x01, 0x04, 0x20,
];
const PUBLIC_FIELD: [u8; 5] = [0xa1, 0x44, 0x03, 0x42, 0x00];
const PUBLIC_AT: usize = DOCUMENT_LEN - PUBLIC_LEN;

pub fn generate_host(
    public: &mut [u8; PUBLIC_LEN],
) -> Result<Zeroizing<[u8; DOCUMENT_LEN]>, KeyFailure> {
    let doc = EcdsaKeyPair::generate_pkcs8(&ALGORITHM, &SystemRandom::new())
        .map_err(|_| KeyFailure::Provider("secret.p256_key: P-256 generation: ring Unspecified"))?;
    let mut key = Zeroizing::new([0; DOCUMENT_LEN]);
    key.copy_from_slice(doc.as_ref());
    public.copy_from_slice(&key[PUBLIC_AT..]);
    Ok(key)
}

pub fn generate_virtual<E>(
    draw: impl FnMut(&mut [u8; 32]) -> Result<(), E>,
    public: &mut [u8; PUBLIC_LEN],
) -> Result<Zeroizing<[u8; DOCUMENT_LEN]>, E> {
    let key = draw_p256(draw)?;
    let point = key.public_key().to_sec1_point(false);
    let scalar = Zeroizing::new(key.to_bytes());
    let mut document = Zeroizing::new([0; DOCUMENT_LEN]);
    document[..PREFIX.len()].copy_from_slice(&PREFIX);
    document[PREFIX.len()..PUBLIC_AT - PUBLIC_FIELD.len()].copy_from_slice(scalar.as_slice());
    document[PUBLIC_AT - PUBLIC_FIELD.len()..PUBLIC_AT].copy_from_slice(&PUBLIC_FIELD);
    document[PUBLIC_AT..].copy_from_slice(point.as_bytes());
    public.copy_from_slice(point.as_bytes());
    Ok(document)
}

pub fn sign_virtual(document: &[u8], msg: &[u8]) -> Result<[u8; 64], KeyFailure> {
    let parse = || {
        let info = PrivateKeyInfoRef::from_der(document).ok()?;
        if info.version() != Version::V1
            || info.algorithm.oid != ALGORITHM_OID
            || info.algorithm.parameters_oid().ok()? != CURVE
        {
            return None;
        }
        let core = EcPrivateKey::from_der(info.private_key.as_bytes()).ok()?;
        if core.private_key.len() != 32
            || core
                .parameters
                .is_some_and(|p| p != EcParameters::NamedCurve(CURVE))
        {
            return None;
        }
        let key = p256::SecretKey::from_pkcs8_der(document).ok()?;
        // RustCrypto also accepts a compressed or absent point. Ring compares
        // the embedded bytes with its uncompressed point and requires them.
        (core.public_key? == key.public_key().to_sec1_point(false).as_bytes()).then_some(key)
    };
    let key = parse().ok_or(KeyFailure::Invalid)?;
    let signer = SigningKey::from(key);
    let signature: Signature = signer.sign(msg);
    Ok(signature.to_bytes().into())
}

pub struct HostSigner(EcdsaKeyPair);

impl HostSigner {
    pub fn load(pkcs8: &[u8]) -> Result<Self, KeyFailure> {
        EcdsaKeyPair::from_pkcs8(&ALGORITHM, pkcs8, &SystemRandom::new())
            .map(Self)
            .map_err(|error| {
                // ring 0.17.14 exposes the nonce-key RNG failure as a diagnostic.
                if error.to_string() == "RNG failed" {
                    KeyFailure::Provider("secret.sign: P-256 key loading: RNG failed")
                } else {
                    KeyFailure::Invalid
                }
            })
    }

    pub fn sign(&self, msg: &[u8]) -> Result<[u8; 64], KeyFailure> {
        let signature = self
            .0
            .sign(&SystemRandom::new(), msg)
            .map_err(|_| KeyFailure::Provider("secret.sign: P-256 signing: ring Unspecified"))?;
        Ok(signature.as_ref().try_into().unwrap())
    }
}

#[cfg(test)]
mod tests;

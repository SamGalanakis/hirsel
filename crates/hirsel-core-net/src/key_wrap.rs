//! `secret.wrap`, `secret.unwrap` and `secret.sign` over ring (TLS-5, key
//! operations). No cryptography of our own: HKDF-SHA256, ChaCha20-Poly1305
//! and the signature schemes are ring's.

use crate::SecretPolicy;
use ring::{aead, hkdf, rand, signature};

/// The shortest key-encryption key `wrap` and `unwrap` accept.
pub const KEK_MIN: usize = 32;
const VERSION: u8 = 1;
const SALT: usize = 32;
const TAG: usize = 16;
const AD_PREFIX: &[u8] = b"hirsel secret.wrap 1\0";
const INFO: &[u8] = b"hirsel secret.wrap 1 chacha20-poly1305";

/// A key operation's typed failure, decided where the key's bytes are read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyFailure {
    /// The blob's bound policy does not permit the requested uses.
    Denied,
    /// A KEK under `KEK_MIN` bytes, or a signing key that is not PKCS#8 for
    /// its algorithm.
    Invalid,
    /// The blob is not one this KEK sealed, unaltered. Nothing more is said.
    Tampered,
    /// The host provider must stop the run before it delivers an outcome.
    Provider(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignAlg {
    Ed25519,
    P256,
    Rs256,
}

impl SignAlg {
    /// The language's `SignAlg` case order.
    pub fn from_case(case: u64) -> Option<Self> {
        [Self::Ed25519, Self::P256, Self::Rs256]
            .get(usize::try_from(case).ok()?)
            .copied()
    }

    pub fn case(self) -> u8 {
        self as u8
    }

    /// The use that permits signing with this algorithm.
    pub fn needed(self) -> crate::SecretUse {
        match self {
            Self::Ed25519 => crate::SecretUse::SIGN_ED25519,
            Self::P256 => crate::SecretUse::SIGN_P256,
            Self::Rs256 => crate::SecretUse::SIGN_RS256,
        }
    }
}

/// The length of the blob that wraps `len` bytes bound to `policy`.
pub fn wrapped_len(policy: &str, len: usize) -> usize {
    1 + 4 + policy.len() + SALT + len + TAG
}

/// The length of the plaintext a well-formed `blob` holds, read from its
/// public header; `None` for a blob too short for its header and tag.
pub fn unwrapped_len(blob: &[u8]) -> Option<usize> {
    let (header, _) = header(blob)?;
    blob.len().checked_sub(header + TAG)
}

fn header(blob: &[u8]) -> Option<(usize, &str)> {
    let (&version, rest) = blob.split_first()?;
    if version != VERSION {
        return None;
    }
    let len = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let policy = std::str::from_utf8(rest.get(4..4 + len)?).ok()?;
    let end = 1 + 4 + len + SALT;
    (blob.len() >= end + TAG).then_some((end, policy))
}

fn key(kek: &[u8], salt: &[u8]) -> aead::LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(kek);
    let okm = prk
        .expand(&[INFO], &aead::CHACHA20_POLY1305)
        .expect("one ChaCha20-Poly1305 key is within HKDF-SHA256's output");
    aead::LessSafeKey::new(aead::UnboundKey::from(okm))
}

fn ad(header: &[u8]) -> aead::Aad<Vec<u8>> {
    aead::Aad::from([AD_PREFIX, header].concat())
}

/// A fresh random salt for `wrap_with_salt`.
pub fn salt() -> [u8; SALT] {
    rand::generate(&rand::SystemRandom::new())
        .expect("the OS random source")
        .expose()
}

/// `secret`, bound to the canonical `policy` text, sealed under `kek`:
/// `[1][len(policy) u32 LE][policy][salt 32][ciphertext][tag 16]`. Each
/// salt derives its own key, so the zero nonce is used once per key.
pub fn wrap_with_salt(
    kek: &[u8],
    policy: &str,
    secret: &[u8],
    salt: [u8; SALT],
) -> Result<Vec<u8>, KeyFailure> {
    if kek.len() < KEK_MIN {
        return Err(KeyFailure::Invalid);
    }
    let mut blob = Vec::with_capacity(wrapped_len(policy, secret.len()));
    blob.push(VERSION);
    blob.extend_from_slice(&(policy.len() as u32).to_le_bytes());
    blob.extend_from_slice(policy.as_bytes());
    blob.extend_from_slice(&salt);
    let header = blob.len();
    blob.extend_from_slice(secret);
    let (head, body) = blob.split_at_mut(header);
    let tag = key(kek, &salt)
        .seal_in_place_separate_tag(aead::Nonce::assume_unique_for_key([0; 12]), ad(head), body)
        .expect("a secret within ChaCha20-Poly1305's message limit");
    blob.extend_from_slice(tag.as_ref());
    Ok(blob)
}

/// Open `blob` under `kek` into `out` (wiped first), returning the bound
/// canonical policy text. On failure `out` holds no plaintext.
pub fn unwrap_into<'b>(
    kek: &[u8],
    blob: &'b [u8],
    out: &mut Vec<u8>,
) -> Result<&'b str, KeyFailure> {
    wipe(out);
    if kek.len() < KEK_MIN {
        return Err(KeyFailure::Invalid);
    }
    let (end, policy) = header(blob).ok_or(KeyFailure::Tampered)?;
    out.extend_from_slice(&blob[end..]);
    let salt = &blob[end - SALT..end];
    let opened = key(kek, salt)
        .open_in_place(
            aead::Nonce::assume_unique_for_key([0; 12]),
            ad(&blob[..end]),
            out,
        )
        .map(|plain| plain.len());
    match opened {
        Ok(len) => {
            out.truncate(len);
            Ok(policy)
        }
        Err(_) => {
            // ring leaves the buffer's contents unspecified on failure.
            wipe(out);
            Err(KeyFailure::Tampered)
        }
    }
}

fn wipe(out: &mut Vec<u8>) {
    for byte in out.iter_mut() {
        // SAFETY: a valid, exclusively borrowed byte.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    out.clear();
}

/// `requested` as unwrap's `uses`: within the policy bound in the blob.
pub fn within(requested: &SecretPolicy, bound: &str) -> Result<(), KeyFailure> {
    let bound = SecretPolicy::parse(bound).ok_or(KeyFailure::Tampered)?;
    if requested.within(&bound) {
        Ok(())
    } else {
        Err(KeyFailure::Denied)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigningRandom {
    Host,
    Virtual,
}

/// Sign `msg` with the PKCS#8 private key `pkcs8`: Ed25519 (RFC 8032),
/// ECDSA P-256 with SHA-256 as the 64-byte `r ‖ s`, or RSASSA-PKCS1-v1_5
/// with SHA-256 (RFC 8017 §8.2).
pub fn sign(
    alg: SignAlg,
    pkcs8: &[u8],
    msg: &[u8],
    random: SigningRandom,
) -> Result<Vec<u8>, KeyFailure> {
    let rng = rand::SystemRandom::new();
    match alg {
        SignAlg::Ed25519 => {
            let key = signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8)
                .map_err(|_| KeyFailure::Invalid)?;
            Ok(key.sign(msg).as_ref().to_vec())
        }
        SignAlg::P256 if random == SigningRandom::Virtual => {
            crate::p256_key::sign_virtual(pkcs8, msg).map(|signature| signature.to_vec())
        }
        SignAlg::P256 => crate::p256_key::HostSigner::load(pkcs8)?
            .sign(msg)
            .map(|signature| signature.to_vec()),
        SignAlg::Rs256 => {
            let key = signature::RsaKeyPair::from_pkcs8(pkcs8).map_err(|_| KeyFailure::Invalid)?;
            let mut sig = vec![0; key.public().modulus_len()];
            key.sign(&signature::RSA_PKCS1_SHA256, &rng, msg, &mut sig)
                .map_err(|_| KeyFailure::Invalid)?;
            Ok(sig)
        }
    }
}

/// The public key of the Ed25519 PKCS#8 private key `pkcs8`.
pub fn ed25519_public(pkcs8: &[u8]) -> Result<[u8; 32], KeyFailure> {
    use signature::KeyPair;
    let key = signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8)
        .map_err(|_| KeyFailure::Invalid)?;
    key.public_key()
        .as_ref()
        .try_into()
        .map_err(|_| KeyFailure::Invalid)
}

/// A fresh Ed25519 private key, as PKCS#8 v2, from the host's randomness.
pub fn ed25519_generate() -> Vec<u8> {
    signature::Ed25519KeyPair::generate_pkcs8(&rand::SystemRandom::new())
        .expect("the host's randomness")
        .as_ref()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    const KEK: [u8; 32] = {
        let mut k = [0; 32];
        let mut i = 0;
        while i < 32 {
            k[i] = i as u8;
            i += 1;
        }
        k
    };
    const POLICY: &str = "tls_send(api.example.test) mac";
    // Python `cryptography` 41.0.7: HKDF(SHA256, 32, salt = 0xa5 * 32,
    // info) over KEK 00..1f, then ChaCha20Poly1305 with a zero nonce and
    // AD "hirsel secret.wrap 1\0" ‖ header, over "refresh-token-1".
    const BLOB: &str = "011e000000746c735f73656e64286170692e6578616d706c652e7465737429206d6163a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a565df267a360e1edd7f03d085e720883fd265674611f4c163ee759710e65697";

    // A KDF, layout or associated-data change breaks the known answer, and a
    // header byte outside the AD (the bound policy among them) would open.
    #[test]
    fn wrap_matches_python_and_any_flipped_byte_is_tampered() {
        let blob = wrap_with_salt(&KEK, POLICY, b"refresh-token-1", [0xa5; 32]).unwrap();
        assert_eq!(blob, hex(BLOB));
        let mut out = Vec::new();
        assert_eq!(unwrap_into(&KEK, &blob, &mut out), Ok(POLICY));
        assert_eq!(out, b"refresh-token-1");
        for at in 0..blob.len() {
            let mut bad = blob.clone();
            bad[at] ^= 1;
            assert_eq!(
                unwrap_into(&KEK, &bad, &mut out),
                Err(KeyFailure::Tampered),
                "byte {at}"
            );
            assert!(out.is_empty());
        }
        let mut other = KEK;
        other[0] ^= 1;
        assert_eq!(
            unwrap_into(&other, &blob, &mut out),
            Err(KeyFailure::Tampered)
        );
        assert_eq!(
            wrap_with_salt(&KEK[..31], POLICY, b"x", [0; 32]),
            Err(KeyFailure::Invalid)
        );
    }

    // Unwrap's `uses` may only shrink the bound policy (by hand from TLS-3).
    #[test]
    fn unwrap_uses_stay_within_the_bound_policy() {
        let verdict = |uses: &str| within(&SecretPolicy::parse(uses).unwrap(), POLICY);
        assert_eq!(verdict("mac"), Ok(()));
        assert_eq!(verdict("verify"), Ok(()));
        assert_eq!(verdict("tls_send(api.example.test)"), Ok(()));
        assert_eq!(verdict("tls_send(*.example.test)"), Err(KeyFailure::Denied));
        assert_eq!(
            verdict("tls_send(api.example.test,other.test)"),
            Err(KeyFailure::Denied)
        );
        assert_eq!(verdict("mac wrap"), Err(KeyFailure::Denied));
        assert_eq!(verdict("sign(ed25519)"), Err(KeyFailure::Denied));
        let any = SecretPolicy::parse("tls_send(*.example.test) verify").unwrap();
        assert!(
            SecretPolicy::parse("tls_send(a.example.test)")
                .unwrap()
                .within(&any)
        );
    }

    // RFC 8032 §7.1 TEST 1, its seed in a PKCS#8 v1 envelope (RFC 8410).
    #[test]
    fn ed25519_signs_rfc8032_test_1() {
        let pkcs8 = hex(concat!(
            "302e020100300506032b657004220420",
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
        ));
        assert_eq!(
            sign(SignAlg::Ed25519, &pkcs8, b"", SigningRandom::Host),
            Ok(hex(concat!(
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
                "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
            )))
        );
    }

    // Wycheproof rsa_pkcs1_2048_sig_gen_test.json, the SHA-256 group:
    // RSASSA-PKCS1-v1_5 is deterministic, so each signature is exact.
    #[test]
    fn rs256_signs_wycheproof_sig_gen() {
        let pkcs8 = hex(concat!(
            "308204bd020100300d06092a864886f70d0101010500048204a7308204a30201000282010100a2b451a07d0aa5f96e45",
            "5671513550514a8a5b462ebef717094fa1fee82224e637f9746d3f7cafd31878d80325b6ef5a1700f65903b469429e89",
            "d6eac8845097b5ab393189db92512ed8a7711a1253facd20f79c15e8247f3d3e42e46e48c98e254a2fe9765313a03eff",
            "8f17e1a029397a1fa26a8dce26f490ed81299615d9814c22da610428e09c7d9658594266f5c021d0fceca08d945a12be",
            "82de4d1ece6b4c03145b5d3495d4ed5411eb878daf05fd7afc3e09ada0f1126422f590975a1969816f48698bcbba1b4d",
            "9cae79d460d8f9f85e7975005d9bc22c4e5ac0f7c1a45d12569a62807d3b9a02e5a530e773066f453d1f5b4c2e9cf782",
            "0283f742b9d50203010001028201007627eef3567b2a27268e52053ecd31c3a7172ccb9ddcee819b306a5b3c66b7573c",
            "a4fa88efc6f3c4a00bfa0ae7139f64543a4dac3d05823f6ff477cfcec84fe2ac7a68b17204b390232e110310c4e899c4",
            "e7c10967db4acde042dbbf19dbe00b4b4741de1020aaaaffb5054c797c9f136f7d93ac3fc8caff6654242d7821ebee51",
            "7bf537f44366a0fdd45ae05b9909c2e6cc1ed9281eff4399f76c96b96233ec29ae0bbf0d752b234fc197389f51050aa1",
            "acd01c074c3ac8fbdb9ea8b651a95995e8db4ad5c43b6c8673e5a126e7ee94b8dff4c5afc01259bc8da76950bae6f8ba",
            "e715f50985b0d6f66d04c6fef3b700720eecdcdf171bb7b1ecbe7289c467c102818100dc431050f782e894fb5248247d",
            "98cb7d58b8d1e24f3b55d041c56e4de086b0d5bb028bda42eeb5d234d5681e5809d415e6a289ad4cfbf78f978f6c3581",
            "4f50eebff1c5b80a69f788e81e6bab5ddaa78369d659d143ec6f17e79813a575cfad9c569156b90113e2e9110ad9e7b4",
            "8a1c9348a6e653321191290ea36cfb3a5b18f102818100bd1a81e7977f9898122273ae3222b598ea5fb19eb4eabc3830",
            "8a5e32196603b2e500ffb79f5b886816611debc472fac45544070beb057c941378a6868af3b7a03d3f9880ec47d5e089",
            "b94fbde542aba9ae8d72c57088d7abf5b131f39098f7bc160f90536abc9492fd4e06f3ed7299d4b97bb03677207d9566",
            "9f140cfbc20f2502818100a94b528b28f291599121d91952ffd1c7f21d7c1479d99d478885fb161870ee1218bf084726",
            "12dbe5497e8d9c650688e09c786961ae3e2c354dc48ae34514759c4c23c4588488961dc06b414e61c0e1e7fbbd2923d3",
            "1532fe289f96da220711e58c14019808e00414276933bb07e4efb9b4a9b37656917205209f33f09515d7c10281803af0",
            "e72a933aef09ff2503df78bafed531c02ff1a2bc437c540cdcbd4ad35435cf511763596543480629b114ca7f780ff7ef",
            "a32ea0cb6e000d6d9ea1f2ef71fd9cf9948422a165557e37e755edfe70d90b920502eb478bc98a63f788ce3a0f856d6e",
            "de7251a383bfa8fa480a81a925af7b3cc538c4bab8c9f7597ffb68011d8d0281802640fbfbcfefb163ee7a87b6483a66",
            "ee41f956d90fa8a7939bfc042ee0924b1b7993d0445f758d51933e85179c0320b0c968b48a91c38b5be923e1097c0c56",
            "2f88d42294b6a2759bafa5428a74f1270874e45f6fcc60f21602de5eccd143cf31241f5921b5ad3983fb54ef17be3b28",
            "5367e50c999c67247b552fe4bfce945f7b",
        ));
        for (msg, sig) in [
            (
                "",
                "840f5dac53106dd1f9c57219224cf51289290c42f20466875ba8e830ac5690e541536fcc8ab03b731f82bf66d83f194e7e180b3963ec7a2f3f7904a7ce49aed47da4d4b79421eaf937d301b3e696169297b797c32c076a12be4de0b58e003c5123051a84a10c62f8dac2f42a8640008eb3c7cccd6760ff5b51b689763922582845f048fb8150e5a7a6ca2eccc7bdc85349ad5b26c52137a79fa3fe5c29ab5cd7615013219c1941b6708e9c3c23feff5febaf0c8ebca5750b54e3e6e99a3e876b396f27860b7f3ec4e9191703c6332d944f6f69751167680c79c4f6b57f1cc8755d24b6ec158ccdbacdb23107a33cb6b332516c13274d1f9dccc21dced869e486",
            ),
            (
                "0000000000000000000000000000000000000000",
                "8a1b220cb2ab415dc760eb7f5bb10335a3cca269d7dbbf7d0962ba79f9cf7b43a5fc09c99a1584f07403473d6c189a836897a5b6f8ea9fa22d601e6ba5f7411fe27c638b81b1a22363583a80fce8c7df3e40fb51bd0e60d0a6653f79f3bcb7ec3e9dc14cfb5b31ab1735bca692d50ac03f979dda92747c6430f8045efa3513ba6e0ce3e9e35570e1c30c8ebe589b44192e1344ca83dfa576fc6fdc7bf1cd7cee875b001c8c02ce8d602769e4bd9d241c4857182a0089a8b67644e73eef105c550efa47a40874289395ac0c4e02fd4ba98e130a4c2d1b95521c6af4a002ac3bdc6e52122ae4c08cc3da1c896e059acbddec574ac0432f6103dd97273d8803c102",
            ),
            (
                "54657374",
                "264491e844c119f14e425c03282139a558dcdaeb82a4628173cd407fd319f9076eaebc0dd87a1c22e4d17839096886d58a9d5b7f7aeb63efec56c45ac7bead4203b6886e1faa90e028ec0ae094d46bf3f97efdd19045cfbc25a1abda2432639f9876405c0d68f8edbf047c12a454f7681d5d5a2b54bd3723d193dbad4338baad753264006e2d08931c4b8bb79aa1c9cad10eb6605f87c5831f6e2b08e002f9c6f21141f5841d92727dd3e1d99c36bc560da3c9067df99fcaf818941f72588be33032bad22caf6704223bb114d575b6d02d9d222b580005d930e8f40cce9f672eebb634a20177d84351627964b83f2053d736a84ab1a005f63bd5ba943de6205c",
            ),
        ] {
            assert_eq!(
                sign(SignAlg::Rs256, &pkcs8, &hex(msg), SigningRandom::Host),
                Ok(hex(sig)),
                "{msg}"
            );
        }
        assert_eq!(
            sign(SignAlg::P256, &pkcs8, b"", SigningRandom::Host),
            Err(KeyFailure::Invalid)
        );
    }

    // ECDSA signing is randomized: a key generated by Python `cryptography`
    // 41.0.7 signs, and its independently encoded public point verifies.
    #[test]
    fn p256_signature_verifies_under_python_public_key() {
        let pkcs8 = hex(concat!(
            "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b0201010420",
            "59c22f6bd5d13c7d013e4a763c58c909b2e15f1c883a329c43139a3c54eb07a9a144034200",
            "040ef3a7e79b1a04c1a1bdcefb8f7ec7129f73b2b8cd3a9d030efedf7cb5639cdbae621895",
            "d9512640d18b7666a53ae59d710f91c0b9d34c3a3cf6efb2ae0b6fb4"
        ));
        let public = hex(concat!(
            "040ef3a7e79b1a04c1a1bdcefb8f7ec7129f73b2b8cd3a9d030efedf7cb5639cdb",
            "ae621895d9512640d18b7666a53ae59d710f91c0b9d34c3a3cf6efb2ae0b6fb4"
        ));
        let sig = sign(SignAlg::P256, &pkcs8, b"hirsel", SigningRandom::Host).unwrap();
        assert_eq!(sig.len(), 64);
        assert!(crate::verify_signature(
            crate::SignatureAlgorithm::P256Fixed,
            &public,
            b"hirsel",
            &sig
        ));
        assert!(!crate::verify_signature(
            crate::SignatureAlgorithm::P256Fixed,
            &public,
            b"hirsel!",
            &sig
        ));
        assert_eq!(
            sign(SignAlg::Ed25519, &pkcs8, b"", SigningRandom::Host),
            Err(KeyFailure::Invalid)
        );
    }
}

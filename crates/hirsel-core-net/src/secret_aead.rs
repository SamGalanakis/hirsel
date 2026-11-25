//! The TLS-5 AEAD sink over ring.

use crate::{
    SecretUse,
    key_wrap::KeyFailure,
    packet_cipher::{PacketCipher, TAG_LEN},
};
use ring::aead;
use std::{mem::MaybeUninit, ops::Deref, sync::OnceLock};
use zeroize::Zeroizing;

#[cfg(test)]
thread_local! {
    static EXPANSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AeadAlg {
    ChaCha20Poly1305,
    Aes128Gcm,
    Aes256Gcm,
}

impl AeadAlg {
    pub fn from_case(case: u64) -> Option<Self> {
        match case {
            0 => Some(Self::ChaCha20Poly1305),
            1 => Some(Self::Aes128Gcm),
            2 => Some(Self::Aes256Gcm),
            _ => None,
        }
    }

    pub fn case(self) -> u8 {
        self as u8
    }

    pub(super) fn algorithm(self) -> &'static aead::Algorithm {
        match self {
            Self::ChaCha20Poly1305 => &aead::CHACHA20_POLY1305,
            Self::Aes128Gcm => &aead::AES_128_GCM,
            Self::Aes256Gcm => &aead::AES_256_GCM,
        }
    }

    pub(super) fn max_input(self) -> u64 {
        match self {
            Self::ChaCha20Poly1305 => (1 << 38) - 64,
            Self::Aes128Gcm | Self::Aes256Gcm => (1 << 36) - 32,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AeadMode {
    Seal,
    Open,
}

#[derive(Debug, Eq, PartialEq)]
pub enum AeadRefusal {
    Denied,
    Invalid,
    Capacity { requested: usize, available: usize },
}

pub struct AeadInput<'a> {
    pub alg: AeadAlg,
    pub mode: AeadMode,
    pub nonce: &'a [u8],
    pub aad: &'a [u8],
    pub bytes: &'a [u8],
}

/// Owned bytes shared by the designations of a Secret. Cache an expanded key
/// only when an authorized sink first uses that algorithm. The cache belongs
/// to the bytes, so narrowing and sharing a designation do not expand again.
pub struct SecretBytes {
    bytes: Zeroizing<Vec<u8>>,
    aead: [OnceLock<Result<Box<PacketCipher>, KeyFailure>>; 3],
}

impl SecretBytes {
    pub fn new(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self {
            bytes,
            aead: Default::default(),
        }
    }

    /// The bytes, for an import that takes custody of them (TLS-8, imports).
    pub fn into_bytes(self) -> Zeroizing<Vec<u8>> {
        self.bytes
    }

    fn key(&self, alg: AeadAlg) -> Result<&PacketCipher, KeyFailure> {
        self.aead[usize::from(alg.case())]
            .get_or_init(|| expand(alg, &self.bytes).map(Box::new))
            .as_deref()
            .map_err(|&failure| failure)
    }
}

impl Deref for SecretBytes {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

/// The size to reserve in the caller's spare capacity before issuing a sink.
pub fn output_len(
    policy: SecretUse,
    alg: AeadAlg,
    mode: AeadMode,
    nonce_len: usize,
    input_len: usize,
    spare: usize,
) -> Result<usize, AeadRefusal> {
    if !policy.permits(SecretUse::aead(alg, mode)) {
        return Err(AeadRefusal::Denied);
    }
    let (message_len, requested) = match mode {
        AeadMode::Seal => (input_len, input_len.saturating_add(TAG_LEN)),
        AeadMode::Open => {
            let len = input_len.saturating_sub(TAG_LEN);
            (len, len)
        }
    };
    if nonce_len != aead::NONCE_LEN || message_len as u64 > alg.max_input() {
        return Err(AeadRefusal::Invalid);
    }
    if requested > spare {
        return Err(AeadRefusal::Capacity {
            requested,
            available: spare,
        });
    }
    Ok(requested)
}

impl AeadInput<'_> {
    /// Run inside the provider. `out` is the spare selected by `output_len`.
    /// The caller publishes its bytes only on success.
    pub fn apply(&self, key: &SecretBytes, out: &mut [MaybeUninit<u8>]) -> Result<(), KeyFailure> {
        let key = key.key(self.alg)?;
        let nonce = self.nonce.try_into().map_err(|_| KeyFailure::Invalid)?;
        match self.mode {
            AeadMode::Seal => {
                key.seal_into(nonce, self.aad, self.bytes, out)
                    .expect("AEAD message length checked before issue");
                Ok(())
            }
            AeadMode::Open => key
                .open_into(nonce, self.aad, self.bytes, out)
                .map_err(|_| KeyFailure::Tampered),
        }
    }
}

fn expand(alg: AeadAlg, key: &[u8]) -> Result<PacketCipher, KeyFailure> {
    #[cfg(test)]
    EXPANSIONS.update(|n| n + 1);

    PacketCipher::new(alg, key).map_err(|_| KeyFailure::Invalid)
}

#[cfg(test)]
#[test]
fn seal_and_open_expand_one_key() {
    // SP 800-38D AES-128-GCM with a zero key, IV and plaintext block.
    const CIPHERTEXT_AND_TAG: [u8; 32] = [
        0x03, 0x88, 0xda, 0xce, 0x60, 0xb6, 0xa3, 0x92, 0xf3, 0x28, 0xc2, 0xb9, 0x71, 0xb2, 0xfe,
        0x78, 0xab, 0x6e, 0x47, 0xd4, 0x2c, 0xec, 0x13, 0xbd, 0xf5, 0x3a, 0x67, 0xb2, 0x12, 0x57,
        0xbd, 0xdf,
    ];
    let key = SecretBytes::new(Zeroizing::new(vec![0; 16]));
    let plain = [0; 16];
    let nonce = [0; 12];
    let mut op = AeadInput {
        alg: AeadAlg::Aes128Gcm,
        mode: AeadMode::Seal,
        nonce: &nonce,
        aad: &[],
        bytes: &plain,
    };
    let mut sealed = [MaybeUninit::uninit(); 32];
    op.apply(&key, &mut sealed).unwrap();
    // SAFETY: apply initialized all 32 bytes of the output on success.
    let sealed = sealed.map(|byte| unsafe { byte.assume_init() });
    assert_eq!(sealed, CIPHERTEXT_AND_TAG);

    op.mode = AeadMode::Open;
    op.bytes = &sealed;
    let mut opened = [MaybeUninit::uninit(); 16];
    op.apply(&key, &mut opened).unwrap();
    // SAFETY: apply initialized all 16 bytes of the output on success.
    let opened = opened.map(|byte| unsafe { byte.assume_init() });
    assert_eq!(opened, plain);
    assert_eq!(EXPANSIONS.get(), 1);
}

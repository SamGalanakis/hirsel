//! The TLS-5 AEAD sink over ring.

use crate::{SecretUse, key_wrap::KeyFailure};
use ring::aead;
use std::mem::MaybeUninit;

const TAG_LEN: usize = 16;

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

    fn algorithm(self) -> &'static aead::Algorithm {
        match self {
            Self::ChaCha20Poly1305 => &aead::CHACHA20_POLY1305,
            Self::Aes128Gcm => &aead::AES_128_GCM,
            Self::Aes256Gcm => &aead::AES_256_GCM,
        }
    }

    fn max_input(self) -> u64 {
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
    pub fn apply(&self, key: &[u8], out: &mut [MaybeUninit<u8>]) -> Result<(), KeyFailure> {
        let key =
            aead::UnboundKey::new(self.alg.algorithm(), key).map_err(|_| KeyFailure::Invalid)?;
        let key = aead::LessSafeKey::new(key);
        let nonce =
            aead::Nonce::try_assume_unique_for_key(self.nonce).map_err(|_| KeyFailure::Invalid)?;
        let aad = aead::Aad::from(self.aad);
        match self.mode {
            AeadMode::Seal => {
                let (body, tail) = out.split_at_mut(self.bytes.len());
                let body = body.write_copy_of_slice(self.bytes);
                let tag = key
                    .seal_in_place_separate_tag(nonce, aad, body)
                    .expect("AEAD message length checked before issue");
                tail.write_copy_of_slice(tag.as_ref());
            }
            AeadMode::Open => {
                let len = self
                    .bytes
                    .len()
                    .checked_sub(TAG_LEN)
                    .ok_or(KeyFailure::Tampered)?;
                let (body, tag) = self.bytes.split_at(len);
                let tag = aead::Tag::try_from(tag).map_err(|_| KeyFailure::Tampered)?;
                let body = out.write_copy_of_slice(body);
                // ring clears the decrypted bytes if authentication fails.
                key.open_in_place_separate_tag(nonce, aad, tag, body, 0..)
                    .map_err(|_| KeyFailure::Tampered)?;
            }
        }
        Ok(())
    }
}

//! Expanded packet keys shared by both execution engines. Callers own
//! counters and nonce construction. The runtime's VAES backend can write
//! directly to a disjoint destination; these ring paths copy when necessary.
use crate::secret_aead::AeadAlg;
use ring::aead::{self, Aad, LessSafeKey, Nonce, Tag, UnboundKey, quic};
use std::mem::MaybeUninit;

pub const TAG_LEN: usize = 16;

/// Owns one expansion. A new expansion is required for a different key.
pub struct PacketCipher(LessSafeKey);

impl PacketCipher {
    pub fn new(algorithm: AeadAlg, bytes: &[u8]) -> Result<Self, ring::error::Unspecified> {
        UnboundKey::new(algorithm.algorithm(), bytes).map(|key| Self(LessSafeKey::new(key)))
    }

    pub fn seal(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], ring::error::Unspecified> {
        let tag = self.0.seal_in_place_separate_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            body,
        )?;
        Ok(tag.as_ref().try_into().unwrap())
    }

    pub fn seal_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        plain: &[u8],
        sealed: &mut [MaybeUninit<u8>],
    ) -> Result<(), ring::error::Unspecified> {
        assert_eq!(sealed.len(), plain.len() + TAG_LEN);
        let (body, tag) = sealed.split_at_mut(plain.len());
        let result = self.seal(nonce, aad, body.write_copy_of_slice(plain))?;
        tag.write_copy_of_slice(&result);
        Ok(())
    }

    /// A tag mismatch clears the ciphertext span that was decrypted.
    pub fn open(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), ring::error::Unspecified> {
        self.0.open_in_place_separate_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            Tag::from(*tag),
            body,
            0..,
        )?;
        Ok(())
    }

    pub fn open_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        sealed: &[u8],
        plain: &mut [MaybeUninit<u8>],
    ) -> Result<(), ring::error::Unspecified> {
        let bytes = sealed
            .len()
            .checked_sub(TAG_LEN)
            .ok_or(ring::error::Unspecified)?;
        let (body, tag) = sealed.split_at(bytes);
        self.open(
            nonce,
            aad,
            plain.write_copy_of_slice(body),
            tag.try_into().unwrap(),
        )
    }
}

/// A separately derived AES or ChaCha20 key that can only mask headers.
pub struct HeaderMask(quic::HeaderProtectionKey);

impl HeaderMask {
    pub fn new(algorithm: AeadAlg, bytes: &[u8]) -> Result<Self, ring::error::Unspecified> {
        let algorithm = match algorithm {
            AeadAlg::Aes128Gcm => &aead::quic::AES_128,
            AeadAlg::Aes256Gcm => &aead::quic::AES_256,
            AeadAlg::ChaCha20Poly1305 => &aead::quic::CHACHA20,
        };
        quic::HeaderProtectionKey::new(algorithm, bytes).map(Self)
    }

    pub fn mask(&self, sample: &[u8; 16]) -> [u8; 5] {
        self.0
            .new_mask(sample)
            .expect("a header sample is 16 bytes")
    }
}

#[cfg(test)]
mod tests;

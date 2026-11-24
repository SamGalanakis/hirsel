use hirsel_core_net::{
    packet_cipher::{PacketCipher as Portable, TAG_LEN},
    secret_aead::AeadAlg,
};
use ring::error::Unspecified;
use std::mem::MaybeUninit;

#[cfg(target_arch = "x86_64")]
const GCM_MAX_BODY: u64 = (1 << 36) - 32;

/// A provider-owned expanded AEAD key. The portable ring key is only built
/// when the runtime cannot select VAES.
pub struct PacketCipher(Backend);

enum Backend {
    #[cfg(target_arch = "x86_64")]
    Vaes(Box<super::gcm::GcmKey>),
    Ring(Portable),
}

impl PacketCipher {
    pub fn new(alg: AeadAlg, bytes: &[u8]) -> Result<Self, Unspecified> {
        #[cfg(target_arch = "x86_64")]
        if matches!(
            (alg, bytes.len()),
            (AeadAlg::Aes128Gcm, 16) | (AeadAlg::Aes256Gcm, 32)
        ) && let Some(vaes) = super::gcm::GcmKey::new(bytes)
        {
            return Ok(Self(Backend::Vaes(vaes)));
        }
        Portable::new(alg, bytes).map(|key| Self(Backend::Ring(key)))
    }

    pub fn writes_into(&self) -> bool {
        match self.0 {
            #[cfg(target_arch = "x86_64")]
            Backend::Vaes(_) => true,
            Backend::Ring(_) => false,
        }
    }

    pub fn seal_record(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        content: u8,
        out: &mut [u8],
    ) -> Result<(), Unspecified> {
        assert_eq!(out.len(), input.len() + 1 + TAG_LEN);
        match &self.0 {
            #[cfg(target_arch = "x86_64")]
            Backend::Vaes(key) => {
                check_gcm_lengths(input.len() + 1, aad.len())?;
                key.seal_record(nonce, aad, input, content, out);
            }
            Backend::Ring(key) => {
                let (body, tag) = out.split_at_mut(input.len() + 1);
                body[..input.len()].copy_from_slice(input);
                body[input.len()] = content;
                tag.copy_from_slice(&key.seal(nonce, aad, body)?);
            }
        }
        Ok(())
    }

    pub fn seal(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Unspecified> {
        match &self.0 {
            #[cfg(target_arch = "x86_64")]
            Backend::Vaes(key) => {
                check_gcm_lengths(body.len(), aad.len())?;
                Ok(key.seal(nonce, aad, body))
            }
            Backend::Ring(key) => key.seal(nonce, aad, body),
        }
    }

    pub fn seal_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        out: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        match &self.0 {
            #[cfg(target_arch = "x86_64")]
            Backend::Vaes(key) => {
                assert_eq!(out.len(), input.len() + TAG_LEN);
                check_gcm_lengths(input.len(), aad.len())?;
                let (body, tag) = out.split_at_mut(input.len());
                tag.write_copy_of_slice(&key.seal_into(nonce, aad, input, body));
                Ok(())
            }
            Backend::Ring(key) => key.seal_into(nonce, aad, input, out),
        }
    }

    pub fn open<'a>(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        ciphertext: &'a mut [u8],
    ) -> Result<&'a [u8], Unspecified> {
        let len = ciphertext.len().checked_sub(TAG_LEN).ok_or(Unspecified)?;
        let (body, tag) = ciphertext.split_at_mut(len);
        let tag = (&*tag).try_into().unwrap();
        match &self.0 {
            #[cfg(target_arch = "x86_64")]
            Backend::Vaes(key) => {
                check_gcm_lengths(len, aad.len())?;
                key.open(nonce, aad, body, tag)
                    .then_some(())
                    .ok_or(Unspecified)?;
            }
            Backend::Ring(key) => key.open(nonce, aad, body, tag)?,
        }
        Ok(body)
    }

    pub fn open_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        out: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        match &self.0 {
            #[cfg(target_arch = "x86_64")]
            Backend::Vaes(key) => {
                let len = input.len().checked_sub(TAG_LEN).ok_or(Unspecified)?;
                check_gcm_lengths(len, aad.len())?;
                let (body, tag) = input.split_at(len);
                key.open_into(nonce, aad, body, tag.try_into().unwrap(), out)
                    .then_some(())
                    .ok_or(Unspecified)
            }
            Backend::Ring(key) => key.open_into(nonce, aad, input, out),
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn check_gcm_lengths(body: usize, aad: usize) -> Result<(), Unspecified> {
    (body as u64 <= GCM_MAX_BODY && aad as u64 <= u64::MAX / 8)
        .then_some(())
        .ok_or(Unspecified)
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests;
impl hirsel_core_net::packet_protection::Aead for PacketCipher {
    fn expand(algorithm: AeadAlg, key: &[u8]) -> Result<Self, Unspecified> {
        Self::new(algorithm, key)
    }

    fn seal(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Unspecified> {
        self.seal(nonce, aad, body)
    }

    fn seal_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        plain: &[u8],
        sealed: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        self.seal_into(nonce, aad, plain, sealed)
    }

    fn open(&self, nonce: [u8; 12], aad: &[u8], sealed: &mut [u8]) -> Result<(), Unspecified> {
        self.open(nonce, aad, sealed).map(|_| ())
    }

    fn open_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        sealed: &[u8],
        plain: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        self.open_into(nonce, aad, sealed, plain)
    }
}

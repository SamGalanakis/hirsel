//! RFC 3711/7714 session material, derived once for each protocol space.
use super::{BLOCK, SrtpCipher, SrtpKdf};
use crate::{ImportUse, packet_epoch::ImportError, secret_aead::AeadAlg};
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub enum SrtpProfile {
    Aes128CmHmacSha1_80,
    AeadAes128Gcm,
    AeadAes256Gcm,
}

#[derive(Clone, Copy)]
pub enum SrtpSpace {
    Rtp,
    Rtcp,
}

pub struct SrtpKeys {
    send: bool,
    spaces: [SrtpSessionKey; 2],
}

pub struct SrtpSessionKey {
    space: SrtpSpace,
    payload: Zeroizing<[u8; 32]>,
    auth: Option<Zeroizing<[u8; 20]>>,
    salt: Zeroizing<[u8; 14]>,
    algorithm: Option<AeadAlg>,
}

impl SrtpKeys {
    /// This import constructs just one direction. The use is resolved from
    /// the designation's policy before the master bytes are read.
    pub fn import(
        profile: SrtpProfile,
        authority: ImportUse,
        master: &[u8],
    ) -> Result<Self, ImportError> {
        let send = match authority {
            ImportUse::SrtpSend => true,
            ImportUse::SrtpReceive => false,
            _ => return Err(ImportError::Denied),
        };
        let algorithm = match profile {
            SrtpProfile::Aes128CmHmacSha1_80 => None,
            SrtpProfile::AeadAes128Gcm => Some(AeadAlg::Aes128Gcm),
            SrtpProfile::AeadAes256Gcm => Some(AeadAlg::Aes256Gcm),
        };
        let key_len = if matches!(profile, SrtpProfile::AeadAes256Gcm) {
            32
        } else {
            16
        };
        let salt_len = if algorithm.is_some() { 12 } else { 14 };
        if master.len() != key_len + 14 && master.len() != key_len + salt_len {
            return Err(ImportError::Invalid);
        }
        let (key, salt) = master.split_at(key_len);
        let kdf = SrtpKdf::new(key, salt).map_err(|_| ImportError::Invalid)?;
        let spaces = [SrtpSpace::Rtp, SrtpSpace::Rtcp].map(|space| {
            let label = (space as u8) * 3;
            let mut payload = Zeroizing::new([0; 32]);
            let mut salt = Zeroizing::new([0; 14]);
            kdf.derive(label, &mut payload[..key_len]);
            kdf.derive(label + 2, &mut salt[..salt_len]);
            let auth = algorithm.is_none().then(|| {
                let mut auth = Zeroizing::new([0; 20]);
                kdf.derive(label + 1, auth.as_mut());
                auth
            });
            SrtpSessionKey {
                space,
                payload,
                auth,
                salt,
                algorithm,
            }
        });
        Ok(Self { send, spaces })
    }

    pub fn keys(&self, space: SrtpSpace, sending: bool) -> Option<&SrtpSessionKey> {
        (sending == self.send).then(|| &self.spaces[space as usize])
    }
}

impl SrtpSessionKey {
    pub fn expand_cm(&self) -> Option<SrtpCipher> {
        let auth = self.auth.as_ref()?;
        Some(SrtpCipher::new(self.payload[..16].try_into().unwrap(), auth).unwrap())
    }

    pub fn cm_nonce(&self, ssrc: u32, index: u64) -> Option<[u8; BLOCK]> {
        self.auth.as_ref()?;
        let start = match self.space {
            SrtpSpace::Rtp if index < (1 << 48) => 8,
            SrtpSpace::Rtcp if index < (1 << 31) => 10,
            _ => return None,
        };
        let mut nonce = [0; BLOCK];
        nonce[..14].copy_from_slice(&*self.salt);
        for (byte, word) in nonce[4..8].iter_mut().zip(ssrc.to_be_bytes()) {
            *byte ^= word;
        }
        for (byte, word) in nonce[start..14]
            .iter_mut()
            .zip(&index.to_be_bytes()[start - 6..])
        {
            *byte ^= word;
        }
        Some(nonce)
    }

    pub fn aead_material(&self) -> Option<(AeadAlg, &[u8], [u8; 12])> {
        let algorithm = self.algorithm?;
        let len = algorithm.algorithm().key_len();
        Some((
            algorithm,
            &self.payload[..len],
            self.salt[..12].try_into().unwrap(),
        ))
    }
}

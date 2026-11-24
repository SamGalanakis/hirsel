//! RFC 3711/7714 session material, derived once for each protocol space.
use super::{BLOCK, SrtpCipher, SrtpKdf};
use crate::{
    ImportUse,
    packet_epoch::ImportError,
    packet_nonce::{NonceFormat, cm_nonce},
    secret_aead::AeadAlg,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub enum SrtpProfile {
    Aes128CmHmacSha1_80,
    AeadAes128Gcm,
    AeadAes256Gcm,
}

impl SrtpProfile {
    pub fn from_case(case: u64) -> Option<Self> {
        Some(match case {
            0 => Self::Aes128CmHmacSha1_80,
            1 => Self::AeadAes128Gcm,
            2 => Self::AeadAes256Gcm,
            _ => return None,
        })
    }

    pub(crate) fn master_widths(self) -> (usize, usize) {
        match self {
            Self::Aes128CmHmacSha1_80 => (16, 14),
            Self::AeadAes128Gcm => (16, 12),
            Self::AeadAes256Gcm => (32, 12),
        }
    }

    pub(crate) fn algorithm(self) -> Option<AeadAlg> {
        match self {
            Self::Aes128CmHmacSha1_80 => None,
            Self::AeadAes128Gcm => Some(AeadAlg::Aes128Gcm),
            Self::AeadAes256Gcm => Some(AeadAlg::Aes256Gcm),
        }
    }
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
        let (key_len, salt_len) = profile.master_widths();
        if master.len() != key_len + 14 && master.len() != key_len + salt_len {
            return Err(ImportError::Invalid);
        }
        let (key, salt) = master.split_at(key_len);
        Ok(Self::from_master(profile, send, key, salt))
    }

    pub(crate) fn from_master(profile: SrtpProfile, send: bool, key: &[u8], salt: &[u8]) -> Self {
        let algorithm = profile.algorithm();
        let (key_len, salt_len) = profile.master_widths();
        let kdf = SrtpKdf::new(key, salt).expect("profile fixes the master widths");
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
        Self { send, spaces }
    }

    pub(crate) fn into_spaces(self) -> [SrtpSessionKey; 2] {
        self.spaces
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

    pub(crate) fn salt(&self) -> &[u8; 14] {
        &self.salt
    }

    pub fn cm_nonce(&self, ssrc: u32, index: u64) -> Option<[u8; BLOCK]> {
        self.auth.as_ref()?;
        let format = match self.space {
            SrtpSpace::Rtp if index < (1 << 48) => NonceFormat::SrtpRtp,
            SrtpSpace::Rtcp if index < (1 << 31) => NonceFormat::SrtpRtcp,
            _ => return None,
        };
        Some(cm_nonce(format, self.salt(), ssrc, index))
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

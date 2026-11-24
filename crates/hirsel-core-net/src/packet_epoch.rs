//! Packet key ownership for one epoch. Issued resources keep their own keys.
use crate::{
    ImportUse,
    packet_cipher::HeaderMask,
    packet_nonce::{Base, NonceFormat, ReceiverNonce, SenderCounter},
    packet_protection::{Algorithm, Key},
    secret_aead::AeadAlg,
    srtp_cipher::{SrtpProfile, SrtpSessionKey},
    tls_kdf::{Side, Tls13Labels, Tls13PacketKeys, Tls13Traffic},
};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

pub const SCHEDULE_CHARGE: u64 = 512;
pub const CIPHER_CHARGE: u64 = 1024;
pub const DOMAIN_CHARGE: u64 = 16;
pub const MASK_CHARGE: u64 = 512;

pub enum DirectionKeys {
    Missing,
    Opaque,
    Live(Tls13PacketKeys),
    Cm(SrtpSessionKey),
}

impl DirectionKeys {
    fn validate(&self) -> Result<(), IssueError> {
        match self {
            Self::Missing => Err(IssueError::Invalid),
            _ => Ok(()),
        }
    }

    fn key(&self) -> Option<Key<'_>> {
        match self {
            Self::Live(keys) => Some(Key::Aead(keys.payload())),
            Self::Cm(keys) => Some(Key::Cm(keys)),
            Self::Opaque => None,
            Self::Missing => unreachable!("the caller validated direction"),
        }
    }

    fn base(&self, algorithm: Algorithm) -> Base {
        match algorithm {
            Algorithm::Aead(_) => {
                let iv = match self {
                    Self::Live(keys) => keys.iv(),
                    _ => [0; 12],
                };
                Base::Aead(Zeroizing::new(iv))
            }
            Algorithm::Cm => {
                let salt = match self {
                    Self::Cm(keys) => *keys.salt(),
                    _ => [0; 14],
                };
                Base::Cm(Zeroizing::new(salt))
            }
        }
    }

    fn erase(&mut self) {
        if matches!(self, Self::Live(_) | Self::Cm(_)) {
            *self = Self::Opaque;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueError {
    Invalid,
    Issued,
    Spent,
    Capacity { requested: u64, available: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportError {
    Denied,
    Invalid,
}

pub struct Epoch<C> {
    format: NonceFormat,
    algorithm: Algorithm,
    send: DirectionKeys,
    receive: DirectionKeys,
    domains: Option<BTreeMap<u64, C>>,
}

pub struct Sealer<'a, C> {
    pub counter: SenderCounter,
    pub algorithm: Algorithm,
    pub key: Option<Key<'a>>,
    pub charge: C,
}

pub struct Opener<'a, C> {
    pub nonce: ReceiverNonce,
    pub algorithm: Algorithm,
    pub key: Option<Key<'a>>,
    pub charge: C,
}

pub fn traffic_profile(profile: u64, suite: u64) -> Option<(NonceFormat, AeadAlg)> {
    let format = match profile {
        0 => NonceFormat::Quic,
        1 => NonceFormat::Dtls13,
        2 => NonceFormat::Tls13,
        3 => NonceFormat::QuicMultipath,
        _ => return None,
    };
    Some((format, tls_suite(suite)?))
}

pub fn tls_suite(suite: u64) -> Option<AeadAlg> {
    Some(match suite {
        0 => AeadAlg::Aes128Gcm,
        1 => AeadAlg::Aes256Gcm,
        2 => AeadAlg::ChaCha20Poly1305,
        _ => return None,
    })
}

pub fn tls13_labels(format: NonceFormat) -> Option<Tls13Labels> {
    Some(match format {
        NonceFormat::Quic | NonceFormat::QuicMultipath => Tls13Labels::Quic,
        NonceFormat::Tls13 => Tls13Labels::Tls,
        NonceFormat::Dtls13 => Tls13Labels::Dtls,
        _ => return None,
    })
}

impl<C> Epoch<C> {
    pub fn import_traffic(
        format: NonceFormat,
        algorithm: AeadAlg,
        authority: ImportUse,
        secret: &[u8],
    ) -> Result<Self, ImportError> {
        Self::traffic_import(format, algorithm, authority, Some(secret))
    }

    pub fn opaque_traffic_import(
        format: NonceFormat,
        algorithm: AeadAlg,
        authority: ImportUse,
    ) -> Result<Self, ImportError> {
        Self::traffic_import(format, algorithm, authority, None)
    }

    fn traffic_import(
        format: NonceFormat,
        algorithm: AeadAlg,
        authority: ImportUse,
        secret: Option<&[u8]>,
    ) -> Result<Self, ImportError> {
        let send = match authority {
            ImportUse::TrafficSend => true,
            ImportUse::TrafficReceive => false,
            _ => return Err(ImportError::Denied),
        };
        let labels = tls13_labels(format).ok_or(ImportError::Invalid)?;
        let keys = secret
            .map(|secret| {
                Tls13PacketKeys::import(labels, algorithm, secret).ok_or(ImportError::Invalid)
            })
            .transpose()?;
        Ok(Self::directional(format, algorithm, send, keys))
    }

    pub(super) fn directional(
        format: NonceFormat,
        algorithm: AeadAlg,
        send: bool,
        keys: Option<Tls13PacketKeys>,
    ) -> Self {
        let live = keys.map_or(DirectionKeys::Opaque, DirectionKeys::Live);
        let (send, receive) = if send {
            (live, DirectionKeys::Missing)
        } else {
            (DirectionKeys::Missing, live)
        };
        Self::new(format, algorithm, send, receive)
    }

    pub fn quic_initial(side: Side, dcid: &[u8]) -> Option<Self> {
        let traffic = Tls13Traffic::quic_initial(dcid)?;
        Some(Self::traffic(
            NonceFormat::Quic,
            AeadAlg::Aes128Gcm,
            side,
            Some(&traffic),
        ))
    }

    pub fn traffic(
        format: NonceFormat,
        algorithm: AeadAlg,
        side: Side,
        traffic: Option<&Tls13Traffic>,
    ) -> Self {
        let keys = |sender| {
            traffic.map_or(DirectionKeys::Opaque, |traffic| {
                DirectionKeys::Live(traffic.packet_keys(sender, algorithm))
            })
        };
        Self::new(format, algorithm, keys(side), keys(side.peer()))
    }

    /// The evaluator applies refunds through its task scheduler.
    pub fn into_ledger(self) -> impl Iterator<Item = C> {
        self.domains.into_iter().flat_map(BTreeMap::into_values)
    }

    pub fn new(
        format: NonceFormat,
        algorithm: AeadAlg,
        send: DirectionKeys,
        receive: DirectionKeys,
    ) -> Self {
        Self::with_algorithm(format, Algorithm::Aead(algorithm), send, receive)
    }

    fn with_algorithm(
        format: NonceFormat,
        algorithm: Algorithm,
        send: DirectionKeys,
        receive: DirectionKeys,
    ) -> Self {
        Self {
            format,
            algorithm,
            send,
            receive,
            domains: Some(BTreeMap::new()),
        }
    }

    pub(super) fn srtp(
        format: NonceFormat,
        profile: SrtpProfile,
        send: DirectionKeys,
        receive: DirectionKeys,
    ) -> Self {
        let algorithm = profile.algorithm().map_or(Algorithm::Cm, Algorithm::Aead);
        Self::with_algorithm(format, algorithm, send, receive)
    }

    /// The caller admits the cipher and entry together, then partitions their charges.
    pub fn sealer(
        &mut self,
        domain: u64,
        floor: u64,
        admit: impl FnOnce(u64) -> Result<(C, C), u64>,
    ) -> Result<Sealer<'_, C>, IssueError> {
        self.send.validate()?;
        if !self.format.valid_domain(domain) {
            return Err(IssueError::Invalid);
        }
        let ledger = self.domains.as_mut().ok_or(IssueError::Spent)?;
        let entry = match ledger.entry(domain) {
            std::collections::btree_map::Entry::Occupied(_) => return Err(IssueError::Issued),
            std::collections::btree_map::Entry::Vacant(entry) => entry,
        };
        let requested = CIPHER_CHARGE + DOMAIN_CHARGE;
        let (charge, domain_charge) =
            admit(requested).map_err(|available| IssueError::Capacity {
                requested,
                available,
            })?;
        let counter =
            SenderCounter::with_base(self.format, self.send.base(self.algorithm), domain, floor)
                .unwrap();
        entry.insert(domain_charge);
        Ok(Sealer {
            counter,
            algorithm: self.algorithm,
            key: self.send.key(),
            charge,
        })
    }

    pub fn opener(
        &self,
        admit: impl FnOnce(u64) -> Result<C, u64>,
    ) -> Result<Opener<'_, C>, IssueError> {
        self.receive.validate()?;
        if self.domains.is_none() {
            return Err(IssueError::Spent);
        }
        let charge = admit(CIPHER_CHARGE).map_err(|available| IssueError::Capacity {
            requested: CIPHER_CHARGE,
            available,
        })?;
        let nonce = ReceiverNonce::with_base(self.format, self.receive.base(self.algorithm));
        Ok(Opener {
            nonce,
            algorithm: self.algorithm,
            key: self.receive.key(),
            charge,
        })
    }

    pub fn header_mask(
        &self,
        local: bool,
        admit: impl FnOnce(u64) -> Result<C, u64>,
    ) -> Result<(Option<HeaderMask>, C), IssueError> {
        if !matches!(
            self.format,
            NonceFormat::Quic | NonceFormat::QuicMultipath | NonceFormat::Dtls13
        ) {
            return Err(IssueError::Invalid);
        }
        let direction = if local { &self.send } else { &self.receive };
        direction.validate()?;
        if self.domains.is_none() {
            return Err(IssueError::Spent);
        }
        let charge = admit(MASK_CHARGE).map_err(|available| IssueError::Capacity {
            requested: MASK_CHARGE,
            available,
        })?;
        let mask = match (self.algorithm, direction) {
            (Algorithm::Aead(algorithm), DirectionKeys::Live(keys)) => {
                Some(HeaderMask::new(algorithm, keys.header().unwrap()).unwrap())
            }
            (_, DirectionKeys::Opaque) => None,
            _ => unreachable!("validated header mask profile and direction"),
        };
        Ok((mask, charge))
    }

    pub(super) fn key_update(
        &mut self,
        local: bool,
        keys: Option<Tls13PacketKeys>,
    ) -> impl Iterator<Item = C> {
        let direction = if local {
            &mut self.send
        } else {
            &mut self.receive
        };
        if let Some(mut next) = keys {
            if matches!(self.format, NonceFormat::Quic | NonceFormat::QuicMultipath)
                && let DirectionKeys::Live(before) = direction
            {
                next.keep_header_of(before);
            }
            *direction = DirectionKeys::Live(next);
        }
        let ledger = if local {
            Some(std::mem::take(self.domains.as_mut().unwrap()))
        } else {
            None
        };
        ledger.into_iter().flat_map(BTreeMap::into_values)
    }

    pub(super) fn retired(&self) -> bool {
        self.domains.is_none()
    }

    pub(super) fn retire_entries(&mut self) -> impl Iterator<Item = C> {
        self.send.erase();
        self.receive.erase();
        self.domains
            .take()
            .into_iter()
            .flat_map(BTreeMap::into_values)
    }

    pub fn retire(&mut self) {
        drop(self.retire_entries());
    }
}

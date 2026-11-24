//! Packet key ownership for one epoch. Issued resources keep their own keys.
use crate::{
    ImportUse,
    packet_cipher::HeaderMask,
    packet_nonce::{NonceFormat, ReceiverNonce, SenderCounter},
    secret_aead::AeadAlg,
    tls_kdf::{Side, Tls13Labels, Tls13PacketKeys, Tls13Traffic},
};
use std::collections::BTreeMap;

pub const SCHEDULE_CHARGE: u64 = 512;
pub const CIPHER_CHARGE: u64 = 1024;
pub const DOMAIN_CHARGE: u64 = 16;
pub const MASK_CHARGE: u64 = 512;

pub enum DirectionKeys {
    Missing,
    Opaque,
    Live(Tls13PacketKeys),
}

impl DirectionKeys {
    fn key(&self) -> Result<Option<&Tls13PacketKeys>, IssueError> {
        match self {
            Self::Missing => Err(IssueError::Invalid),
            Self::Opaque => Ok(None),
            Self::Live(keys) => Ok(Some(keys)),
        }
    }

    fn erase(&mut self) {
        if matches!(self, Self::Live(_)) {
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
    algorithm: AeadAlg,
    send: DirectionKeys,
    receive: DirectionKeys,
    domains: Option<BTreeMap<u64, C>>,
}

pub struct Sealer<'a, C> {
    pub counter: SenderCounter,
    pub algorithm: AeadAlg,
    pub key: Option<&'a [u8]>,
    pub charge: C,
}

pub struct Opener<'a, C> {
    pub nonce: ReceiverNonce,
    pub algorithm: AeadAlg,
    pub key: Option<&'a [u8]>,
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
    let algorithm = match suite {
        0 => AeadAlg::Aes128Gcm,
        1 => AeadAlg::Aes256Gcm,
        2 => AeadAlg::ChaCha20Poly1305,
        _ => return None,
    };
    Some((format, algorithm))
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
        let live = match secret {
            Some(secret) => DirectionKeys::Live(
                Tls13PacketKeys::import(labels, algorithm, secret).ok_or(ImportError::Invalid)?,
            ),
            None => DirectionKeys::Opaque,
        };
        let (send, receive) = if send {
            (live, DirectionKeys::Missing)
        } else {
            (DirectionKeys::Missing, live)
        };
        Ok(Self::new(format, algorithm, send, receive))
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
        let peer = match side {
            Side::Client => Side::Server,
            Side::Server => Side::Client,
        };
        let keys = |sender| {
            traffic.map_or(DirectionKeys::Opaque, |traffic| {
                DirectionKeys::Live(traffic.packet_keys(sender, algorithm))
            })
        };
        Self::new(format, algorithm, keys(side), keys(peer))
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
        Self {
            format,
            algorithm,
            send,
            receive,
            domains: Some(BTreeMap::new()),
        }
    }

    /// The caller admits the cipher and entry together, then partitions their charges.
    pub fn sealer(
        &mut self,
        domain: u64,
        floor: u64,
        admit: impl FnOnce(u64) -> Result<(C, C), u64>,
    ) -> Result<Sealer<'_, C>, IssueError> {
        let keys = self.send.key()?;
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
        let counter = SenderCounter::new(
            self.format,
            keys.map_or([0; 12], Tls13PacketKeys::iv),
            domain,
            floor,
        )
        .unwrap();
        entry.insert(domain_charge);
        Ok(Sealer {
            counter,
            algorithm: self.algorithm,
            key: keys.map(Tls13PacketKeys::payload),
            charge,
        })
    }

    pub fn opener(
        &self,
        admit: impl FnOnce(u64) -> Result<C, u64>,
    ) -> Result<Opener<'_, C>, IssueError> {
        let keys = self.receive.key()?;
        if self.domains.is_none() {
            return Err(IssueError::Spent);
        }
        let charge = admit(CIPHER_CHARGE).map_err(|available| IssueError::Capacity {
            requested: CIPHER_CHARGE,
            available,
        })?;
        let nonce = ReceiverNonce::new(self.format, keys.map_or([0; 12], Tls13PacketKeys::iv));
        Ok(Opener {
            nonce,
            algorithm: self.algorithm,
            key: keys.map(Tls13PacketKeys::payload),
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
        let keys = if local { &self.send } else { &self.receive }.key()?;
        if self.domains.is_none() {
            return Err(IssueError::Spent);
        }
        let charge = admit(MASK_CHARGE).map_err(|available| IssueError::Capacity {
            requested: MASK_CHARGE,
            available,
        })?;
        let mask =
            keys.map(|keys| HeaderMask::new(self.algorithm, keys.header().unwrap()).unwrap());
        Ok((mask, charge))
    }

    pub fn retire(&mut self) {
        self.send.erase();
        self.receive.erase();
        drop(self.domains.take());
    }
}

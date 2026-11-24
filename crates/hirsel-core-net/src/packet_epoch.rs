//! Packet key ownership for one epoch. Issued resources keep their own keys.
use crate::{
    packet_cipher::HeaderMask,
    packet_nonce::{NonceFormat, ReceiverNonce, SenderCounter},
    secret_aead::AeadAlg,
    tls_kdf::Tls13PacketKeys,
};
use std::collections::BTreeMap;

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

impl<C> Epoch<C> {
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

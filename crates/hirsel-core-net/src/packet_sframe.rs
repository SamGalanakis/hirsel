//! TLS-8 SFrame schedules: an RFC 9605 base key held in core, its ratchet
//! (§5.1), and per-KID keys (§4.4.2) issued as sealers and openers.
//!
//! The HKDF here is HMAC-SHA256 over arrays this module owns and zeroes, the
//! padded key blocks, inner digests and PRKs included, so no keyed HMAC state
//! outlives a call in a value Hirsel cannot erase (TLS-8, erasure).
use crate::{
    SframeDirection,
    packet_epoch::{CIPHER_CHARGE, DOMAIN_CHARGE, IssueError},
    packet_nonce::{NonceFormat, ReceiverNonce, SenderCounter},
    secret_kdf::sha256_wiped,
    tls_kdf::Tls13PacketKeys,
};
use std::{collections::BTreeMap, sync::Mutex};
use zeroize::Zeroizing;

/// The most ratchet steps one call takes (TLS-8, step counts).
pub const MAX_STEPS: u64 = 1 << 16;
/// A send import's entry in the run's send-key registry.
pub const REGISTRY_CHARGE: u64 = 48;
const MIN_BASE: usize = 16;
const MAX_BASE: usize = 64;
const BLOCK: usize = 64;
/// `AES_128_GCM_SHA256_128` (0x0004), RFC 9605 §4.5.
const SUITE: [u8; 2] = [0, 4];

/// HMAC-SHA256 (RFC 2104) of the concatenated `parts`, for a key of at most
/// one block, which every key here is.
fn hmac(key: &[u8], parts: &[&[u8]]) -> Zeroizing<[u8; 32]> {
    assert!(key.len() <= BLOCK);
    let mut inner_pad = Zeroizing::new([0x36; BLOCK]);
    let mut outer_pad = Zeroizing::new([0x5c; BLOCK]);
    for (i, byte) in key.iter().enumerate() {
        inner_pad[i] ^= byte;
        outer_pad[i] ^= byte;
    }
    let inner = sha256_wiped(|update| {
        update(&inner_pad[..]);
        for part in parts {
            update(part);
        }
    });
    sha256_wiped(|update| {
        update(&outer_pad[..]);
        update(&inner[..]);
    })
}

/// HKDF-Extract with the empty salt, which RFC 5869 reads as 32 zero bytes.
fn extract(ikm: &[u8]) -> Zeroizing<[u8; 32]> {
    hmac(&[0; 32], &[ikm])
}

/// HKDF-Expand to at most one hash length: `T(1)`, truncated by the caller.
fn expand(prk: &[u8; 32], info: &[&[u8]]) -> Zeroizing<[u8; 32]> {
    let mut parts = [&[][..]; 5];
    parts[..info.len()].copy_from_slice(info);
    parts[info.len()] = &[1];
    hmac(prk, &parts[..=info.len()])
}

/// A base key, in place in its schedule's slot.
struct BaseKey {
    bytes: Zeroizing<[u8; MAX_BASE]>,
    len: usize,
}

impl BaseKey {
    fn new(bytes: &[u8]) -> Option<Self> {
        if !(MIN_BASE..=MAX_BASE).contains(&bytes.len()) {
            return None;
        }
        let mut key = Self {
            bytes: Zeroizing::new([0; MAX_BASE]),
            len: bytes.len(),
        };
        key.bytes[..bytes.len()].copy_from_slice(bytes);
        Some(key)
    }

    fn get(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// RFC 9605 §5.1: `HKDF-Expand(HKDF-Extract("", base), "SFrame 1.0 Ratchet", 32)`,
    /// `steps` times, each output written over the base key it supersedes.
    fn ratchet(&mut self, steps: u64) {
        for _ in 0..steps {
            let prk = extract(self.get());
            let next = expand(&prk, &[b"SFrame 1.0 Ratchet"]);
            self.bytes[..32].copy_from_slice(&next[..]);
            self.bytes[32..].fill(0);
            self.len = 32;
        }
    }

    fn ratcheted(&self, steps: u64) -> Self {
        let mut copy = Self::new(self.get()).unwrap();
        copy.ratchet(steps);
        copy
    }

    /// RFC 9605 §4.4.2: `sframe_key` and `sframe_salt` of `kid`, with the KID as
    /// 8 bytes big-endian and the suite as 2.
    fn keys(&self, kid: u64) -> Tls13PacketKeys {
        let secret = extract(self.get());
        let kid = kid.to_be_bytes();
        let key = expand(&secret, &[b"SFrame 1.0 Secret key ", &kid, &SUITE]);
        let salt = expand(&secret, &[b"SFrame 1.0 Secret salt ", &kid, &SUITE]);
        let mut iv = Zeroizing::new([0; 12]);
        iv.copy_from_slice(&salt[..12]);
        Tls13PacketKeys::new(&key[..16], *iv)
    }
}

/// An SFrame schedule's state: its KID directions, its base key at the
/// current step (`None` when opaque), and the current step's sender ledger.
pub struct Sframe<C> {
    direction: SframeDirection,
    base: Option<BaseKey>,
    ledger: BTreeMap<u64, C>,
}

pub struct SframeSealer<C> {
    pub counter: SenderCounter,
    pub keys: Option<Tls13PacketKeys>,
    pub charge: C,
    /// The ledger of a step the call ratcheted past, for refund.
    pub retired: BTreeMap<u64, C>,
}

pub struct SframeOpener<C> {
    pub nonce: ReceiverNonce,
    pub keys: Option<Tls13PacketKeys>,
    pub charge: C,
}

impl<C> Sframe<C> {
    /// `None` for a base key of fewer than 16 or more than 64 bytes.
    pub(crate) fn import(direction: SframeDirection, bytes: Option<&[u8]>) -> Option<Self> {
        let base = match bytes {
            Some(bytes) => Some(BaseKey::new(bytes)?),
            None => None,
        };
        Some(Self {
            direction,
            base,
            ledger: BTreeMap::new(),
        })
    }

    /// Whether `bytes` is a base key length an import takes.
    pub fn valid_length(bytes: &[u8]) -> bool {
        (MIN_BASE..=MAX_BASE).contains(&bytes.len())
    }

    pub fn direction(&self) -> SframeDirection {
        self.direction
    }

    /// `sframe_sealer`'s refusals before admission, in order.
    pub fn check_sealer(&self, steps: u64, kid: u64) -> Result<(), IssueError> {
        if steps > MAX_STEPS || !self.direction.in_block(kid) {
            return Err(IssueError::Invalid);
        }
        if steps == 0 && self.ledger.contains_key(&kid) {
            return Err(IssueError::Issued);
        }
        Ok(())
    }

    /// The 1024 bytes of the sealer and the 16 of its ledger entry.
    pub const SEALER_CHARGE: u64 = CIPHER_CHARGE + DOMAIN_CHARGE;

    /// Ratchets `steps` times, then issues the sealer for `kid` at the step it
    /// reaches. The caller made `check_sealer` and admitted `SEALER_CHARGE`.
    pub fn sealer(
        &mut self,
        steps: u64,
        kid: u64,
        floor: u64,
        (charge, entry): (C, C),
    ) -> SframeSealer<C> {
        let retired = if steps == 0 {
            BTreeMap::new()
        } else {
            self.advance(steps)
        };
        self.ledger.insert(kid, entry);
        let keys = self.base.as_ref().map(|base| base.keys(kid));
        let salt = keys.as_ref().map_or([0; 12], Tls13PacketKeys::iv);
        SframeSealer {
            counter: SenderCounter::new(NonceFormat::Sframe, salt, 0, floor).unwrap(),
            keys,
            charge,
            retired,
        }
    }

    pub fn check_opener(&self, ahead: u64, kid: u64) -> Result<(), IssueError> {
        if ahead > MAX_STEPS || self.direction.in_block(kid) {
            return Err(IssueError::Invalid);
        }
        Ok(())
    }

    /// An opener for `kid` under the base key `ahead` steps on, ratcheted on a
    /// copy that is erased before return. The schedule does not move.
    pub fn opener(&self, ahead: u64, kid: u64, charge: C) -> SframeOpener<C> {
        let keys = self.base.as_ref().map(|base| {
            if ahead == 0 {
                base.keys(kid)
            } else {
                base.ratcheted(ahead).keys(kid)
            }
        });
        let salt = keys.as_ref().map_or([0; 12], Tls13PacketKeys::iv);
        SframeOpener {
            nonce: ReceiverNonce::new(NonceFormat::Sframe, salt),
            keys,
            charge,
        }
    }

    pub fn check_ratchet(steps: u64) -> Result<(), IssueError> {
        if steps == 0 || steps > MAX_STEPS {
            return Err(IssueError::Invalid);
        }
        Ok(())
    }

    /// Moves `steps` steps on and retires the ledger, whose entries the caller refunds.
    pub fn advance(&mut self, steps: u64) -> BTreeMap<u64, C> {
        if let Some(base) = &mut self.base {
            base.ratchet(steps);
        }
        std::mem::take(&mut self.ledger)
    }

    pub(super) fn domains(&self) -> impl Iterator<Item = u64> + '_ {
        self.ledger.keys().copied()
    }

    pub(crate) fn into_ledger(self) -> impl Iterator<Item = C> {
        self.ledger.into_values()
    }

    /// The base key slot, for erasure checks (TLS-8 PC11).
    pub fn base_slot(&self) -> Option<&[u8; MAX_BASE]> {
        self.base.as_ref().map(|base| &*base.bytes)
    }
}

/// The run's send-key registry (TLS-8, sender ownership): the fingerprint of
/// every `send` import's base key, each entry's charge held until the run ends.
pub struct SendKeys<C>(Mutex<Vec<(Option<[u8; 32]>, C)>>);

impl<C> Default for SendKeys<C> {
    fn default() -> Self {
        Self(Mutex::new(Vec::new()))
    }
}

impl<C> SendKeys<C> {
    /// Adds the fingerprint of `base`, keeping `charge` for the run, or gives
    /// the charge back when the registry holds it already. The look-up and the
    /// addition are one transition.
    pub fn register(&self, base: &[u8], charge: C) -> Result<(), C> {
        let print = sha256_wiped(|update| {
            update(b"hirsel sframe 1\0");
            update(base);
        });
        let mut entries = self.0.lock().unwrap();
        if entries
            .iter()
            .any(|(held, _)| held.as_ref() == Some(&*print))
        {
            return Err(charge);
        }
        entries.push((Some(*print), charge));
        Ok(())
    }

    /// A replayed import's entry: its charge, held as live, without a fingerprint.
    pub fn replayed(&self, charge: C) {
        self.0.lock().unwrap().push((None, charge));
    }

    /// Ends the run: every entry's charge, for refund.
    pub fn end(self) -> impl Iterator<Item = C> {
        self.0
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|(_, charge)| charge)
    }
}

#[cfg(test)]
mod tests;

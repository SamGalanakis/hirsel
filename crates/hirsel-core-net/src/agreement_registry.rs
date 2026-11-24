//! TLS-8's run-wide agreement uniqueness check.
use crate::tls_kdf::Side;
use ring::digest;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub struct AgreementRegistry<Charge> {
    state: Mutex<State<Charge>>,
}

struct State<Charge> {
    next: u64,
    outstanding: BTreeSet<u64>,
    entries: Vec<Box<Entry<Charge>>>,
}

/// The charge lives until every share eligible for this record's verdict has
/// delivered a verdict of its own or been dropped.
struct Entry<Charge> {
    fingerprint: Option<Zeroizing<[u8; 32]>>,
    side: Side,
    count: u64,
    _charge: Charge,
}

/// Held by a share and moved into its constructor at issue. The charge of the
/// share itself is separate and can be released at issue without releasing
/// the ticket's registry protection.
pub struct ShareTicket<Charge> {
    registry: Arc<AgreementRegistry<Charge>>,
    ordinal: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryVerdict {
    Accepted,
    Issued,
}

impl<Charge> Default for AgreementRegistry<Charge> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                next: 0,
                outstanding: BTreeSet::new(),
                entries: Vec::new(),
            }),
        }
    }
}

impl<Charge> AgreementRegistry<Charge> {
    /// A share becomes visible only after key generation succeeds.
    pub fn new_share(self: &Arc<Self>) -> Option<ShareTicket<Charge>> {
        let ordinal = {
            let mut state = self.state.lock().unwrap();
            let ordinal = state.next;
            state.next = state.next.checked_add(1)?;
            state.outstanding.insert(ordinal);
            ordinal
        };
        Some(ShareTicket {
            registry: self.clone(),
            ordinal,
        })
    }
}

impl<Charge> ShareTicket<Charge> {
    pub fn resolve(self, side: Side, shared: &[u8; 32], charge: Charge) -> RegistryVerdict {
        let mut hash = digest::Context::new(&digest::SHA256);
        hash.update(b"hirsel agreement 1\0");
        hash.update(shared);
        let fingerprint: Zeroizing<[u8; 32]> =
            Zeroizing::new(hash.finish().as_ref().try_into().unwrap());
        let mut state = self.registry.state.lock().unwrap();
        if state.entries.iter().any(|entry| {
            entry.side == side
                && entry
                    .fingerprint
                    .as_deref()
                    .is_some_and(|key| bool::from(key.ct_eq(&*fingerprint)))
        }) {
            return RegistryVerdict::Issued;
        }
        let count = state.next;
        let mut entry = Box::new(Entry {
            fingerprint: None,
            side,
            count,
            _charge: charge,
        });
        entry.fingerprint = Some(fingerprint);
        state.entries.push(entry);
        RegistryVerdict::Accepted
    }

    /// Replay has the recorded verdict. A replayed admission still lives
    /// until the last eligible share's verdict, even when the key is opaque.
    pub fn replay(self, side: Side, verdict: RegistryVerdict, charge: Charge) -> RegistryVerdict {
        if verdict == RegistryVerdict::Accepted {
            let mut state = self.registry.state.lock().unwrap();
            let count = state.next;
            state.entries.push(Box::new(Entry {
                fingerprint: None,
                side,
                count,
                _charge: charge,
            }));
        }
        verdict
    }
}

impl<Charge> Drop for ShareTicket<Charge> {
    fn drop(&mut self) {
        let retired = {
            let mut state = self.registry.state.lock().unwrap();
            state.outstanding.remove(&self.ordinal);
            let oldest = state.outstanding.first().copied();
            state
                .entries
                .extract_if(.., |entry| oldest.is_none_or(|id| id >= entry.count))
                .collect::<Vec<_>>()
        };
        // A refund can notify another unit. Only the registry owns its lock.
        drop(retired);
    }
}

#[cfg(test)]
mod tests;

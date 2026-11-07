//! Operation-local query identities. Portable keys keep their separate domains.
use hirsel_syntax::ContentHash;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A canonical definition rank within one immutable operation capture.
/// Neither compiler identity nor the process's interned definition ID participates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct LocalSlot {
    pub rank: u32,
    pub generation: u64,
    pub body: bool,
}
impl LocalSlot {
    pub fn generation() -> Option<u64> {
        NEXT_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .ok()
    }
    /// The generic runtime address is an exact envelope, with no hashing.
    pub fn address(self) -> ContentHash {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&self.rank.to_be_bytes());
        bytes[4..12].copy_from_slice(&self.generation.to_be_bytes());
        bytes[12] = u8::from(self.body);
        ContentHash(bytes)
    }
    pub fn decode(address: ContentHash) -> Option<Self> {
        if address.0[12] > 1 || address.0[13..].iter().any(|&byte| byte != 0) {
            return None;
        }
        Some(Self {
            rank: u32::from_be_bytes(address.0[..4].try_into().ok()?),
            generation: u64::from_be_bytes(address.0[4..12].try_into().ok()?),
            body: address.0[12] != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_are_exact_and_reject_foreign_envelopes() {
        for generation in [13, 79] {
            let first = LocalSlot {
                rank: 3,
                generation,
                body: false,
            };
            let next = LocalSlot { rank: 4, ..first };
            let body = LocalSlot {
                body: true,
                ..first
            };
            let foreign = LocalSlot {
                generation: generation + 1,
                ..first
            };
            for slot in [first, next, body, foreign] {
                assert_eq!(LocalSlot::decode(slot.address()), Some(slot));
            }
            assert!(first.address() < next.address());
            assert_ne!(first.address(), body.address());
            assert_ne!(first.address(), foreign.address());
            let mut collision = first.address();
            collision.0[31] = 1;
            assert_eq!(LocalSlot::decode(collision), None);
            let mut bad_kind = first.address();
            bad_kind.0[12] = 2;
            assert_eq!(LocalSlot::decode(bad_kind), None);
        }
    }
}

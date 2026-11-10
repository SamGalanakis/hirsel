//! The little of the history format (storage.md STORE-11 "History format") an
//! authority needs to append a claim entry: a group's header, the first commit
//! when it is a claim or a fork, and the claim group's bytes. Everything else
//! in an entry is the engine's to check.

use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"HSHIST01";
/// The magic's family: other version digits are another format version.
const FAMILY: &[u8; 6] = b"HSHIST";
const HEADER: usize = 60;
const KIND_ENGINE: u8 = 2;
const OP_CLAIM: u8 = 2;
const OP_FORK: u8 = 5;

/// What a claim needs from the history's last entry and its newest claim or fork.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tip {
    pub digest: [u8; 32],
    /// The version of the last commit, 0 for an empty history.
    pub last: u64,
    pub epoch: u64,
    /// The store identity, empty before the first claim.
    pub identity: Option<[u8; 32]>,
}

impl Tip {
    pub fn origin() -> Self {
        Tip {
            digest: [0; 32],
            last: 0,
            epoch: 0,
            identity: None,
        }
    }
}

/// Why an entry could not be followed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Bad {
    Corrupt,
    /// The record is of the format family with other version digits.
    Unsupported([u8; 8]),
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

pub fn digest(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The tip after `entry`, which must follow `tip` by its header: `prev`, `first`
/// and `epoch`, and a claim's identity. Checks only what the authority relies
/// on; the engine checks the whole group.
pub fn follow(tip: &Tip, entry: &[u8]) -> Result<Tip, Bad> {
    if entry.len() < 8 {
        return Err(Bad::Corrupt);
    }
    if &entry[..8] != MAGIC {
        if &entry[..6] == FAMILY {
            return Err(Bad::Unsupported(entry[..8].try_into().unwrap()));
        }
        return Err(Bad::Corrupt);
    }
    if entry.len() < HEADER + 2 {
        return Err(Bad::Corrupt);
    }
    let prev: [u8; 32] = entry[8..40].try_into().unwrap();
    let first = u64_at(entry, 40);
    let epoch = u64_at(entry, 48);
    let count = u32_at(entry, 56) as u64;
    if prev != tip.digest || first != tip.last + 1 || count == 0 {
        return Err(Bad::Corrupt);
    }
    let last = first.checked_add(count - 1).ok_or(Bad::Corrupt)?;
    let mut identity = tip.identity;
    let first_commit = engine_payload(entry);
    match first_commit {
        Some((OP_CLAIM, payload)) if payload.len() == 56 => {
            let claimed = u64_at(payload, 0);
            let id: [u8; 32] = payload[24..56].try_into().unwrap();
            if epoch != tip.epoch + 1 || claimed != epoch {
                return Err(Bad::Corrupt);
            }
            match identity {
                None => identity = Some(id),
                Some(known) if known != id => return Err(Bad::Corrupt),
                Some(_) => {}
            }
        }
        Some((OP_CLAIM, _)) => return Err(Bad::Corrupt),
        Some((OP_FORK, payload)) if payload.len() == 72 => {
            if epoch != tip.epoch || tip.epoch == 0 {
                return Err(Bad::Corrupt);
            }
            identity = Some(payload[..32].try_into().unwrap());
        }
        _ => {
            if epoch != tip.epoch || tip.epoch == 0 {
                return Err(Bad::Corrupt);
            }
        }
    }
    Ok(Tip {
        digest: digest(entry),
        last,
        epoch,
        identity,
    })
}

/// The op and payload of the group's first commit when it is an engine commit.
fn engine_payload(entry: &[u8]) -> Option<(u8, &[u8])> {
    if entry[HEADER] != KIND_ENGINE || entry.len() < HEADER + 6 {
        return None;
    }
    let op = entry[HEADER + 1];
    let n = u32_at(entry, HEADER + 2) as usize;
    let start = HEADER + 6;
    let payload = entry.get(start..start.checked_add(n)?)?;
    Some((op, payload))
}

/// The claim group after `tip`: one Claim commit one epoch up, with the
/// claimant's attempt identity and the history's identity (`fresh` when the
/// history has none yet).
pub fn claim_group(tip: &Tip, attempt: [u8; 16], fresh: [u8; 32]) -> Vec<u8> {
    let epoch = tip.epoch + 1;
    let identity = tip.identity.unwrap_or(fresh);
    let mut out = Vec::with_capacity(130);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&tip.digest);
    out.extend_from_slice(&(tip.last + 1).to_le_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.push(KIND_ENGINE);
    out.push(OP_CLAIM);
    out.extend_from_slice(&56u32.to_le_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&attempt);
    out.extend_from_slice(&identity);
    out.extend_from_slice(&0u32.to_le_bytes()); // writes
    out.extend_from_slice(&0u32.to_le_bytes()); // definitions
    out
}

/// Whether `entry` is a claim group naming `attempt`: how a provider that lost
/// a response recognises its own claim.
pub fn claims(entry: &[u8], attempt: &[u8; 16]) -> bool {
    entry.len() > HEADER + 2
        && &entry[..8] == MAGIC
        && matches!(engine_payload(entry), Some((OP_CLAIM, p)) if p.len() == 56 && &p[8..24] == attempt)
}

/// Whether `entry` is any claim group.
pub fn is_claim(entry: &[u8]) -> bool {
    entry.len() > HEADER + 2
        && &entry[..8] == MAGIC
        && matches!(engine_payload(entry), Some((OP_CLAIM, _)))
}

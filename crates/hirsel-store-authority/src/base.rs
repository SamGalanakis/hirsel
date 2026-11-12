//! A store's base (storage.md STORE-11, Bases): what a reader needs of the
//! history before a position `first` to recover from `first` - 1 on. The
//! store's engine chooses `first` and writes `boot`, its own state at
//! `first` - 1, which no provider reads; the provider adds the chain's tip
//! there, which a chain resumed at the base follows from, and the objects the
//! records below `first` leave live: created by one and retired by none.
//!
//! ```text
//! base = "HSBASE01" first:u64 digest:[32] epoch:u64 identity:[32]
//!        appended:locator count:u32 ref* len:u32 boot
//! ref  = lifetime:u64 epoch:u64 digest:[32]
//! ```
//!
//! The tip is the one after the record at `first` - 1 (its end is implied);
//! an `appended` of position 0 is none. The refs are in ascending order. A
//! claim holder selects a base with [`crate::Authority::select`]; the
//! selected base's `first` never goes down.

use crate::envelope::Tip;
use crate::reclaim::Space;
use crate::{Error, Locator, ObjectRef, Record, invalid};
use std::collections::BTreeSet;

pub const MAGIC: &[u8; 8] = b"HSBASE01";
/// The magic's family: other version digits are another format version.
const FAMILY: &[u8; 6] = b"HSBASE";
/// Records one read asks for.
const READ_BATCH: u64 = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Base {
    pub first: u64,
    /// The chain's tip after the record at `first` - 1.
    pub tip: Tip,
    /// The objects the records below `first` leave live, in ascending order.
    pub live: Vec<ObjectRef>,
    /// The engine's bootstrap.
    pub boot: Vec<u8>,
}

impl Base {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 8 + 32 + 8 + 32 + 40 + 8 + 48 * self.live.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.first.to_le_bytes());
        out.extend_from_slice(&self.tip.digest);
        out.extend_from_slice(&self.tip.epoch.to_le_bytes());
        out.extend_from_slice(&self.tip.identity.unwrap_or_default());
        out.extend_from_slice(&self.tip.appended.unwrap_or_default().encode());
        out.extend_from_slice(&(self.live.len() as u32).to_le_bytes());
        for r in &self.live {
            out.extend_from_slice(&r.lifetime.to_le_bytes());
            out.extend_from_slice(&r.epoch.to_le_bytes());
            out.extend_from_slice(&r.digest);
        }
        out.extend_from_slice(&(self.boot.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.boot);
        out
    }

    /// The base `b` holds, every field checked; `object` names it in an error.
    pub fn decode(b: &[u8], object: &str) -> Result<Base, Error> {
        let corrupt = || Error::Corrupt(object.to_string());
        if b.len() >= 8 && &b[..8] != MAGIC && &b[..6] == FAMILY {
            return Err(Error::Format {
                found: b[..8].try_into().unwrap(),
                object: object.to_string(),
            });
        }
        if b.len() < 8 || &b[..8] != MAGIC {
            return Err(corrupt());
        }
        let mut at = 8;
        let mut take = |n: usize| -> Result<&[u8], Error> {
            let s = b.get(at..at + n).ok_or_else(corrupt)?;
            at += n;
            Ok(s)
        };
        let u64_of = |s: &[u8]| u64::from_le_bytes(s.try_into().unwrap());
        let first = u64_of(take(8)?);
        let digest: [u8; 32] = take(32)?.try_into().unwrap();
        let epoch = u64_of(take(8)?);
        let identity: [u8; 32] = take(32)?.try_into().unwrap();
        let appended = Locator::decode(take(Locator::BYTES)?).unwrap();
        let count = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let mut live = Vec::with_capacity(count.min(b.len() / 48));
        for _ in 0..count {
            let r = take(48)?;
            live.push(ObjectRef {
                lifetime: u64_of(&r[..8]),
                epoch: u64_of(&r[8..16]),
                digest: r[16..].try_into().unwrap(),
            });
        }
        let len = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let boot = take(len)?.to_vec();
        let appended = match appended.position {
            0 if appended == Locator::default() => None,
            p if p > 0 && p < first => Some(appended),
            _ => return Err(corrupt()),
        };
        // The first record is a claim, so a base names an epoch and an identity.
        if at != b.len()
            || first < 2
            || epoch == 0
            || !live.windows(2).all(|w| w[0] < w[1])
            || live
                .iter()
                .any(|r| r.lifetime == 0 || r.lifetime >= first || r.epoch == 0 || r.epoch > epoch)
        {
            return Err(corrupt());
        }
        Ok(Base {
            first,
            tip: Tip {
                digest,
                end: first - 1,
                epoch,
                identity: Some(identity),
                appended,
            },
            live,
            boot,
        })
    }
}

/// Whether a selection of `first` makes a new base over `prior`, at a
/// claim whose strongest-durable end is `s`: refused below 2, past `s` + 1
/// or below the selected base's; `false` at the selected base's own.
pub(crate) fn admit(prior: Option<&Base>, first: u64, s: u64) -> Result<bool, Error> {
    if first < 2 || first - 1 > s {
        return Err(invalid());
    }
    match prior {
        Some(p) if first < p.first => Err(invalid()),
        Some(p) => Ok(first > p.first),
        None => Ok(true),
    }
}

/// The base at `first` carrying `boot`, folding the strongest-durable
/// records from `prior`'s (or the history's start) to `first` - 1.
pub(crate) fn build(
    sp: &mut impl Space,
    prior: Option<Base>,
    first: u64,
    boot: Vec<u8>,
) -> Result<Base, Error> {
    let (mut tip, live) = match prior {
        Some(p) => (p.tip, p.live),
        None => (Tip::origin(), Vec::new()),
    };
    let mut live: BTreeSet<ObjectRef> = live.into_iter().collect();
    // The newest append this fold reaches; before it, the prior tip's.
    let mut newest = None;
    let stop = first - 1;
    while tip.end < stop {
        let records = sp.records(tip.end, (stop - tip.end).min(READ_BATCH))?;
        if records.is_empty() {
            return Err(Error::Corrupt(format!("history entry {}", tip.end + 1)));
        }
        for record in records {
            tip.end += 1;
            match record {
                Record::Claimed {
                    epoch, identity, ..
                } => {
                    tip.epoch = epoch;
                    tip.identity = Some(identity);
                }
                Record::Appended {
                    created, retired, ..
                } => {
                    live.extend(created);
                    for r in &retired {
                        live.remove(r);
                    }
                    newest = Some(tip.end);
                }
            }
        }
    }
    tip.digest = sp.locator(stop)?.digest;
    if let Some(p) = newest {
        tip.appended = Some(sp.locator(p)?);
    }
    Ok(Base {
        first,
        tip,
        live: live.into_iter().collect(),
        boot,
    })
}

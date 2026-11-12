//! Reclamation (storage.md STORE-11, Reclamation): one algorithm over each
//! provider's object space, deciding from the history alone.
//!
//! `S` is the claim's strongest-durable end. An object `r` is removed when
//! - R1: a record at or below `S` retires it;
//! - R2: `r.lifetime <= S` and the record there does not inventory it (a claim
//!   record, another epoch, another digest);
//! - R3: `r.lifetime > S` and `r.epoch` is below the claim's: the claim's own
//!   record is at or below `S`, so no record of `r`'s epoch is at or past it.
//!
//! A call first deletes the retirements of the records after the continuation's
//! `scanned` up to `S`, then, once per epoch, sweeps the provider's listing from
//! its cursor and removes what any rule removes: an object of lifetime at most
//! `S` stays only while it is [`Live`] at `S`, so a late put that re-creates an
//! object after R1 deleted it is removed again (R1), as is one no record
//! inventories (R2). Where it stopped is the [`Sweep`] the
//! provider publishes, fenced, so a takeover continues it and a stale publisher
//! never moves it back. Once a claim's sweep has reached the listing's end, its
//! later calls only delete retirements: garbage made after the sweep waits for
//! the next claim's.

use crate::{AuthorityError, Claim, Error, ObjectRef, Record};
use std::collections::HashSet;
use std::io::ErrorKind;

/// Where a store's reclamation stands. A claim loads it and a takeover
/// inherits it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Sweep {
    /// Every retirement of the records up to this position is deleted.
    pub scanned: u64,
    /// How many retirements of the record after `scanned` are deleted.
    pub index: u32,
    /// The last listing key the sweep for R2 and R3 garbage has decided, empty
    /// at the sweep's start.
    pub cursor: String,
    /// The epoch of the claim whose sweep the cursor is in; a claim of another
    /// epoch starts its own sweep from the listing's start.
    pub sweeping: u64,
    /// The epoch whose sweep reached the listing's end, 0 before any did.
    pub swept: u64,
}

/// The objects the records up to `upto` leave live: created by one and
/// retired by none. A claim builds it from the records its reclaim calls read
/// and keeps it in memory, so each record is read once per claim, and a
/// takeover builds its own.
#[derive(Debug, Default)]
pub(crate) struct Live {
    upto: u64,
    refs: HashSet<ObjectRef>,
}

/// What one [`crate::Authority::reclaim`] call left.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reclaimed {
    /// The budget ran out first: call again.
    More,
    /// Every retirement up to `S` is deleted, and the claim's sweep has
    /// reached the listing's end.
    Done,
}

/// What a listed key is.
pub(crate) enum Found {
    /// A derived object, decided by R2 or R3.
    Object(ObjectRef),
    /// The provider's own garbage, decided by the provider from the history.
    Garbage,
    /// Never removed by reclamation.
    Keep,
}

pub(crate) struct Page {
    pub keys: Vec<(String, Found)>,
    /// Keys past the page's last.
    pub more: bool,
}

/// A provider's object space, as reclamation uses it. Each method is one
/// provider request.
pub(crate) trait Space {
    /// The keys after `after` in key order. `s` is the claim's
    /// strongest-durable end, which bounds the provider's own garbage.
    fn page(&mut self, after: &str, s: u64) -> Result<Page, Error>;
    /// Remove `key`; an absent key is success.
    fn remove(&mut self, key: &str) -> Result<(), Error>;
    /// The key of the object `r`.
    fn key(&self, r: &ObjectRef) -> String;
    /// Up to `max` records after `from`, strongest-durable.
    fn records(&mut self, from: u64, max: u64) -> Result<Vec<Record>, Error>;
    /// The locator of a record [`Space::records`] has answered.
    fn locator(&mut self, position: u64) -> Result<crate::Locator, Error>;
    /// Make `sweep` the store's continuation if `c` still holds the store and
    /// the continuation is the one `c` loaded or last published; moves
    /// `c.revision` one up.
    fn publish(&mut self, c: &mut Claim, sweep: &Sweep) -> Result<(), Error>;
}

/// Records one read asks for.
const READ_BATCH: u64 = 1024;
/// Requests for one delete before reclamation fails.
const DELETE_TRIES: u64 = 3;

/// One reclaim call of at most `budget` requests at strongest-durable end `s`.
pub(crate) fn run(
    sp: &mut impl Space,
    c: &mut Claim,
    s: u64,
    budget: u64,
) -> Result<Reclaimed, Error> {
    // One request decides something; one more publishes it.
    if budget < 2 {
        return Err(Error::Refused(AuthorityError::io(ErrorKind::InvalidInput)));
    }
    let mut w = c.sweep.clone();
    let mut left = budget - 1;
    let epoch = c.epoch;
    let outcome = step(sp, epoch, s, &mut w, &mut c.live, &mut left)?;
    if w != c.sweep {
        sp.publish(c, &w)?;
        c.sweep = w;
    }
    Ok(outcome)
}

fn step(
    sp: &mut impl Space,
    epoch: u64,
    s: u64,
    w: &mut Sweep,
    live: &mut Live,
    left: &mut u64,
) -> Result<Reclaimed, Error> {
    // R1: the retirements of every record up to S.
    while w.scanned < s {
        if !spend(left) {
            return Ok(Reclaimed::More);
        }
        let records = sp.records(w.scanned, (s - w.scanned).min(READ_BATCH))?;
        if records.is_empty() {
            return Err(Error::Corrupt(format!("history entry {}", w.scanned + 1)));
        }
        for record in records {
            if let Record::Appended { retired, .. } = &record {
                while let Some(r) = retired.get(w.index as usize) {
                    let key = sp.key(r);
                    if !remove(sp, &key, left)? {
                        return Ok(Reclaimed::More);
                    }
                    w.index += 1;
                }
            }
            w.scanned += 1;
            w.index = 0;
        }
    }
    // R1, R2 and R3 over the listing, once per epoch.
    if w.swept == epoch {
        return Ok(Reclaimed::Done);
    }
    if w.sweeping != epoch {
        w.sweeping = epoch;
        w.cursor.clear();
    }
    // Live(S), from the records this claim has not read for it yet.
    if live.upto > s {
        *live = Live::default();
    }
    while live.upto < s {
        if !spend(left) {
            return Ok(Reclaimed::More);
        }
        let records = sp.records(live.upto, (s - live.upto).min(READ_BATCH))?;
        if records.is_empty() {
            return Err(Error::Corrupt(format!("history entry {}", live.upto + 1)));
        }
        for record in records {
            if let Record::Appended {
                created, retired, ..
            } = &record
            {
                live.refs.extend(created.iter().copied());
                for r in retired {
                    live.refs.remove(r);
                }
            }
            live.upto += 1;
        }
    }
    loop {
        if !spend(left) {
            return Ok(Reclaimed::More);
        }
        let page = sp.page(&w.cursor, s)?;
        for (key, found) in page.keys {
            let gone = match found {
                Found::Keep => false,
                Found::Garbage => true,
                Found::Object(r) if r.lifetime == 0 => false,
                Found::Object(r) if r.lifetime > s => r.epoch < epoch,
                Found::Object(r) => !live.refs.contains(&r),
            };
            if gone && !remove(sp, &key, left)? {
                return Ok(Reclaimed::More);
            }
            w.cursor = key;
        }
        if !page.more {
            w.cursor.clear();
            w.swept = epoch;
            return Ok(Reclaimed::Done);
        }
    }
}

fn spend(left: &mut u64) -> bool {
    let ok = *left > 0;
    *left = left.saturating_sub(1);
    ok
}

/// Deletes `key`, retrying a failed request: `false` when the budget ran out first.
fn remove(sp: &mut impl Space, key: &str, left: &mut u64) -> Result<bool, Error> {
    let mut tries = 0;
    loop {
        if !spend(left) {
            return Ok(false);
        }
        tries += 1;
        match sp.remove(key) {
            Ok(()) => return Ok(true),
            Err(e) if tries == DELETE_TRIES => return Err(e),
            Err(_) => {}
        }
    }
}

/// The continuation's bytes after a format's magic and revision:
/// `scanned:u64 index:u32 sweeping:u64 swept:u64 len:u32 cursor`.
pub(crate) fn encode(revision: u64, sweep: &Sweep, out: &mut Vec<u8>) {
    out.extend_from_slice(&revision.to_le_bytes());
    out.extend_from_slice(&sweep.scanned.to_le_bytes());
    out.extend_from_slice(&sweep.index.to_le_bytes());
    out.extend_from_slice(&sweep.sweeping.to_le_bytes());
    out.extend_from_slice(&sweep.swept.to_le_bytes());
    out.extend_from_slice(&(sweep.cursor.len() as u32).to_le_bytes());
    out.extend_from_slice(sweep.cursor.as_bytes());
}

/// The revision and continuation [`encode`] wrote at the start of `b`, and the
/// bytes after it; `None` when they do not parse.
pub(crate) fn decode(b: &[u8]) -> Option<(u64, Sweep, &[u8])> {
    let u64_at = |i: usize| Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().unwrap()));
    let u32_at = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().unwrap()));
    let revision = u64_at(0)?;
    let scanned = u64_at(8)?;
    let index = u32_at(16)?;
    let sweeping = u64_at(20)?;
    let swept = u64_at(28)?;
    let len = u32_at(36)? as usize;
    let cursor = std::str::from_utf8(b.get(40..40 + len)?).ok()?.to_string();
    Some((
        revision,
        Sweep {
            scanned,
            index,
            cursor,
            sweeping,
            swept,
        },
        &b[40 + len..],
    ))
}

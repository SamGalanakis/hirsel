//! `authority.objects@1`: the history in an object-storage entry.
//!
//! One level, the one the grant names: whatever the storage survives, every
//! acknowledged write survives with it. The history is a chain of immutable
//! group objects `h/<position as 20 digits>-<digest>` (the SHA-256 of the
//! entry, lowercase hexadecimal), each entry naming the digest of the one
//! before it, and one mutable object, `head`:
//!
//! ```text
//! head = "HSOBJH02" attempt:[16] end:u64 digest:[32] revision:u64
//!        scanned:u64 index:u32 len:u32 cursor sum:[32]
//! ```
//!
//! naming the newest claim's attempt, the history's end and the last entry's
//! digest, then reclamation's continuation; `sum` is the SHA-256 of everything
//! before it. The head changes only by a conditional replace on the entity tag
//! the writer's own previous replace returned, and every replace, a claim's,
//! an append's or a reclamation's, writes the revision one up, so no two heads
//! have equal bytes and no tag recurs.
//!
//! An append of `k` entries is `k + 1` requests: create each record object if
//! absent, then replace the head once, naming the newest. That record's digest
//! covers every record before it, so the batch lands whole or not at all. A claim reads the head, follows the history to its end, creates its
//! claim entry and replaces the head (or creates it when absent); of racing
//! claims one replace succeeds.
//!
//! Recovery reads the head and follows `prev` from the digest it names, getting
//! each group by its name, so it never depends on a listing. When it must
//! follow more than one group it first lists `h/` past what it knows and gets
//! every listed candidate at once: a listing is only a hint that lets the gets
//! run in parallel, and a group it misses is got by name. Requests per recovery
//! of `n` groups: one head get, `ceil(n / 1000)` listing pages and `n` group gets
//! (plus one per orphan the listing shows), the gets concurrent; with no
//! listing, one head get and `n` sequential gets.
//!
//! Reclamation lists every key. Derived objects are `o/<name>`. Besides them it
//! removes a group object at a position the head has reached that the chain
//! does not name: left by a writer fenced or undecided before its head replace.
//! It never removes one past the head's end, which may be the current writer's
//! append between its puts and its replace.

use crate::envelope::{self, Chain};
use crate::reclaim::{self, Found, Page, Space, Sweep};
use crate::{
    Advanced, Authority, AuthorityError, Claim, Claimed, Error, Level, ObjectRef, Queued,
    Reclaimed, Record, live, owner_id,
};
use hirsel_object_storage::{Condition, ObjectStorage, Put, Replaced, StorageError, Tag};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

const HEAD: &str = "head";
const MAGIC: &[u8; 8] = b"HSOBJH02";
/// The magic's family: other version digits are another format version.
const FAMILY: &[u8; 6] = b"HSOBJH";
/// The head's bytes before its continuation's cursor.
const HEAD_FIXED: usize = 8 + 16 + 8 + 32 + 8 + 8 + 4 + 4;
/// Attempts of a claim whose head replace lost to the current writer's append.
const CLAIM_TRIES: usize = 8;

/// Deliberate defects the harness must catch (its negative controls; the first
/// two are the place-authority-674 model's).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Bug {
    /// `no_cas`: the head replace is conditional on the tag of a fresh read of
    /// the head rather than on the writer's own last replace, so it overwrites
    /// whatever another claim wrote.
    NoCas,
    /// `any_epoch`: reclamation removes every group object the chain does not
    /// name, past the head's end too.
    AnyEpoch,
    /// Recovery takes a listing as complete: a group it does not show is
    /// missing.
    TrustListing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Head {
    attempt: [u8; 16],
    end: u64,
    digest: [u8; 32],
    revision: u64,
    sweep: Sweep,
}

impl Head {
    /// The head `c` writes next: at `end` and `digest`, with continuation
    /// `sweep`, one revision past the claim's last.
    fn next(c: &Claim, end: u64, digest: [u8; 32], sweep: &Sweep) -> Head {
        Head {
            attempt: c.attempt,
            end,
            digest,
            revision: c.revision + 1,
            sweep: sweep.clone(),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEAD_FIXED + self.sweep.cursor.len() + 32);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.attempt);
        out.extend_from_slice(&self.end.to_le_bytes());
        out.extend_from_slice(&self.digest);
        reclaim::encode(self.revision, &self.sweep, &mut out);
        let sum = Sha256::digest(&out);
        out.extend_from_slice(&sum);
        out
    }

    fn decode(b: &[u8]) -> Result<Head, Error> {
        let corrupt = || Error::Corrupt(HEAD.to_string());
        if b.len() >= 8 && &b[..8] != MAGIC && &b[..6] == FAMILY {
            return Err(Error::Format {
                found: b[..8].try_into().unwrap(),
                object: HEAD.to_string(),
            });
        }
        if b.len() < HEAD_FIXED + 32 || &b[..8] != MAGIC {
            return Err(corrupt());
        }
        let (body, sum) = b.split_at(b.len() - 32);
        if Sha256::digest(body)[..] != *sum {
            return Err(corrupt());
        }
        let Some((revision, sweep, [])) = reclaim::decode(&body[64..]) else {
            return Err(corrupt());
        };
        Ok(Head {
            attempt: b[8..24].try_into().unwrap(),
            end: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            digest: b[32..64].try_into().unwrap(),
            revision,
            sweep,
        })
    }
}

fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The group object holding the entry at `position` with `digest`.
pub fn group_key(position: u64, digest: &[u8; 32]) -> String {
    format!("h/{position:020}-{}", hex(digest))
}

/// The position and digest a group object's key names.
pub fn parse_group_key(key: &str) -> Option<(u64, [u8; 32])> {
    let rest = key.strip_prefix("h/")?;
    let (position, digest) = rest.split_once('-')?;
    if position.len() != 20 || digest.len() != 64 {
        return None;
    }
    let position = position.parse().ok()?;
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(digest.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    (hex(&out) == digest).then_some((position, out))
}

/// The portable condition of an object-storage failure.
fn failure(e: StorageError) -> AuthorityError {
    match e {
        StorageError::Unavailable(_) => AuthorityError::Unreachable,
        StorageError::Key(_) | StorageError::Io(_) => AuthorityError::Io(0),
    }
}

pub struct Objects {
    store: Arc<dyn ObjectStorage>,
    levels: Vec<String>,
    floor: Level,
    owner: u64,
    /// The records this handle has followed, each linked to the one before.
    chain: Chain,
    /// Groups got past the chain's end that are not yet linked to it, kept
    /// across a failed request so following the history makes progress.
    fetched: BTreeMap<String, Vec<u8>>,
    #[cfg(test)]
    pub(crate) bug: Option<Bug>,
}

impl Objects {
    /// The history in `store`, whose one level is `level`.
    pub fn new(store: Arc<dyn ObjectStorage>, level: String, floor: Level) -> Self {
        Objects {
            store,
            levels: vec![level],
            floor,
            owner: owner_id(),
            chain: Chain::default(),
            fetched: BTreeMap::new(),
            #[cfg(test)]
            bug: None,
        }
    }

    #[cfg(test)]
    fn has(&self, bug: Bug) -> bool {
        self.bug == Some(bug)
    }

    /// The head and its tag, `None` for an empty history.
    fn head(&self) -> Result<Option<(Head, Tag)>, Error> {
        match self.store.get_tagged(HEAD) {
            Ok(None) => Ok(None),
            Ok(Some((bytes, tag))) => Ok(Some((Head::decode(&bytes)?, tag))),
            Err(e) => Err(Error::Refused(failure(e))),
        }
    }

    /// Follow the history from what this handle knows to the head's `end`,
    /// returning the records after the known end, in order.
    fn catch_up(&mut self, end: u64, digest: [u8; 32]) -> Result<Vec<Record>, Error> {
        let known = self.chain.end();
        let anchor = |p: u64| {
            if p == 0 {
                [0; 32]
            } else {
                self.chain.digests[p as usize - 1]
            }
        };
        if end < known || (end == known && anchor(end) != digest) {
            return Err(Error::Corrupt(HEAD.to_string()));
        }
        if end == known {
            return Ok(Vec::new());
        }
        // A listing only lets the gets run at once; a group it misses is got
        // by name below.
        let mut listed = false;
        if end - known > 1 {
            let mut candidates = Vec::new();
            let mut after = format!("h/{known:020}~");
            while let Ok(page) = self.store.list("h/", &after) {
                listed = true;
                let mut past = false;
                for key in &page.keys {
                    match parse_group_key(key) {
                        Some((p, _)) if p <= end && !self.fetched.contains_key(key) => {
                            candidates.push(key.clone())
                        }
                        Some((p, _)) if p <= end => {}
                        Some(_) => past = true,
                        None => {}
                    }
                }
                match page.keys.last() {
                    Some(last) if page.truncated && !past => after = last.clone(),
                    _ => break,
                }
            }
            for (key, got) in candidates.iter().zip(self.store.get_many(&candidates)) {
                if let Ok(Some(bytes)) = got {
                    self.fetched.insert(key.clone(), bytes);
                }
            }
        }
        #[cfg(test)]
        let trust = listed && self.has(Bug::TrustListing);
        #[cfg(not(test))]
        let trust = !listed && false;
        let mut back = Vec::with_capacity((end - known) as usize);
        let (mut p, mut d) = (end, digest);
        while p > known {
            let key = group_key(p, &d);
            let bytes = match self.fetched.get(&key) {
                Some(bytes) => bytes.clone(),
                None if trust => return Err(Error::Corrupt(key)),
                None => match self.store.get(&key) {
                    Ok(Some(bytes)) => {
                        self.fetched.insert(key.clone(), bytes.clone());
                        bytes
                    }
                    Ok(None) => return Err(Error::Corrupt(key)),
                    Err(e) => return Err(Error::Refused(failure(e))),
                },
            };
            if envelope::digest(&bytes) != d {
                return Err(Error::Corrupt(key));
            }
            d = envelope::prev(&bytes).map_err(|b| crate::bad(b, p))?;
            back.push(bytes);
            p -= 1;
        }
        if d != anchor(known) {
            return Err(Error::Corrupt(group_key(
                known + 1,
                &envelope::digest(back.last().expect("at least one group was followed")),
            )));
        }
        back.reverse();
        self.fetched.clear();
        // Followed whole or not at all, so a bad record leaves the chain as it was.
        let mut chain = self.chain.clone();
        let mut records = Vec::with_capacity(back.len());
        for (i, b) in back.iter().enumerate() {
            records.push(
                chain
                    .follow(b)
                    .map_err(|e| crate::bad(e, known + i as u64 + 1))?,
            );
        }
        self.chain = chain;
        Ok(records)
    }

    /// Moves the head from the writer's own end to `last`, the newest record
    /// of a batch whose objects are all put, retrying while the head is still
    /// the writer's own. A replace that went unanswered may still land at any
    /// later moment, so no later answer, a retry's mismatch included, proves
    /// on its own that the batch is absent.
    fn replace_head(&mut self, c: &mut Claim, last: &Queued) -> Advanced {
        let head = Head::next(c, last.position, last.digest, &c.sweep).encode();
        let mut doubt = false;
        for _ in 0..REPLACE_TRIES {
            let tag = c
                .tag
                .clone()
                .expect("an objects claim holds its head's tag");
            #[cfg(test)]
            let tag = match self.has(Bug::NoCas).then(|| self.store.get_tagged(HEAD)) {
                Some(Ok(Some((_, fresh)))) => fresh,
                _ => tag,
            };
            match self.store.replace(HEAD, &head, Condition::Matches(tag)) {
                Ok(Replaced::Replaced(tag)) => {
                    c.tag = Some(tag);
                    c.revision += 1;
                    return advanced(c, last.position, last.digest);
                }
                Ok(Replaced::Mismatch) => {}
                Err(_) => doubt = true,
            }
            match self.settle(c, last.position, last.digest, doubt) {
                Settled::Done(a) => return a,
                Settled::Retry => {}
            }
        }
        dead(c, Advanced::Undecided(unreachable()))
    }

    /// Decides a batch whose head replaces to `position`, the record of digest
    /// `entry`, were answered with a mismatch or not at all. The record's
    /// digest covers every record before it, so the batch lands whole or not.
    fn settle(&mut self, c: &mut Claim, position: u64, entry: [u8; 32], doubt: bool) -> Settled {
        let (h, tag) = match self.head() {
            Ok(Some(read)) => read,
            // Every replace was answered with a mismatch: none had an effect.
            Err(e) if !doubt => return Settled::Done(Advanced::Refused(refused(e))),
            Ok(None) | Err(_) => return Settled::Done(dead(c, Advanced::Undecided(unreachable()))),
        };
        if h.attempt == c.attempt {
            if h.end == position && h.digest == entry && h.revision == c.revision + 1 {
                c.tag = Some(tag);
                c.revision += 1;
                return Settled::Done(advanced(c, position, entry));
            }
            if h.end == c.end && h.revision == c.revision {
                if doubt && c.tag.as_ref() != Some(&tag) {
                    return Settled::Done(dead(c, Advanced::Undecided(unreachable())));
                }
                // The head is still this writer's own at its own end, under the
                // tag its replaces are conditional on: retry the identical
                // request. Of it and any unanswered original at most one lands,
                // with the same bytes.
                c.tag = Some(tag);
                return Settled::Retry;
            }
            return Settled::Done(dead(c, Advanced::Undecided(unreachable())));
        }
        // A later claim holds the history. A replace of this append may have
        // landed before it, whatever this attempt was answered, so the chain
        // decides: the entry at its position is Advanced. Anything else there is
        // proof of absence, and the head has moved past the tag every replace
        // of this append was conditional on, which never recurs, so none of
        // them can land later: Fenced. A chain that cannot be read decides
        // nothing.
        match self.catch_up(h.end, h.digest) {
            Ok(_) if self.chain.digests.get(position as usize - 1) == Some(&entry) => {
                let a = advanced(c, position, entry);
                Settled::Done(dead(c, a))
            }
            Ok(_) => Settled::Done(dead(c, Advanced::Fenced)),
            Err(_) => Settled::Done(dead(c, Advanced::Undecided(unreachable()))),
        }
    }
}

enum Settled {
    Done(Advanced),
    /// The writer's tag was stale while the head is still its own: replace again.
    Retry,
}

/// Head replaces one append sends before it gives up as undecided.
const REPLACE_TRIES: u32 = 3;

fn unreachable() -> AuthorityError {
    AuthorityError::Unreachable
}

fn refused(e: Error) -> AuthorityError {
    match e {
        Error::Refused(e) | Error::Storage(e) => e,
        _ => AuthorityError::Unreachable,
    }
}

fn dead(c: &mut Claim, a: Advanced) -> Advanced {
    c.dead = true;
    a
}

fn advanced(c: &mut Claim, position: u64, digest: [u8; 32]) -> Advanced {
    c.end = position;
    c.digest = digest;
    c.durable[0] = position;
    Advanced::Advanced(position)
}

impl Authority for Objects {
    fn levels(&self) -> &[String] {
        &self.levels
    }
    fn floor(&self) -> Level {
        self.floor
    }
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        for _ in 0..CLAIM_TRIES {
            let read = self.head()?;
            let (end, digest) = read
                .as_ref()
                .map_or((0, [0; 32]), |(h, _)| (h.end, h.digest));
            let (revision, sweep) = read.as_ref().map_or((0, Sweep::default()), |(h, _)| {
                (h.revision, h.sweep.clone())
            });
            let holder = read.as_ref().map(|(h, _)| h.attempt);
            let condition = match &read {
                Some((_, tag)) => Condition::Matches(tag.clone()),
                None => Condition::Absent,
            };
            self.catch_up(end, digest)?;
            let entry = envelope::claimed(&self.chain.tip, attempt, fresh);
            let d = envelope::digest(&entry);
            let position = end + 1;
            self.store
                .put_new(&group_key(position, &d), &entry)
                .map_err(|e| Error::Refused(failure(e)))?;
            let head = Head {
                attempt,
                end: position,
                digest: d,
                revision: revision + 1,
                sweep: sweep.clone(),
            };
            let tag = match self.store.replace(HEAD, &head.encode(), condition) {
                Ok(Replaced::Replaced(tag)) => tag,
                Ok(Replaced::Mismatch) | Err(_) => match self.head()? {
                    Some((h, tag)) if h == head => tag,
                    Some((h, _)) if Some(h.attempt) != holder => return Err(Error::Fenced),
                    // The holder advanced, or the request was lost before its
                    // effect: claim again from the new head.
                    _ => continue,
                },
            };
            self.chain
                .follow(&entry)
                .expect("a claim record follows its tip");
            let mut claim = Claim::new(self.owner, attempt, &self.chain.tip, 1);
            claim.tag = Some(tag);
            claim.revision = revision + 1;
            claim.sweep = sweep;
            return Ok(Claimed {
                claim,
                end: position,
            });
        }
        Err(Error::Refused(AuthorityError::Unreachable))
    }

    fn append(&mut self, c: &mut Claim, batch: &[Queued]) -> Vec<Advanced> {
        if !live(self.owner, c) {
            c.dead = true;
            return crate::outcomes(batch, 0, |_| unreachable!());
        }
        // Every group object first. A failed put changed no head: at most it
        // left an orphan, and it and the entries after it are not appended.
        let mut put = 0;
        let mut failed = None;
        for q in batch {
            match self
                .store
                .put_new(&group_key(q.position, &q.digest), &q.record)
            {
                Ok(Put::Created | Put::Exists) => put += 1,
                Err(e) => {
                    failed = Some(Advanced::Refused(failure(e)));
                    break;
                }
            }
        }
        let head = match put {
            0 => Advanced::Fenced,
            _ => self.replace_head(c, &batch[put - 1]),
        };
        let mut out: Vec<Advanced> = batch
            .iter()
            .take(put)
            .map(|q| match &head {
                Advanced::Advanced(_) => Advanced::Advanced(q.position),
                other => other.clone(),
            })
            .collect();
        if let Some(f) = failed {
            c.dead = true;
            out.push(f);
        }
        out.resize(batch.len(), Advanced::Fenced);
        out
    }

    fn durable(&mut self, c: &mut Claim, _at: Level) -> Result<u64, Error> {
        if !live(self.owner, c) {
            c.dead = true;
            return Err(Error::Fenced);
        }
        match self.head()? {
            Some((h, _)) if h.attempt == c.attempt => Ok(c.durable[0]),
            _ => {
                c.dead = true;
                Err(Error::Fenced)
            }
        }
    }

    fn read(&mut self, from: u64, _at: Level, max: u64) -> Result<Vec<Record>, Error> {
        let Some((h, _)) = self.head()? else {
            return Ok(Vec::new());
        };
        let known = self.chain.end();
        let fresh = self.catch_up(h.end, h.digest)?;
        let stop = h.end.min(from.saturating_add(max));
        let start = from.min(stop);
        let older: Vec<String> = (start + 1..=stop.min(known))
            .map(|p| group_key(p, &self.chain.digests[p as usize - 1]))
            .collect();
        let mut out = Vec::with_capacity((stop - start) as usize);
        // Up to `max` entries: a failed get ends the answer early, and fails
        // the read only when it returns nothing.
        for (i, (key, got)) in older.iter().zip(self.store.get_many(&older)).enumerate() {
            let p = start + 1 + i as u64;
            match got {
                Ok(Some(bytes)) => {
                    out.push(self.chain.check(p, &bytes).map_err(|b| crate::bad(b, p))?)
                }
                Ok(None) => return Err(Error::Corrupt(key.clone())),
                Err(_) if !out.is_empty() => return Ok(out),
                Err(e) => return Err(Error::Refused(failure(e))),
            }
        }
        for p in (start + 1).max(known + 1)..=stop {
            out.push(fresh[(p - known - 1) as usize].clone());
        }
        Ok(out)
    }

    fn get_object(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let key = object_key(name)?;
        self.store.get(&key).map_err(|e| Error::Storage(failure(e)))
    }

    fn put_object(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
        let key = object_key(name)?;
        match self.store.put_new(&key, &data) {
            Ok(Put::Created) => Ok(()),
            Ok(Put::Exists) => match self.store.get(&key) {
                Ok(Some(old)) if old == data => Ok(()),
                Ok(_) => Err(Error::Corrupt(name.to_string())),
                Err(e) => Err(Error::Storage(failure(e))),
            },
            Err(e) => Err(Error::Storage(failure(e))),
        }
    }

    fn reclaim(&mut self, c: &mut Claim, budget: u64) -> Result<Reclaimed, Error> {
        if !live(self.owner, c) {
            c.dead = true;
            return Err(Error::Fenced);
        }
        let h = match self.head()? {
            Some((h, _)) if h.attempt == c.attempt => h,
            _ => {
                c.dead = true;
                return Err(Error::Fenced);
            }
        };
        self.catch_up(h.end, h.digest)?;
        reclaim::run(self, c, c.durable[0], budget)
    }
}

impl Space for Objects {
    fn page(&mut self, after: &str, s: u64) -> Result<Page, Error> {
        #[cfg(test)]
        let any_epoch = self.has(Bug::AnyEpoch);
        #[cfg(not(test))]
        let any_epoch = false;
        let listing = self
            .store
            .list("", after)
            .map_err(|e| Error::Storage(failure(e)))?;
        let keys = listing
            .keys
            .into_iter()
            .map(|key| {
                let found = if let Some((p, d)) = parse_group_key(&key) {
                    let named = p >= 1 && self.chain.digests.get(p as usize - 1) == Some(&d);
                    // Past the head's end may be the current writer's put whose
                    // head replace is still to come.
                    if !named && (p <= s || any_epoch) {
                        Found::Garbage
                    } else {
                        Found::Keep
                    }
                } else {
                    key.strip_prefix("o/")
                        .and_then(ObjectRef::parse)
                        .map_or(Found::Keep, Found::Object)
                };
                (key, found)
            })
            .collect();
        Ok(Page {
            keys,
            more: listing.truncated,
        })
    }

    fn remove(&mut self, key: &str) -> Result<(), Error> {
        self.store
            .delete(key)
            .map_err(|e| Error::Storage(failure(e)))
    }

    fn key(&self, r: &ObjectRef) -> String {
        format!("o/{}", r.name())
    }

    fn records(&mut self, from: u64, max: u64) -> Result<Vec<Record>, Error> {
        self.read(from, Level(0), max)
    }

    /// Replaces the head with the claim's own end and `sweep`, on the claim's
    /// own tag, settling an unanswered or mismatched replace by the head it
    /// reads: its own new bytes landed, its own old bytes under another tag
    /// retry, anything else is a later claim.
    fn publish(&mut self, c: &mut Claim, sweep: &Sweep) -> Result<(), Error> {
        let head = Head::next(c, c.end, c.digest, sweep);
        let bytes = head.encode();
        for _ in 0..REPLACE_TRIES {
            let tag = c
                .tag
                .clone()
                .expect("an objects claim holds its head's tag");
            if let Ok(Replaced::Replaced(tag)) =
                self.store.replace(HEAD, &bytes, Condition::Matches(tag))
            {
                c.tag = Some(tag);
                c.revision += 1;
                return Ok(());
            }
            match self.head() {
                Ok(Some((h, tag))) if h == head => {
                    c.tag = Some(tag);
                    c.revision += 1;
                    return Ok(());
                }
                Ok(Some((h, tag)))
                    if h.attempt == c.attempt
                        && h.revision == c.revision
                        && (h.end, h.digest) == (c.end, c.digest) =>
                {
                    c.tag = Some(tag);
                }
                Ok(_) => {
                    c.dead = true;
                    return Err(Error::Fenced);
                }
                Err(_) => break,
            }
        }
        c.dead = true;
        Err(Error::Storage(unreachable()))
    }
}

/// The storage key of a named object, refusing a name outside the key grammar.
fn object_key(name: &str) -> Result<String, Error> {
    let key = format!("o/{name}");
    hirsel_object_storage::check_key(&key)
        .map_err(|_| Error::Refused(AuthorityError::io(std::io::ErrorKind::InvalidInput)))?;
    Ok(key)
}

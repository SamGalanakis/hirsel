//! `authority.objects@1`: the history in an object-storage entry.
//!
//! One level, the one the grant names: whatever the storage survives, every
//! acknowledged write survives with it. The history is a chain of immutable
//! group objects `h/<position as 20 digits>-<digest>` (the SHA-256 of the
//! entry, lowercase hexadecimal), each entry naming the digest of the one
//! before it, and one mutable object, `head`:
//!
//! ```text
//! head = "HSOBJH01" attempt:[16] end:u64 digest:[32] sum:[32]
//! ```
//!
//! naming the newest claim's attempt, the history's end and the last entry's
//! digest; `sum` is the SHA-256 of everything before it. The head changes only
//! by a conditional replace on the entity tag the writer's own previous replace
//! returned. Since a head names its claim and end, no two heads have equal
//! bytes and no tag recurs.
//!
//! An advance is two requests: create the group object if absent, then replace
//! the head. A claim reads the head, follows the history to its end, creates its
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
//! Reclamation lists `h/` and removes a group object at a position the head
//! has reached that the chain does not name: left by a writer fenced or
//! undecided before its head replace. It never removes one past the head's end,
//! which may be the current writer's advance between its two requests.

use crate::group::{self, Tip};
use crate::{
    Advanced, Authority, AuthorityError, Claim, Claimed, Error, Level, MAX_ENTRY, live, owner_id,
};
use hirsel_object_storage::{Condition, ObjectStorage, Put, Replaced, StorageError, Tag};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

const HEAD: &str = "head";
const MAGIC: &[u8; 8] = b"HSOBJH01";
/// The magic's family: other version digits are another format version.
const FAMILY: &[u8; 6] = b"HSOBJH";
const HEAD_BYTES: usize = 8 + 16 + 8 + 32 + 32;
/// Attempts of a claim whose head replace lost to the current writer's advance.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Head {
    attempt: [u8; 16],
    end: u64,
    digest: [u8; 32],
}

impl Head {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEAD_BYTES);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.attempt);
        out.extend_from_slice(&self.end.to_le_bytes());
        out.extend_from_slice(&self.digest);
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
        if b.len() != HEAD_BYTES || &b[..8] != MAGIC {
            return Err(corrupt());
        }
        let body = HEAD_BYTES - 32;
        if Sha256::digest(&b[..body])[..] != b[body..] {
            return Err(corrupt());
        }
        Ok(Head {
            attempt: b[8..24].try_into().unwrap(),
            end: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            digest: b[32..64].try_into().unwrap(),
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
    /// The digest of each entry this handle has followed, by position - 1,
    /// each linked to the one before.
    chain: Vec<[u8; 32]>,
    /// The tip after `chain`.
    tip: Tip,
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
            chain: Vec::new(),
            tip: Tip::origin(),
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
    /// returning the entries after the known end, in order.
    fn catch_up(&mut self, end: u64, digest: [u8; 32]) -> Result<Vec<Vec<u8>>, Error> {
        let known = self.chain.len() as u64;
        let anchor = |p: u64| {
            if p == 0 {
                [0; 32]
            } else {
                self.chain[p as usize - 1]
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
            if group::digest(&bytes) != d {
                return Err(Error::Corrupt(key));
            }
            d = group::prev(&bytes).map_err(|b| crate::bad(b, p))?;
            back.push(bytes);
            p -= 1;
        }
        if d != anchor(known) {
            return Err(Error::Corrupt(group_key(
                known + 1,
                &group::digest(back.last().expect("at least one group was followed")),
            )));
        }
        back.reverse();
        self.fetched.clear();
        let mut tip = self.tip.clone();
        for (i, entry) in back.iter().enumerate() {
            tip = group::follow(&tip, entry).map_err(|b| crate::bad(b, known + i as u64 + 1))?;
        }
        self.tip = tip;
        self.chain.extend(back.iter().map(|e| group::digest(e)));
        Ok(back)
    }

    /// Settle a head replace that did not report success. `doubt` is whether
    /// any replace this advance sent went unanswered: such a request may still
    /// land at any later moment, so no later answer, a retry's mismatch
    /// included, proves on its own that the entry is absent.
    fn settle(&mut self, c: &mut Claim, entry: [u8; 32], doubt: bool) -> Settled {
        let position = c.end + 1;
        let (h, tag) = match self.head() {
            Ok(Some(read)) => read,
            // Every replace was answered with a mismatch: none had an effect.
            Err(e) if !doubt => return Settled::Done(Advanced::Refused(refused(e))),
            Ok(None) | Err(_) => return Settled::Done(dead(c, Advanced::Undecided(unreachable()))),
        };
        if h.attempt == c.attempt {
            if h.end == position && h.digest == entry {
                c.tag = Some(tag);
                return Settled::Done(advanced(c, position));
            }
            if h.end == c.end {
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
        // A later claim holds the history. A replace of this advance may have
        // landed before it, whatever this attempt was answered, so the chain
        // decides: the entry at its position is Advanced. Anything else there is
        // proof of absence, and the head has moved past the tag every replace
        // of this advance was conditional on, which never recurs, so none of
        // them can land later: Fenced. A chain that cannot be read decides
        // nothing.
        match self.catch_up(h.end, h.digest) {
            Ok(_) if self.chain.get(c.end as usize) == Some(&entry) => {
                c.end = position;
                c.durable[0] = position;
                c.dead = true;
                Settled::Done(Advanced::Advanced(position))
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

/// Head replaces one advance sends before it gives up as undecided.
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

fn advanced(c: &mut Claim, position: u64) -> Advanced {
    c.end = position;
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
    fn max_entry(&self) -> u64 {
        MAX_ENTRY
    }

    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        for _ in 0..CLAIM_TRIES {
            let read = self.head()?;
            let (end, digest) = read
                .as_ref()
                .map_or((0, [0; 32]), |(h, _)| (h.end, h.digest));
            let holder = read.as_ref().map(|(h, _)| h.attempt);
            let condition = match &read {
                Some((_, tag)) => Condition::Matches(tag.clone()),
                None => Condition::Absent,
            };
            self.catch_up(end, digest)?;
            let entry = group::claim_group(&self.tip, attempt, fresh);
            let d = group::digest(&entry);
            let position = end + 1;
            self.store
                .put_new(&group_key(position, &d), &entry)
                .map_err(|e| Error::Refused(failure(e)))?;
            let head = Head {
                attempt,
                end: position,
                digest: d,
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
            self.tip = group::follow(&self.tip, &entry).expect("a claim group follows its tip");
            self.chain.push(d);
            let claim = Claim {
                owner: self.owner,
                attempt,
                end: position,
                durable: vec![position],
                segment: 0,
                length: 0,
                tag: Some(tag),
                dead: false,
            };
            return Ok(Claimed {
                claim,
                end: position,
            });
        }
        Err(Error::Refused(AuthorityError::Unreachable))
    }

    fn advance(&mut self, c: &mut Claim, from: u64, entry: Vec<u8>, _at: Level) -> Advanced {
        if !live(self.owner, c) || from != c.end {
            return dead(c, Advanced::Fenced);
        }
        if entry.len() as u64 > MAX_ENTRY {
            return Advanced::Refused(AuthorityError::io(std::io::ErrorKind::InvalidInput));
        }
        let d = group::digest(&entry);
        let position = from + 1;
        // A failed put changed no head: at most it left an orphan.
        match self.store.put_new(&group_key(position, &d), &entry) {
            Ok(Put::Created | Put::Exists) => {}
            Err(e) => return Advanced::Refused(failure(e)),
        }
        let head = Head {
            attempt: c.attempt,
            end: position,
            digest: d,
        }
        .encode();
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
                    return advanced(c, position);
                }
                Ok(Replaced::Mismatch) => {}
                Err(_) => doubt = true,
            }
            match self.settle(c, d, doubt) {
                Settled::Done(a) => return a,
                Settled::Retry => {}
            }
        }
        dead(c, Advanced::Undecided(unreachable()))
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

    fn read(&mut self, from: u64, _at: Level, max: u64) -> Result<Vec<Vec<u8>>, Error> {
        let Some((h, _)) = self.head()? else {
            return Ok(Vec::new());
        };
        let known = self.chain.len() as u64;
        let fresh = self.catch_up(h.end, h.digest)?;
        let stop = h.end.min(from.saturating_add(max));
        let start = from.min(stop);
        let older: Vec<String> = (start + 1..=stop.min(known))
            .map(|p| group_key(p, &self.chain[p as usize - 1]))
            .collect();
        let mut out = Vec::with_capacity((stop - start) as usize);
        // Up to `max` entries: a failed get ends the answer early, and fails
        // the read only when it returns nothing.
        for (key, got) in older.iter().zip(self.store.get_many(&older)) {
            match got {
                Ok(Some(bytes))
                    if parse_group_key(key).map(|(_, d)| d) == Some(group::digest(&bytes)) =>
                {
                    out.push(bytes)
                }
                Ok(_) => return Err(Error::Corrupt(key.clone())),
                Err(_) if !out.is_empty() => return Ok(out),
                Err(e) => return Err(Error::Refused(failure(e))),
            }
        }
        for p in (start + 1).max(known + 1)..=stop {
            out.push(fresh[(p - known - 1) as usize].clone());
        }
        Ok(out)
    }

    fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let key = object_key(name)?;
        self.store.get(&key).map_err(|e| Error::Storage(failure(e)))
    }

    fn put(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
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

    fn reclaim(&mut self, c: &mut Claim) -> Result<(), Error> {
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
        #[cfg(test)]
        let any_epoch = self.has(Bug::AnyEpoch);
        #[cfg(not(test))]
        let any_epoch = false;
        let mut after = String::new();
        loop {
            let page = self
                .store
                .list("h/", &after)
                .map_err(|e| Error::Storage(failure(e)))?;
            for key in &page.keys {
                let Some((p, d)) = parse_group_key(key) else {
                    continue;
                };
                let named = p >= 1 && self.chain.get(p as usize - 1) == Some(&d);
                // Past the head's end may be the current writer's put whose
                // head replace is still to come.
                if !named && (p <= h.end || any_epoch) {
                    self.store
                        .delete(key)
                        .map_err(|e| Error::Storage(failure(e)))?;
                }
            }
            match page.keys.last() {
                Some(last) if page.truncated => after = last.clone(),
                _ => return Ok(()),
            }
        }
    }
}

/// The storage key of a named object, refusing a name outside the key grammar.
fn object_key(name: &str) -> Result<String, Error> {
    let key = format!("o/{name}");
    hirsel_object_storage::check_key(&key)
        .map_err(|_| Error::Refused(AuthorityError::io(std::io::ErrorKind::InvalidInput)))?;
    Ok(key)
}

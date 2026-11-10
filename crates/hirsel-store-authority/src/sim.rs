//! `authority.sim@1`: a simulated authority, the one fault path. With
//! `represent: "log"` it keeps a log representation by the log's rules and
//! injects, by seed, what a store must survive by detecting and refusing: the
//! loss of anything not yet durable at a level, torn and garbage tails, refused
//! requests, lost responses and failed barriers. Handles made by [`Sim::share`]
//! race on one history. With `represent: "objects"` it is
//! [`crate::objects::Objects`] over a [`Bucket`], a simulated object storage
//! that refuses requests, loses responses after their effect and returns
//! incomplete listings.
//!
//! Levels are the grant's, weakest first. An append is durable at the weakest
//! level; a barrier at a level makes everything appended durable at it and
//! every weaker one. [`Sim::crash`] is a failure that levels from `k` up
//! survive: every entry not durable at level `k` is lost.

use crate::group::{self, Tip};
use crate::{
    Advanced, Authority, AuthorityError, AuthorityGrant, Claim, Claimed, ConfigError, Error, Level,
    MAX_ENTRY, level_name_ok, live, owner_id,
};
use hirsel_object_storage::{
    Condition, LIST_PAGE, Listing, ObjectStorage, Put, Replaced, StorageError, Tag, check_key,
    content_tag,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The faults the simulated log injects, each with a `fail_one_in` count.
pub const FAULTS: [&str; 5] = ["barrier", "garbage", "lost_response", "refuse", "torn"];
/// The faults the simulated object storage injects.
pub const OBJECT_FAULTS: [&str; 3] = ["incomplete_listing", "lost_response", "refuse"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Represent {
    Log,
    Objects,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub seed: u64,
    pub represent: Represent,
    pub levels: Vec<String>,
    /// `fail_one_in` by fault name, 0 for never.
    pub faults: BTreeMap<String, u64>,
}

impl Config {
    pub fn from_grant(g: &AuthorityGrant) -> Result<Self, ConfigError> {
        let fail = |m: String| ConfigError(m);
        let represent = match g.represent.as_deref() {
            Some("log") => Represent::Log,
            Some("objects") => Represent::Objects,
            _ => return Err(fail("sim `represent` is \"log\" or \"objects\"".into())),
        };
        let levels = g
            .levels
            .clone()
            .ok_or_else(|| fail("sim `levels` is a list of level names".into()))?;
        if levels.is_empty() || levels.len() > 255 {
            return Err(fail("sim `levels` names 1 to 255 levels".into()));
        }
        for (i, l) in levels.iter().enumerate() {
            if !level_name_ok(l) || levels[..i].contains(l) {
                return Err(fail(format!(
                    "sim level `{l}` is not a distinct lowercase name"
                )));
            }
        }
        if represent == Represent::Objects && levels.len() != 1 {
            return Err(fail(
                "sim `represent: objects` declares one level, as object storage does".into(),
            ));
        }
        let known: &[&str] = match represent {
            Represent::Log => &FAULTS,
            Represent::Objects => &OBJECT_FAULTS,
        };
        let faults = g.faults.clone().unwrap_or_default();
        if let Some(k) = faults.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(fail(format!("unknown sim fault `{k}`")));
        }
        Ok(Config {
            seed: g.seed.unwrap_or(0),
            represent,
            levels,
            faults,
        })
    }
}

/// SplitMix64: the seeded stream every fault decision is drawn from.
struct Draws(u64);

impl Draws {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Deliberate defects the harness must catch (its negative controls).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Bug {
    /// Advance appends without checking the claim or the end.
    NoFence,
    /// Advance acknowledges a level without its barrier.
    AckWithoutBarrier,
    /// Read returns entries not yet durable at the level asked.
    ReadWithoutBarrier,
}

struct World {
    entries: Vec<Vec<u8>>,
    /// The highest position durable at each level, weakest first; never
    /// greater at a stronger level than at a weaker one.
    durable: Vec<u64>,
    /// Bytes of an append a crash cut short, after the last entry.
    torn: Option<Vec<u8>>,
    newest: Option<[u8; 16]>,
    objects: BTreeMap<String, Vec<u8>>,
    draws: Draws,
    faults: BTreeMap<String, u64>,
    #[cfg(test)]
    bug: Option<Bug>,
}

impl World {
    fn fault(&mut self, name: &str) -> bool {
        let one_in = self.faults.get(name).copied().unwrap_or(0);
        one_in != 0 && self.draws.next().is_multiple_of(one_in)
    }

    /// Make everything appended durable at `at` and every weaker level.
    fn barrier(&mut self, at: Level) {
        let end = self.entries.len() as u64;
        for d in &mut self.durable[..=at.0 as usize] {
            *d = end;
        }
    }
}

pub struct Sim {
    world: Arc<Mutex<World>>,
    levels: Vec<String>,
    floor: Level,
    owner: u64,
}

impl Sim {
    pub fn new(config: Config, floor: Level) -> Self {
        let n = config.levels.len();
        let world = World {
            entries: Vec::new(),
            durable: vec![0; n],
            torn: None,
            newest: None,
            objects: BTreeMap::new(),
            draws: Draws(config.seed),
            faults: config.faults,
            #[cfg(test)]
            bug: None,
        };
        Sim {
            world: Arc::new(Mutex::new(world)),
            levels: config.levels,
            floor,
            owner: owner_id(),
        }
    }

    /// Another handle on the same history.
    pub fn share(&self) -> Self {
        Sim {
            world: self.world.clone(),
            levels: self.levels.clone(),
            floor: self.floor,
            owner: owner_id(),
        }
    }

    #[cfg(test)]
    pub(crate) fn plant(&self, bug: Bug) {
        self.world.lock().unwrap().bug = Some(bug);
    }

    /// A failure that the levels from `k` up survive: every entry not durable
    /// at level `k` is lost. The first lost append may be left torn, or its
    /// place taken by garbage that recovery must refuse.
    pub fn crash(&self, k: Level) {
        let mut w = self.world.lock().unwrap();
        let keep = w.durable[k.0 as usize];
        let lost = w.entries.get(keep as usize).cloned();
        w.entries.truncate(keep as usize);
        w.torn = None;
        for d in &mut w.durable {
            *d = (*d).min(keep);
        }
        if let Some(lost) = lost {
            if w.fault("garbage") {
                let n = lost.len().max(1);
                let garbage: Vec<u8> = (0..n).map(|_| w.draws.next() as u8).collect();
                w.entries.push(garbage);
            } else if w.fault("torn") {
                let cut = (w.draws.next() % lost.len().max(1) as u64) as usize;
                w.torn = Some(lost[..cut].to_vec());
            }
        }
        // A crash ends every writer; the newest claim entry still fences them.
        w.newest = None;
    }

    #[cfg(test)]
    fn has(&self, bug: Bug) -> bool {
        self.world.lock().unwrap().bug == Some(bug)
    }
}

impl Authority for Sim {
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
        let top = Level(self.levels.len() as u8 - 1);
        let mut w = self.world.lock().unwrap();
        if w.fault("refuse") {
            return Err(Error::Refused(AuthorityError::Unreachable));
        }
        w.torn = None;
        let mut tip = Tip::origin();
        for (i, e) in w.entries.iter().enumerate() {
            tip = group::follow(&tip, e).map_err(|b| crate::bad(b, i as u64 + 1))?;
        }
        let entry = group::claim_group(&tip, attempt, fresh);
        w.entries.push(entry);
        // A claim is a synchronized append: durable at every level.
        w.barrier(top);
        w.newest = Some(attempt);
        let end = w.entries.len() as u64;
        let claim = Claim {
            owner: self.owner,
            attempt,
            end,
            durable: vec![end; self.levels.len()],
            segment: 0,
            length: 0,
            tag: None,
            dead: false,
        };
        Ok(Claimed { claim, end })
    }

    fn advance(&mut self, c: &mut Claim, from: u64, entry: Vec<u8>, at: Level) -> Advanced {
        #[cfg(test)]
        let (no_fence, no_barrier) = (self.has(Bug::NoFence), self.has(Bug::AckWithoutBarrier));
        #[cfg(not(test))]
        let (no_fence, no_barrier) = (false, false);
        let mut w = self.world.lock().unwrap();
        if !no_fence
            && (!live(self.owner, c)
                || w.newest != Some(c.attempt)
                || from != c.end
                || w.entries.len() as u64 != from)
        {
            c.dead = true;
            return Advanced::Fenced;
        }
        if entry.len() as u64 > MAX_ENTRY {
            return Advanced::Refused(AuthorityError::io(std::io::ErrorKind::InvalidInput));
        }
        if w.fault("refuse") {
            return Advanced::Refused(AuthorityError::Unreachable);
        }
        let lost = w.fault("lost_response");
        if lost && w.draws.next().is_multiple_of(2) {
            // The request was lost: the provider reads the history, finds it
            // still ends at `from`, and reports no effect, unless it cannot read.
            if w.fault("refuse") {
                c.dead = true;
                return Advanced::Undecided(AuthorityError::Unreachable);
            }
            return Advanced::Refused(AuthorityError::Unreachable);
        }
        w.entries.push(entry);
        let position = w.entries.len() as u64;
        c.end = position;
        // Whatever is appended is durable at the weakest level.
        w.durable[0] = position;
        if lost && w.fault("refuse") {
            // The response was lost and the history cannot be read.
            c.dead = true;
            return Advanced::Undecided(AuthorityError::Unreachable);
        }
        if at.0 > 0 && !no_barrier {
            if w.fault("barrier") {
                c.dead = true;
                return Advanced::Undecided(AuthorityError::io(std::io::ErrorKind::Other));
            }
            w.barrier(at);
        }
        for d in &mut c.durable[..=at.0 as usize] {
            *d = position;
        }
        Advanced::Advanced(position)
    }

    fn durable(&mut self, c: &mut Claim, at: Level) -> Result<u64, Error> {
        let mut w = self.world.lock().unwrap();
        if !live(self.owner, c) || w.newest != Some(c.attempt) {
            c.dead = true;
            return Err(Error::Fenced);
        }
        let i = at.0 as usize;
        if c.durable[i] < c.end {
            if w.fault("barrier") {
                c.dead = true;
                return Err(Error::Storage(AuthorityError::io(
                    std::io::ErrorKind::Other,
                )));
            }
            w.barrier(at);
            for d in &mut c.durable[..=i] {
                *d = c.end;
            }
        }
        Ok(c.durable[i])
    }

    fn read(&mut self, from: u64, at: Level, max: u64) -> Result<Vec<Vec<u8>>, Error> {
        #[cfg(test)]
        let no_barrier = self.has(Bug::ReadWithoutBarrier);
        #[cfg(not(test))]
        let no_barrier = false;
        let mut w = self.world.lock().unwrap();
        if w.fault("refuse") {
            return Err(Error::Refused(AuthorityError::Unreachable));
        }
        // Like the log, a reader issues the barrier itself, so it never
        // returns an entry a failure at `at` could lose.
        if !no_barrier {
            if at.0 > 0 && w.fault("barrier") {
                return Err(Error::Storage(AuthorityError::io(
                    std::io::ErrorKind::Other,
                )));
            }
            w.barrier(at);
        }
        let end = w.entries.len();
        let start = (from as usize).min(end);
        let stop = start.saturating_add(max as usize).min(end);
        Ok(w.entries[start..stop].to_vec())
    }

    fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let mut w = self.world.lock().unwrap();
        if w.fault("refuse") {
            return Err(Error::Refused(AuthorityError::Unreachable));
        }
        Ok(w.objects.get(name).cloned())
    }

    fn put(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
        let mut w = self.world.lock().unwrap();
        if w.fault("refuse") {
            return Err(Error::Refused(AuthorityError::Unreachable));
        }
        match w.objects.get(name) {
            Some(old) if *old == data => Ok(()),
            Some(_) => Err(Error::Corrupt(name.to_string())),
            None => {
                w.objects.insert(name.to_string(), data);
                Ok(())
            }
        }
    }

    fn reclaim(&mut self, c: &mut Claim) -> Result<(), Error> {
        let w = self.world.lock().unwrap();
        if !live(self.owner, c) || w.newest != Some(c.attempt) {
            drop(w);
            c.dead = true;
            return Err(Error::Fenced);
        }
        Ok(())
    }
}

/// A simulated object storage with a bucket's conditional-write semantics,
/// drawing every fault from one seeded stream: `refuse` fails a request before
/// its effect, `lost_response` after it, and `incomplete_listing` drops each key
/// of a listing page with probability one half. It counts requests.
pub struct Bucket {
    world: Mutex<BucketWorld>,
    /// Called before every request, outside the bucket's lock: the harness's
    /// scheduler interleaves racing handles here.
    #[cfg(test)]
    pub(crate) before: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct BucketWorld {
    objects: BTreeMap<String, Vec<u8>>,
    draws: Draws,
    faults: BTreeMap<String, u64>,
    requests: u64,
}

impl BucketWorld {
    fn fault(&mut self, name: &str) -> bool {
        let one_in = self.faults.get(name).copied().unwrap_or(0);
        one_in != 0 && self.draws.next().is_multiple_of(one_in)
    }
}

fn unavailable(what: &str) -> StorageError {
    StorageError::Unavailable(format!("simulated {what}"))
}

impl Bucket {
    pub fn new(seed: u64, faults: BTreeMap<String, u64>) -> Self {
        Bucket {
            world: Mutex::new(BucketWorld {
                objects: BTreeMap::new(),
                draws: Draws(seed),
                faults,
                requests: 0,
            }),
            #[cfg(test)]
            before: Mutex::new(None),
        }
    }

    /// Requests served so far.
    pub fn requests(&self) -> u64 {
        self.world.lock().unwrap().requests
    }

    /// Every object, read past any fault and uncounted.
    pub fn contents(&self) -> BTreeMap<String, Vec<u8>> {
        self.world.lock().unwrap().objects.clone()
    }

    /// Start a request: run the scheduler hook, then count it and draw
    /// whether it is refused.
    fn request(&self) -> Result<std::sync::MutexGuard<'_, BucketWorld>, StorageError> {
        #[cfg(test)]
        {
            let before = self.before.lock().unwrap().clone();
            if let Some(f) = before {
                f();
            }
        }
        let mut w = self.world.lock().unwrap();
        w.requests += 1;
        if w.fault("refuse") {
            return Err(unavailable("refusal"));
        }
        Ok(w)
    }

    /// End a request that took effect: its response may be lost.
    fn respond<T>(w: &mut BucketWorld, value: T) -> Result<T, StorageError> {
        if w.fault("lost_response") {
            return Err(unavailable("lost response"));
        }
        Ok(value)
    }
}

impl ObjectStorage for Bucket {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        check_key(key)?;
        let w = self.request()?;
        Ok(w.objects.get(key).map(|b| (b.clone(), content_tag(b))))
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        check_key(key)?;
        let mut w = self.request()?;
        let put = if w.objects.contains_key(key) {
            Put::Exists
        } else {
            w.objects.insert(key.into(), bytes.to_vec());
            Put::Created
        };
        Self::respond(&mut w, put)
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        check_key(key)?;
        let mut w = self.request()?;
        let current = w.objects.get(key).map(|b| content_tag(b));
        let holds = match &condition {
            Condition::Absent => current.is_none(),
            Condition::Matches(tag) => current.as_ref() == Some(tag),
        };
        let replaced = if holds {
            w.objects.insert(key.into(), bytes.to_vec());
            Replaced::Replaced(content_tag(bytes))
        } else {
            Replaced::Mismatch
        };
        Self::respond(&mut w, replaced)
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        let mut w = self.request()?;
        let mut keys: Vec<String> = w
            .objects
            .keys()
            .filter(|k| k.starts_with(prefix) && k.as_str() > after)
            .take(LIST_PAGE + 1)
            .cloned()
            .collect();
        let truncated = keys.len() > LIST_PAGE;
        keys.truncate(LIST_PAGE);
        if w.fault("incomplete_listing") {
            keys.retain(|_| w.draws.next() & 1 == 0);
        }
        Ok(Listing { keys, truncated })
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        check_key(key)?;
        let mut w = self.request()?;
        w.objects.remove(key);
        Self::respond(&mut w, ())
    }
}

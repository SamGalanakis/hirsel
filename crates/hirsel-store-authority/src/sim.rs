//! `authority.sim@1`: a simulated authority, the one fault path. With
//! `represent: "log"` it keeps a log representation by the log's rules and
//! injects, by seed, what a store must survive by detecting and refusing: the
//! loss of anything not yet durable at a level, torn and garbage tails, refused
//! requests, lost responses and failed barriers. Handles made by [`Sim::share`]
//! race on one history. With `represent: "objects"` it is
//! [`crate::objects::Objects`] over a [`Bucket`], a simulated object storage
//! that refuses requests, loses responses after their effect and returns
//! incomplete listings. Over `objects.sim@1`, `claim_at` makes another client
//! claim the history inside a request sequence (see [`Client`]).
//!
//! Levels are the grant's, weakest first. An append is durable at the weakest
//! level; a barrier at a level makes everything appended durable at it and
//! every weaker one. [`Sim::crash`] is a failure that levels from `k` up
//! survive: every entry not durable at level `k` is lost.
//!
//! A log representation also cuts its history at a chosen request, for tests
//! that must reach every seam call in turn. Its requests (claim, wait,
//! durable, read, get, put and reclaim, on every handle of one history) are
//! numbered from 1, and each of the faults [`CUTS`] names the request it
//! fires at: `fail_at` refuses it before any effect; `lose_response_at` lets
//! it take its whole effect and then answers that the history cannot be read,
//! so the claim dies; `tear_at` is a power failure partway through it, which
//! leaves its own append, if any, torn and so never in the history; and
//! `crash_at` is a power failure right after its answer. A power failure
//! loses everything not durable at the strongest level and ends every claim.
//! A reclaim call is one request whatever its budget, its continuation kept
//! in the world, which a failure never loses.

use crate::envelope::{self, Chain};
use crate::reclaim::{self, Found, Page, Space, Sweep};
use crate::{
    Advanced, Authority, AuthorityError, AuthorityGrant, Claim, Claimed, ConfigError, Error, Level,
    Locator, ObjectRef, Queued, Reclaimed, Record, level_name_ok, live, owner_id,
};
use hirsel_object_storage::{
    Condition, LIST_PAGE, Listing, ObjectStorage, Put, Replaced, StorageError, Tag, check_key,
    content_tag,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The faults the simulated log injects, each with a `fail_one_in` count.
pub const FAULTS: [&str; 5] = ["barrier", "garbage", "lost_response", "refuse", "torn"];
/// The simulated log's cuts, each naming the request (from 1) it fires at.
pub const CUTS: [&str; 4] = ["crash_at", "fail_at", "lose_response_at", "tear_at"];
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
        let known: Vec<&str> = match represent {
            Represent::Log => FAULTS.iter().chain(CUTS.iter()).copied().collect(),
            Represent::Objects => OBJECT_FAULTS.to_vec(),
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
    /// Reclamation's continuation and its revision, which a failure keeps.
    sweep: (u64, Sweep),
    draws: Draws,
    faults: BTreeMap<String, u64>,
    /// Requests served so far, which number the cuts.
    requests: u64,
    #[cfg(test)]
    bug: Option<Bug>,
}

/// Which cuts fire at one request.
#[derive(Clone, Copy, Default)]
struct Cut {
    fail: bool,
    lose: bool,
    tear: bool,
    crash: bool,
}

impl World {
    /// The chain of the history's first `end` records.
    fn follow(&self, end: usize) -> Result<Chain, Error> {
        let mut chain = Chain::default();
        for (i, e) in self.entries[..end].iter().enumerate() {
            chain.follow(e).map_err(|b| crate::bad(b, i as u64 + 1))?;
        }
        Ok(chain)
    }

    /// Count a request and say which cuts fire at it.
    fn cut(&mut self) -> Cut {
        self.requests += 1;
        let n = self.requests;
        let at = |k: &str| self.faults.get(k) == Some(&n);
        Cut {
            fail: at("fail_at"),
            lose: at("lose_response_at"),
            tear: at("tear_at"),
            crash: at("crash_at"),
        }
    }

    /// A power failure: every entry not durable at the strongest level is lost
    /// and every claim ends.
    /// Lose everything not durable at `k`, as the simulated disk's crash does: the first
    /// lost entry may survive as garbage or a torn prefix when those faults fire.
    fn crash(&mut self, k: Level) {
        let keep = self.durable[k.0 as usize];
        let lost = self.entries.get(keep as usize).cloned();
        self.entries.truncate(keep as usize);
        self.torn = None;
        for d in &mut self.durable {
            *d = (*d).min(keep);
        }
        if let Some(lost) = lost {
            if self.fault("garbage") {
                let n = lost.len().max(1);
                let garbage: Vec<u8> = (0..n).map(|_| self.draws.next() as u8).collect();
                self.entries.push(garbage);
            } else if self.fault("torn") {
                let cut = (self.draws.next() % lost.len().max(1) as u64) as usize;
                self.torn = Some(lost[..cut].to_vec());
            }
        }
        // A crash ends every writer; the newest claim entry still fences them.
        self.newest = None;
    }

    /// A power failure: the crash at the strongest level.
    fn power_fail(&mut self) {
        self.crash(Level(self.durable.len() as u8 - 1));
    }

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
            sweep: (0, Sweep::default()),
            draws: Draws(config.seed),
            faults: config.faults,
            requests: 0,
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
        self.world.lock().unwrap().crash(k);
    }
}

impl Sim {
    /// A reading request under the cut its request number draws.
    fn reading<T>(
        &self,
        run: impl FnOnce(&Sim, &mut World) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let r = if cut.fail {
            Err(Error::Refused(unreadable()))
        } else if cut.tear {
            w.power_fail();
            Err(Error::Storage(unreadable()))
        } else {
            match run(self, &mut w) {
                Ok(_) if cut.lose => Err(Error::Storage(unreadable())),
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
    }

    fn claim_in(
        &self,
        w: &mut World,
        attempt: [u8; 16],
        fresh: [u8; 32],
    ) -> Result<Claimed, Error> {
        let top = Level(self.levels.len() as u8 - 1);
        if w.fault("refuse") {
            return Err(Error::Refused(AuthorityError::Unreachable));
        }
        w.torn = None;
        let mut chain = w.follow(w.entries.len())?;
        let entry = envelope::claimed(&chain.tip, attempt, fresh, chain.appended());
        chain
            .follow(&entry)
            .expect("a claim record follows its tip");
        w.entries.push(entry);
        // A claim is a synchronized append: durable at every level.
        w.barrier(top);
        w.newest = Some(attempt);
        let mut claim = Claim::new(self.owner, attempt, &chain.tip, self.levels.len());
        (claim.revision, claim.sweep) = w.sweep.clone();
        let end = claim.end;
        Ok(Claimed { claim, end })
    }

    fn append_in(&self, w: &mut World, c: &mut Claim, batch: &[Queued]) -> Vec<Advanced> {
        #[cfg(test)]
        let (no_fence, no_barrier) = (
            w.bug == Some(Bug::NoFence),
            w.bug == Some(Bug::AckWithoutBarrier),
        );
        #[cfg(not(test))]
        let (no_fence, no_barrier) = (false, false);
        // Only the first entry's outcome is its own; the rest were never sent.
        let first = |a: Advanced| crate::outcomes(batch, 1, move |_| a.clone());
        if !no_fence
            && (!live(self.owner, c)
                || w.newest != Some(c.attempt)
                || w.entries.len() as u64 != c.end)
        {
            c.dead = true;
            return crate::outcomes(batch, 0, |_| unreachable!());
        }
        if w.fault("refuse") {
            return first(Advanced::Refused(AuthorityError::Unreachable));
        }
        let lost = w.fault("lost_response");
        if lost && w.draws.next().is_multiple_of(2) {
            // The request was lost: the provider reads the history, finds it
            // still ends at the writer's end, and reports no effect, unless it
            // cannot read.
            if w.fault("refuse") {
                return first(Advanced::Undecided(AuthorityError::Unreachable));
            }
            return first(Advanced::Refused(AuthorityError::Unreachable));
        }
        for q in batch {
            w.entries.push(q.record.clone());
            c.appended(q);
        }
        // Whatever is appended is durable at the weakest level.
        w.durable[0] = c.end;
        if lost && w.fault("refuse") {
            // The response was lost and the history cannot be read.
            return crate::outcomes(batch, batch.len(), |_| {
                Advanced::Undecided(AuthorityError::Unreachable)
            });
        }
        let top = batch.iter().map(|q| q.at).max().unwrap_or(Level(0));
        if top.0 > 0 && !no_barrier {
            if w.fault("barrier") {
                let e = AuthorityError::io(std::io::ErrorKind::Other);
                return crate::outcomes(batch, batch.len(), |q| {
                    if q.at.0 > 0 {
                        Advanced::Undecided(e.clone())
                    } else {
                        Advanced::Advanced(q.position)
                    }
                });
            }
            w.barrier(top);
        }
        for d in &mut c.durable[..=top.0 as usize] {
            *d = c.end;
        }
        crate::outcomes(batch, batch.len(), |q| Advanced::Advanced(q.position))
    }

    fn durable_in(&self, w: &mut World, c: &mut Claim, at: Level) -> Result<u64, Error> {
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

    /// The faults a reader meets and the barrier it issues at `at`.
    fn barrier_in(&self, w: &mut World, at: Level) -> Result<(), Error> {
        #[cfg(test)]
        let no_barrier = w.bug == Some(Bug::ReadWithoutBarrier);
        #[cfg(not(test))]
        let no_barrier = false;
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
        Ok(())
    }

    fn last_in(&self, w: &mut World, at: Level) -> Result<Option<Locator>, Error> {
        self.barrier_in(w, at)?;
        Ok(w.follow(w.entries.len())?.appended())
    }

    fn at_in(
        &self,
        w: &mut World,
        l: &Locator,
        from: u64,
        at: Level,
        max: u64,
    ) -> Result<Vec<Record>, Error> {
        self.barrier_in(w, at)?;
        let span = crate::span(l, from, max);
        if span.is_empty() || w.follow(w.entries.len())?.locator(l.position) != Some(*l) {
            return Ok(Vec::new());
        }
        self.read_in(w, from, at, span.end() - from)
    }

    fn read_in(&self, w: &mut World, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        self.barrier_in(w, at)?;
        let end = w.entries.len();
        let start = (from as usize).min(end);
        let stop = start.saturating_add(max as usize).min(end);
        // A simulated history is followed from its start on every read: a
        // power failure may have cut it anywhere since the last.
        let mut chain = w.follow(start)?;
        (start..stop)
            .map(|i| {
                chain
                    .follow(&w.entries[i])
                    .map_err(|b| crate::bad(b, i as u64 + 1))
            })
            .collect()
    }

    fn get_in(&self, w: &mut World, name: &str) -> Result<Option<Vec<u8>>, Error> {
        if w.fault("refuse") {
            return Err(Error::Refused(AuthorityError::Unreachable));
        }
        Ok(w.objects.get(name).cloned())
    }

    fn put_in(&self, w: &mut World, name: &str, data: Vec<u8>) -> Result<(), Error> {
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

    /// One reclaim call, served whole inside the one request it counts as:
    /// its barrier, reads, listings and deletes draw no faults of their own.
    fn reclaim_in(&self, w: &mut World, c: &mut Claim, budget: u64) -> Result<Reclaimed, Error> {
        let top = Level(self.levels.len() as u8 - 1);
        let s = self.durable_in(w, c, top)?;
        reclaim::run(
            &mut InWorld {
                w,
                owner: self.owner,
            },
            c,
            s,
            budget,
        )
    }
}

/// The simulated log's objects and history as reclamation sees them.
struct InWorld<'a> {
    w: &'a mut World,
    owner: u64,
}

/// Object names one listing page holds.
const PAGE: usize = 1000;

impl Space for InWorld<'_> {
    fn page(&mut self, after: &str, _s: u64) -> Result<Page, Error> {
        let mut names = self
            .w
            .objects
            .keys()
            .filter(|k| k.as_str() > after)
            .cloned();
        let keys: Vec<(String, Found)> = names
            .by_ref()
            .take(PAGE)
            .map(|n| {
                let found = ObjectRef::parse(&n).map_or(Found::Keep, Found::Object);
                (n, found)
            })
            .collect();
        let more = names.next().is_some();
        Ok(Page { keys, more })
    }

    fn remove(&mut self, key: &str) -> Result<(), Error> {
        self.w.objects.remove(key);
        Ok(())
    }

    fn key(&self, r: &ObjectRef) -> String {
        r.name()
    }

    fn records(&mut self, from: u64, max: u64) -> Result<Vec<Record>, Error> {
        let end = *self.w.durable.last().unwrap() as usize;
        let start = (from as usize).min(end);
        let stop = start.saturating_add(max as usize).min(end);
        let mut chain = self.w.follow(start)?;
        (start..stop)
            .map(|i| {
                chain
                    .follow(&self.w.entries[i])
                    .map_err(|b| crate::bad(b, i as u64 + 1))
            })
            .collect()
    }

    fn publish(&mut self, c: &mut Claim, sweep: &Sweep) -> Result<(), Error> {
        if !live(self.owner, c) || self.w.newest != Some(c.attempt) || self.w.sweep.0 != c.revision
        {
            c.dead = true;
            return Err(Error::Fenced);
        }
        c.revision += 1;
        self.w.sweep = (c.revision, sweep.clone());
        Ok(())
    }
}

/// What a request whose answer was lost, or cut short, is told: the history
/// cannot be read.
fn unreadable() -> AuthorityError {
    AuthorityError::Unreachable
}

impl Authority for Sim {
    fn levels(&self) -> &[String] {
        &self.levels
    }
    fn floor(&self) -> Level {
        self.floor
    }
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let r = if cut.fail {
            Err(Error::Refused(unreadable()))
        } else if cut.tear {
            w.power_fail();
            Err(Error::Storage(unreadable()))
        } else {
            match self.claim_in(&mut w, attempt, fresh) {
                // The claim entry is in the history; its claimant never learns it.
                Ok(_) if cut.lose => Err(Error::Storage(unreadable())),
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
    }

    fn append(&mut self, c: &mut Claim, batch: &[Queued]) -> Vec<Advanced> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let all = |a: Advanced| crate::outcomes(batch, batch.len(), move |_| a.clone());
        let r = if cut.fail {
            crate::outcomes(batch, 1, |_| Advanced::Refused(unreadable()))
        } else if cut.tear {
            w.power_fail();
            all(Advanced::Undecided(unreadable()))
        } else {
            match self.append_in(&mut w, c, batch) {
                r if cut.lose && r.iter().all(|a| matches!(a, Advanced::Advanced(_))) => {
                    all(Advanced::Undecided(unreadable()))
                }
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
    }

    fn durable(&mut self, c: &mut Claim, at: Level) -> Result<u64, Error> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let r = if cut.fail {
            Err(Error::Refused(unreadable()))
        } else if cut.tear {
            w.power_fail();
            c.dead = true;
            Err(Error::Storage(unreadable()))
        } else {
            match self.durable_in(&mut w, c, at) {
                Ok(_) if cut.lose => {
                    c.dead = true;
                    Err(Error::Storage(unreadable()))
                }
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
    }

    fn read(&mut self, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        self.reading(|s, w| s.read_in(w, from, at, max))
    }

    fn followed_locator(&mut self, position: u64) -> Result<Locator, Error> {
        let w = self.world.lock().unwrap();
        w.follow(w.entries.len())?
            .locator(position)
            .ok_or_else(crate::invalid)
    }

    fn last(&mut self, at: Level) -> Result<Option<Locator>, Error> {
        self.reading(|s, w| s.last_in(w, at))
    }

    fn at(&mut self, l: &Locator, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        self.reading(|s, w| s.at_in(w, l, from, at, max))
    }

    fn get_object(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let r = if cut.fail {
            Err(Error::Refused(unreadable()))
        } else if cut.tear {
            w.power_fail();
            Err(Error::Storage(unreadable()))
        } else {
            match self.get_in(&mut w, name) {
                Ok(_) if cut.lose => Err(Error::Storage(unreadable())),
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
    }

    fn put_object(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let r = if cut.fail {
            Err(Error::Refused(unreadable()))
        } else if cut.tear {
            // Objects are put whole or not at all: a cut-short put made none.
            w.power_fail();
            Err(Error::Storage(unreadable()))
        } else {
            match self.put_in(&mut w, name, data) {
                Ok(()) if cut.lose => Err(Error::Storage(unreadable())),
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
    }

    fn reclaim(&mut self, c: &mut Claim, budget: u64) -> Result<Reclaimed, Error> {
        let mut w = self.world.lock().unwrap();
        let cut = w.cut();
        let r = if cut.fail {
            Err(Error::Refused(unreadable()))
        } else if cut.tear {
            w.power_fail();
            c.dead = true;
            Err(Error::Storage(unreadable()))
        } else {
            match self.reclaim_in(&mut w, c, budget) {
                Ok(_) if cut.lose => {
                    c.dead = true;
                    Err(Error::Storage(unreadable()))
                }
                r => r,
            }
        };
        if cut.crash {
            w.power_fail();
        }
        r
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

/// `authority.objects@1` over `objects.sim@1`: the objects authority as a
/// client of simulated objects whose crash cut restarts it. The restarted
/// client keeps nothing of the old one, neither the history it followed nor
/// its head's tag, and the old client's claims are dead, as a new process's
/// would be.
pub struct Client {
    bucket: Arc<hirsel_object_storage::MemoryObjects>,
    level: String,
    floor: Level,
    client: crate::objects::Objects,
}

impl Client {
    pub fn new(bucket: hirsel_object_storage::MemoryObjects, level: String, floor: Level) -> Self {
        let bucket = Arc::new(bucket);
        // The claim `claim_at` runs: another client of the same objects claims
        // the history, as a process starting elsewhere would.
        let weak = Arc::downgrade(&bucket);
        let (foreign_level, foreign_floor) = (level.clone(), floor);
        bucket.on_claim(Arc::new(move || {
            if let Some(bucket) = weak.upgrade() {
                let mut other = crate::objects::Objects::new(
                    Arc::new(bucket.client()),
                    foreign_level.clone(),
                    foreign_floor,
                );
                let _ = other.claim([0xc1; 16], [0xc2; 32]);
            }
        }));
        let client = crate::objects::Objects::new(bucket.clone(), level.clone(), floor);
        Client {
            bucket,
            level,
            floor,
            client,
        }
    }

    /// The client serving a request: a new one after a crash.
    fn serving(&mut self) -> &mut crate::objects::Objects {
        if self.bucket.restart() {
            self.client =
                crate::objects::Objects::new(self.bucket.clone(), self.level.clone(), self.floor);
        }
        &mut self.client
    }
}

impl Authority for Client {
    fn levels(&self) -> &[String] {
        self.client.levels()
    }
    fn floor(&self) -> Level {
        self.floor
    }
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        self.serving().claim(attempt, fresh)
    }
    fn append(&mut self, c: &mut Claim, batch: &[Queued]) -> Vec<Advanced> {
        self.serving().append(c, batch)
    }
    fn durable(&mut self, c: &mut Claim, at: Level) -> Result<u64, Error> {
        self.serving().durable(c, at)
    }
    fn read(&mut self, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        self.serving().read(from, at, max)
    }
    fn followed_locator(&mut self, position: u64) -> Result<Locator, Error> {
        self.serving().followed_locator(position)
    }
    fn locate(&mut self, c: &Claim, position: u64) -> Result<Locator, Error> {
        self.serving().locate(c, position)
    }
    fn last(&mut self, at: Level) -> Result<Option<Locator>, Error> {
        self.serving().last(at)
    }
    fn at(&mut self, l: &Locator, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        self.serving().at(l, from, at, max)
    }
    fn get_object(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        self.serving().get_object(name)
    }
    fn put_object(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
        self.serving().put_object(name, data)
    }
    fn reclaim(&mut self, c: &mut Claim, budget: u64) -> Result<Reclaimed, Error> {
        self.serving().reclaim(c, budget)
    }
}

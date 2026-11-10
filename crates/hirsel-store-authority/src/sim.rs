//! `authority.sim@1`: a simulated authority, the one fault path. It keeps a
//! log representation by the log's rules and injects, by seed, what a store
//! must survive by detecting and refusing: the loss of anything not yet
//! durable at a level, torn and garbage tails, refused requests, lost
//! responses and failed barriers. Handles made by [`Sim::share`] race on one
//! history.
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
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The faults the simulated authority injects, each with a `fail_one_in` count.
pub const FAULTS: [&str; 5] = ["barrier", "garbage", "lost_response", "refuse", "torn"];

#[derive(Clone, Debug)]
pub struct Config {
    pub seed: u64,
    pub levels: Vec<String>,
    /// `fail_one_in` by fault name, 0 for never.
    pub faults: BTreeMap<String, u64>,
}

impl Config {
    pub fn from_grant(g: &AuthorityGrant) -> Result<Self, ConfigError> {
        let fail = |m: String| ConfigError(m);
        match g.represent.as_deref() {
            Some("log") => {}
            Some("objects") => {
                return Err(fail(
                    "sim `represent: objects` arrives with authority.objects@1".into(),
                ));
            }
            _ => return Err(fail("sim `represent` is \"log\" or \"objects\"".into())),
        }
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
        let faults = g.faults.clone().unwrap_or_default();
        if let Some(k) = faults.keys().find(|k| !FAULTS.contains(&k.as_str())) {
            return Err(fail(format!("unknown sim fault `{k}`")));
        }
        Ok(Config {
            seed: g.seed.unwrap_or(0),
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

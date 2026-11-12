//! `authority.memory@1`: the history in host memory. Its one level, `memory`,
//! survives no failure. Handles made by [`Memory::share`] see one history.

use crate::envelope::{self, Chain};
use crate::reclaim::{self, Found, Page, Space, Sweep};
use crate::{
    Advanced, Authority, Claim, Claimed, Error, Level, Locator, ObjectRef, Queued, Reclaimed,
    Record, live, owner_id,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub fn levels() -> Vec<String> {
    vec!["memory".to_string()]
}

#[derive(Default)]
struct World {
    records: Vec<Vec<u8>>,
    chain: Chain,
    newest: Option<[u8; 16]>,
    objects: BTreeMap<String, Vec<u8>>,
    /// Reclamation's continuation and its revision.
    sweep: (u64, Sweep),
}

pub struct Memory {
    world: Arc<Mutex<World>>,
    levels: Vec<String>,
    floor: Level,
    owner: u64,
}

impl Memory {
    pub fn new(floor: Level) -> Self {
        Memory {
            world: Arc::default(),
            levels: levels(),
            floor,
            owner: owner_id(),
        }
    }

    /// Another handle on the same history.
    pub fn share(&self) -> Self {
        Memory {
            world: self.world.clone(),
            levels: self.levels.clone(),
            floor: self.floor,
            owner: owner_id(),
        }
    }
}

impl Authority for Memory {
    fn levels(&self) -> &[String] {
        &self.levels
    }
    fn floor(&self) -> Level {
        self.floor
    }
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        let mut w = self.world.lock().unwrap();
        let w = &mut *w;
        let record = envelope::claimed(&w.chain.tip, attempt, fresh, w.chain.appended());
        w.chain
            .follow(&record)
            .expect("a claim record follows its tip");
        w.records.push(record);
        w.newest = Some(attempt);
        let mut claim = Claim::new(self.owner, attempt, &w.chain.tip, 1);
        (claim.revision, claim.sweep) = w.sweep.clone();
        let end = claim.end;
        Ok(Claimed { claim, end })
    }

    fn append(&mut self, c: &mut Claim, batch: &[Queued]) -> Vec<Advanced> {
        let mut w = self.world.lock().unwrap();
        let w = &mut *w;
        if !live(self.owner, c) || w.newest != Some(c.attempt) || w.chain.end() != c.end {
            c.dead = true;
            return crate::outcomes(batch, 0, |_| unreachable!());
        }
        for q in batch {
            w.chain
                .follow(&q.record)
                .expect("an append follows its claim");
            w.records.push(q.record.clone());
            c.appended(q);
        }
        c.durable[0] = c.end;
        crate::outcomes(batch, batch.len(), |q| Advanced::Advanced(q.position))
    }

    fn durable(&mut self, c: &mut Claim, _at: Level) -> Result<u64, Error> {
        let w = self.world.lock().unwrap();
        if !live(self.owner, c) || w.newest != Some(c.attempt) {
            c.dead = true;
            return Err(Error::Fenced);
        }
        Ok(c.durable[0])
    }

    fn read(&mut self, from: u64, _at: Level, max: u64) -> Result<Vec<Record>, Error> {
        let w = self.world.lock().unwrap();
        let start = (from as usize).min(w.records.len());
        let end = start.saturating_add(max as usize).min(w.records.len());
        (start..end)
            .map(|i| {
                let p = i as u64 + 1;
                w.chain
                    .check(p, &w.records[i])
                    .map_err(|b| crate::bad(b, p))
            })
            .collect()
    }

    fn followed_locator(&mut self, position: u64) -> Result<Locator, Error> {
        let w = self.world.lock().unwrap();
        w.chain.locator(position).ok_or_else(crate::invalid)
    }

    fn last(&mut self, _at: Level) -> Result<Option<Locator>, Error> {
        Ok(self.world.lock().unwrap().chain.appended())
    }

    fn at(&mut self, l: &Locator, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error> {
        let span = crate::span(l, from, max);
        if self.world.lock().unwrap().chain.locator(l.position) != Some(*l) || span.is_empty() {
            return Ok(Vec::new());
        }
        self.read(from, at, span.end() - from)
    }

    fn get_object(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.world.lock().unwrap().objects.get(name).cloned())
    }

    fn put_object(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
        let mut w = self.world.lock().unwrap();
        match w.objects.get(name) {
            Some(old) if *old == data => Ok(()),
            Some(_) => Err(Error::Corrupt(name.to_string())),
            None => {
                w.objects.insert(name.to_string(), data);
                Ok(())
            }
        }
    }

    fn reclaim(&mut self, c: &mut Claim, budget: u64) -> Result<Reclaimed, Error> {
        let s = self.durable(c, Level(0))?;
        reclaim::run(self, c, s, budget)
    }
}

/// Object names one listing page holds.
const PAGE: usize = 1000;

impl Space for Memory {
    fn page(&mut self, after: &str, _s: u64) -> Result<Page, Error> {
        let w = self.world.lock().unwrap();
        let mut names = w
            .objects
            .range::<str, _>((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
            .map(|(n, _)| n.clone());
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
        self.world.lock().unwrap().objects.remove(key);
        Ok(())
    }

    fn key(&self, r: &ObjectRef) -> String {
        r.name()
    }

    fn records(&mut self, from: u64, max: u64) -> Result<Vec<Record>, Error> {
        self.read(from, Level(0), max)
    }

    fn publish(&mut self, c: &mut Claim, sweep: &Sweep) -> Result<(), Error> {
        let mut w = self.world.lock().unwrap();
        if !live(self.owner, c) || w.newest != Some(c.attempt) || w.sweep.0 != c.revision {
            c.dead = true;
            return Err(Error::Fenced);
        }
        c.revision += 1;
        w.sweep = (c.revision, sweep.clone());
        Ok(())
    }
}

//! `authority.memory@1`: the history in host memory. Its one level, `memory`,
//! survives no failure. Handles made by [`Memory::share`] see one history.

use crate::envelope::{self, Chain};
use crate::{Advanced, Authority, Claim, Claimed, Error, Level, Queued, Record, live, owner_id};
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
        let record = envelope::claimed(&w.chain.tip, attempt, fresh);
        w.chain
            .follow(&record)
            .expect("a claim record follows its tip");
        w.records.push(record);
        w.newest = Some(attempt);
        let claim = Claim::new(self.owner, attempt, &w.chain.tip, 1);
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

    fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.world.lock().unwrap().objects.get(name).cloned())
    }

    fn put(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error> {
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

    fn reclaim(&mut self, c: &mut Claim) -> Result<(), Error> {
        self.durable(c, Level(0)).map(|_| ())
    }
}

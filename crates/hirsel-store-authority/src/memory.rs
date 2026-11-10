//! `authority.memory@1`: the history in host memory. Its one level, `memory`,
//! survives no failure. Handles made by [`Memory::share`] see one history.

use crate::group::{self, Tip};
use crate::{Advanced, Authority, Claim, Claimed, Error, Level, MAX_ENTRY, live, owner_id};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub fn levels() -> Vec<String> {
    vec!["memory".to_string()]
}

#[derive(Default)]
struct World {
    entries: Vec<Vec<u8>>,
    /// The tip after the first `scanned` entries.
    scanned: usize,
    tip: Option<Tip>,
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
    fn max_entry(&self) -> u64 {
        MAX_ENTRY
    }

    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error> {
        let mut w = self.world.lock().unwrap();
        let mut tip = w.tip.clone().unwrap_or_else(Tip::origin);
        for i in w.scanned..w.entries.len() {
            tip = group::follow(&tip, &w.entries[i]).map_err(|b| crate::bad(b, i as u64 + 1))?;
        }
        let entry = group::claim_group(&tip, attempt, fresh);
        tip = group::follow(&tip, &entry).expect("a claim group follows its tip");
        w.entries.push(entry);
        w.scanned = w.entries.len();
        w.tip = Some(tip);
        w.newest = Some(attempt);
        let end = w.entries.len() as u64;
        let claim = Claim {
            owner: self.owner,
            attempt,
            end,
            durable: vec![end],
            segment: 0,
            length: 0,
            tag: None,
            dead: false,
        };
        Ok(Claimed { claim, end })
    }

    fn advance(&mut self, c: &mut Claim, from: u64, entry: Vec<u8>, _at: Level) -> Advanced {
        let mut w = self.world.lock().unwrap();
        if !live(self.owner, c)
            || w.newest != Some(c.attempt)
            || from != c.end
            || w.entries.len() as u64 != from
        {
            c.dead = true;
            return Advanced::Fenced;
        }
        if entry.len() as u64 > MAX_ENTRY {
            return Advanced::Refused(crate::AuthorityError::io(std::io::ErrorKind::InvalidInput));
        }
        w.entries.push(entry);
        c.end = from + 1;
        c.durable[0] = c.end;
        Advanced::Advanced(c.end)
    }

    fn durable(&mut self, c: &mut Claim, _at: Level) -> Result<u64, Error> {
        let w = self.world.lock().unwrap();
        if !live(self.owner, c) || w.newest != Some(c.attempt) {
            c.dead = true;
            return Err(Error::Fenced);
        }
        Ok(c.durable[0])
    }

    fn read(&mut self, from: u64, _at: Level, max: u64) -> Result<Vec<Vec<u8>>, Error> {
        let w = self.world.lock().unwrap();
        let start = (from as usize).min(w.entries.len());
        let end = start.saturating_add(max as usize).min(w.entries.len());
        Ok(w.entries[start..end].to_vec())
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

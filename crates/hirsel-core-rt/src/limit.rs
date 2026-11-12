//! Union by rank, full path compression, and reference-counted record slots.
//! Reservations hold their parent record; union edges hold their representative.
//! Ending a limit allocates nothing and never walks values (MEM-20).

use crate::Capacity;
use std::sync::{Arc, Mutex};

struct Live {
    capacity: u64,
    used: u64,
    parent: Option<usize>,
}
struct Record {
    refs: usize,
    parent: Option<usize>,
    rank: u32,
    live: Option<Live>,
}
enum Slot {
    Record(Record),
    Free(Option<usize>),
}
struct Forest {
    slots: Vec<Slot>,
    free: Option<usize>,
    #[cfg(test)]
    pub(crate) attempts: usize,
    #[cfg(test)]
    pub(crate) fail: Option<(usize, bool)>,
}

impl Forest {
    fn record(&self, id: usize) -> &Record {
        match &self.slots[id] {
            Slot::Record(r) => r,
            Slot::Free(_) => unreachable!(),
        }
    }
    fn record_mut(&mut self, id: usize) -> &mut Record {
        match &mut self.slots[id] {
            Slot::Record(r) => r,
            Slot::Free(_) => unreachable!(),
        }
    }
    fn retain(&mut self, id: usize) {
        self.record_mut(id).refs += 1;
    }
    fn release(&mut self, mut id: usize) {
        loop {
            let r = self.record_mut(id);
            r.refs -= 1;
            if r.refs != 0 {
                break;
            }
            let parent = r.parent.or_else(|| r.live.as_ref().and_then(|l| l.parent));
            self.slots[id] = Slot::Free(self.free);
            self.free = Some(id);
            match parent {
                Some(p) => id = p,
                None => break,
            }
        }
    }
    fn insert(&mut self, live: Live) -> usize {
        let record = Slot::Record(Record {
            refs: 1,
            parent: None,
            rank: 0,
            live: Some(live),
        });
        if let Some(id) = self.free {
            let Slot::Free(next) = self.slots[id] else {
                unreachable!()
            };
            self.free = next;
            self.slots[id] = record;
            id
        } else {
            self.slots.push(record);
            self.slots.len() - 1
        }
    }
    fn find(&mut self, id: usize) -> usize {
        let mut root = id;
        while let Some(p) = self.record(root).parent {
            root = p;
        }
        let mut node = id;
        while let Some(p) = self.record(node).parent {
            if p == root {
                break;
            }
            self.retain(root);
            self.record_mut(node).parent = Some(root);
            // Hold p while releasing its former incoming edge: compression may
            // otherwise reclaim the next node before we follow it.
            self.retain(p);
            self.release(p);
            if node != id {
                self.release(node);
            }
            node = p;
        }
        if node != id {
            self.release(node);
        }
        root
    }
    fn admit(&mut self, id: usize, bytes: Option<u64>) -> Result<(), Capacity> {
        let root = self.find(id);
        #[cfg(test)]
        let injected = {
            self.attempts += 1;
            self.fail
                .is_some_and(|(n, later)| self.attempts == n || later && self.attempts >= n)
        };
        #[cfg(not(test))]
        let injected = false;
        let live = self.record_mut(root).live.as_mut().unwrap();
        let available = live.capacity - live.used;
        match bytes {
            Some(n) if !injected && n <= available => {
                live.used += n;
                Ok(())
            }
            n => Err(Capacity {
                requested: n.unwrap_or(u64::MAX),
                available,
            }),
        }
    }
    fn refund(&mut self, id: usize, bytes: u64) {
        let root = self.find(id);
        self.record_mut(root).live.as_mut().unwrap().used -= bytes;
    }
}

/// A storage domain identity, retained by every allocation it paid for.
pub struct Funding {
    forest: Arc<Mutex<Forest>>,
    id: usize,
}

impl Funding {
    pub fn root(capacity: u64) -> Self {
        let mut forest = Forest {
            slots: Vec::new(),
            free: None,
            #[cfg(test)]
            attempts: 0,
            #[cfg(test)]
            fail: None,
        };
        let id = forest.insert(Live {
            capacity,
            used: 0,
            parent: None,
        });
        Self {
            forest: Arc::new(Mutex::new(forest)),
            id,
        }
    }
    pub fn available(&self) -> u64 {
        let mut forest = self.forest.lock().unwrap();
        let root = forest.find(self.id);
        let l = forest.record(root).live.as_ref().unwrap();
        l.capacity - l.used
    }
    pub(crate) fn admit(&self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.forest.lock().unwrap().admit(self.id, bytes)
    }
    pub(crate) fn refund(&self, bytes: u64) {
        self.forest.lock().unwrap().refund(self.id, bytes);
    }
    pub fn reserve(&self, bytes: u64) -> Result<Self, Capacity> {
        self.admit(Some(bytes))?;
        Ok(self.prepaid(bytes))
    }
    /// Subdivide an already admitted reservation, without another admission.
    pub(crate) fn prepaid(&self, bytes: u64) -> Self {
        let mut forest = self.forest.lock().unwrap();
        forest.retain(self.id);
        let id = forest.insert(Live {
            capacity: bytes,
            used: 0,
            parent: Some(self.id),
        });
        Self {
            forest: self.forest.clone(),
            id,
        }
    }
    pub fn end(&self) {
        let mut f = self.forest.lock().unwrap();
        let child = f.find(self.id);
        let live = f.record_mut(child).live.take().unwrap();
        let original_parent = live.parent.unwrap();
        let parent = f.find(original_parent);
        f.record_mut(parent).live.as_mut().unwrap().used -= live.capacity - live.used;
        let parent_live = f.record_mut(parent).live.take();
        let (winner, loser) = if f.record(child).rank > f.record(parent).rank {
            (child, parent)
        } else {
            if f.record(child).rank == f.record(parent).rank {
                f.record_mut(parent).rank += 1;
            }
            (parent, child)
        };
        f.record_mut(winner).live = parent_live;
        f.retain(winner);
        f.record_mut(loser).parent = Some(winner);
        f.release(original_parent);
    }
    #[cfg(test)]
    pub(crate) fn inject(&self, n: usize, later: bool) {
        let mut f = self.forest.lock().unwrap();
        f.attempts = 0;
        f.fail = Some((n, later));
    }
    #[cfg(test)]
    pub(crate) fn records(&self) -> usize {
        self.forest
            .lock()
            .unwrap()
            .slots
            .iter()
            .filter(|s| matches!(s, Slot::Record(_)))
            .count()
    }
}
impl Clone for Funding {
    fn clone(&self) -> Self {
        self.forest.lock().unwrap().retain(self.id);
        Self {
            forest: self.forest.clone(),
            id: self.id,
        }
    }
}
impl Drop for Funding {
    fn drop(&mut self) {
        self.forest.lock().unwrap().release(self.id);
    }
}

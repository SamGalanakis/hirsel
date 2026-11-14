//! core.map's cached byte hash, dense live/dead entries and u64 probe index.
//! Safe Rust keeps entries and index in separate buffers; Hirsel packs both in one block.
use crate::support::{String, Vec};
struct Entry<V> {
    hash: u64,
    live: u64,
    key: String,
    value: V,
}
pub struct Map<V> {
    entries: Vec<Entry<V>>,
    index: Vec<u64>,
    live: usize,
}
impl<V: Default> Map<V> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: Vec::new(),
            live: 0,
        }
    }
    fn locate(&self, key: &[u8], h: u64) -> (usize, Option<usize>) {
        let mask = (self.index.len() as u64).wrapping_sub(1);
        let mut j = h & mask;
        loop {
            let s = self.index[j as usize];
            if s == 0 {
                return (j as usize, None);
            }
            let i = (s - 1) as usize;
            let e = &self.entries[i];
            if e.hash == h && e.live != 0 && e.key.as_bytes() == key {
                return (j as usize, Some(i));
            }
            j = j.wrapping_add(1) & mask
        }
    }
    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&V> {
        if self.entries.capacity() == 0 {
            return None;
        }
        let h = hash(key.as_ref());
        self.locate(key.as_ref(), h)
            .1
            .map(|i| &self.entries[i].value)
    }
    fn rebuild(&mut self) {
        let mask = (self.index.len() as u64).wrapping_sub(1);
        let mut i = 0u64;
        while i < self.entries.len() as u64 {
            let mut j = self.entries[i as usize].hash & mask;
            while self.index[j as usize] != 0 {
                j = j.wrapping_add(1) & mask
            }
            let s = i.wrapping_add(1);
            self.index[j as usize] = s;
            i = s
        }
    }
    fn room(&mut self) {
        let cap = self.entries.capacity();
        if cap != 0 && self.live <= cap / 2 {
            self.entries.retain(|e| e.live != 0);
            self.index.fill(0)
        } else {
            let cap = if cap == 0 { 4 } else { cap * 2 };
            let mut entries = Vec::with_capacity(cap);
            for entry in std::mem::take(&mut self.entries) {
                entries.push(entry)
            }
            self.entries = entries;
            let last = (cap as u64).wrapping_mul(2) - 1;
            let n = (1u64 << (64 - u64::from(last.leading_zeros()))) as usize;
            let mut index = Vec::with_capacity(n);
            index.resize(n, 0);
            self.index = index
        }
        self.rebuild()
    }
    pub fn insert(&mut self, key: String, value: V) {
        let h = hash(key.as_bytes());
        let mut j = 0;
        if !self.index.is_empty() {
            let (slot, found) = self.locate(key.as_bytes(), h);
            j = slot;
            if let Some(i) = found {
                self.entries[i].value = value;
                return;
            }
        }
        if self.entries.len() == self.entries.capacity() {
            self.room();
            let mask = (self.index.len() as u64).wrapping_sub(1);
            let mut k = h & mask;
            while self.index[k as usize] != 0 {
                k = k.wrapping_add(1) & mask
            }
            j = k as usize
        }
        let s = (self.entries.len() as u64).wrapping_add(1);
        self.entries.push(Entry {
            hash: h,
            live: 1,
            key,
            value,
        });
        self.live += 1;
        self.index[j] = s;
    }
    pub fn remove(&mut self, key: impl AsRef<[u8]>) -> Option<V> {
        if self.index.is_empty() {
            return None;
        }
        let h = hash(key.as_ref());
        let i = self.locate(key.as_ref(), h).1?;
        let e = &mut self.entries[i];
        e.live = 0;
        self.live -= 1;
        e.key = String::new();
        Some(std::mem::take(&mut e.value))
    }
}
fn hash(bytes: &[u8]) -> u64 {
    let mix = |h: u64, w: u64| ((h ^ w).wrapping_mul(0x9e3779b97f4a7c15)).rotate_left(26);
    let mut h = bytes.len() as u64;
    let (words, tail) = bytes.as_chunks::<8>();
    for &w in words {
        h = mix(h, u64::from_le_bytes(w))
    }
    if !tail.is_empty() {
        let mut w = [0; 8];
        w[..tail.len()].copy_from_slice(tail);
        h = mix(h, u64::from_le_bytes(w))
    }
    h ^ (h >> 32)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_compaction_and_static_keys() {
        let mut m = Map::new();
        m.insert(String::literal("/alpha"), 7u32);
        m.insert(String::literal("/beta"), 8);
        assert_eq!(m.entries[0].key.capacity(), 0);
        assert_eq!(m.get("/alpha"), Some(&7));
        assert_eq!(m.remove("/alpha"), Some(7));
        assert_eq!(m.get("/alpha"), None);
        for _ in 0..20 {
            m.insert(String::literal("a"), 1);
            m.insert(String::literal("b"), 2);
            m.remove("a");
            m.remove("b");
            assert_eq!(m.get("/beta"), Some(&8));
            assert!(m.entries.capacity() <= 4)
        }
    }
}

mod program;
#[path = "../support.rs"]
mod support;
use std::collections::HashMap;
const NONE: u32 = u32::MAX;
struct Slot {
    key: [u8; 32],
    key_len: u32,
    value: [u8; 128],
    len: u32,
    prev: u32,
    next: u32,
    generation: u64,
}
struct Cache {
    slots: Vec<Slot>,
    index: HashMap<String, u32>,
    head: u32,
    tail: u32,
    first: Option<(u32, u64)>,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            slots: Vec::with_capacity(64),
            index: HashMap::new(),
            head: NONE,
            tail: NONE,
            first: None,
        }
    }
}
impl Cache {
    fn touch(&mut self, i: u32) {
        if self.head == i {
            return;
        }
        let (p, n) = (self.slots[i as usize].prev, self.slots[i as usize].next);
        if p != NONE {
            self.slots[p as usize].next = n;
        }
        if n != NONE {
            self.slots[n as usize].prev = p;
        }
        if self.tail == i {
            self.tail = p;
        }
        self.slots[i as usize].prev = NONE;
        self.slots[i as usize].next = self.head;
        if self.head != NONE {
            self.slots[self.head as usize].prev = i;
        }
        self.head = i;
        if self.tail == NONE {
            self.tail = i;
        }
    }
    fn key(&self, i: u32) -> String {
        let s = &self.slots[i as usize];
        String::from_utf8(s.key[..s.key_len as usize].into()).unwrap()
    }
}
impl program::Cache for Cache {
    fn put(&mut self, key: &str, value: &str) -> bool {
        assert!(key.len() <= 32 && value.len() <= 128);
        if let Some(&i) = self.index.get(key) {
            let s = &mut self.slots[i as usize];
            s.value[..value.len()].copy_from_slice(value.as_bytes());
            s.len = value.len() as u32;
            self.touch(i);
            return false;
        }
        let evicted = self.slots.len() == 64;
        let i = if evicted {
            let i = self.tail;
            self.index.remove(&self.key(i));
            self.slots[i as usize].generation += 1;
            i
        } else {
            let i = self.slots.len() as u32;
            self.slots.push(Slot {
                key: [0; 32],
                key_len: 0,
                value: [0; 128],
                len: 0,
                prev: NONE,
                next: NONE,
                generation: 0,
            });
            i
        };
        let s = &mut self.slots[i as usize];
        s.key[..key.len()].copy_from_slice(key.as_bytes());
        s.key_len = key.len() as u32;
        s.value[..value.len()].copy_from_slice(value.as_bytes());
        s.len = value.len() as u32;
        self.index.insert(key.into(), i);
        self.touch(i);
        if self.first.is_none() {
            self.first = Some((i, self.slots[i as usize].generation));
        }
        evicted
    }
    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        let i = *self.index.get(key)?;
        self.touch(i);
        let s = &self.slots[i as usize];
        Some(s.value[..s.len as usize].into())
    }
    fn order(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut p = self.head;
        while p != NONE {
            out.push(self.key(p));
            p = self.slots[p as usize].next;
        }
        out
    }
    fn first_is_stale(&self) -> bool {
        self.first
            .is_some_and(|(i, g)| self.slots[i as usize].generation != g)
    }
}
fn main() {
    program::run::<Cache>();
}

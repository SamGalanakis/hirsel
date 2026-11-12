mod program;
#[path = "../support.rs"]
mod support;
use std::collections::HashMap;
struct Entry {
    key: String,
    value: Vec<u8>,
    prev: Option<usize>,
    next: Option<usize>,
}
#[derive(Default)]
struct Cache {
    slots: Vec<Entry>,
    index: HashMap<String, usize>,
    head: Option<usize>,
    tail: Option<usize>,
    generation: Vec<u64>,
    first: Option<(usize, u64)>,
}
impl Cache {
    fn touch(&mut self, i: usize) {
        if self.head == Some(i) {
            return;
        }
        let (prev, next) = (self.slots[i].prev, self.slots[i].next);
        if let Some(p) = prev {
            self.slots[p].next = next;
        }
        if let Some(n) = next {
            self.slots[n].prev = prev;
        }
        if self.tail == Some(i) {
            self.tail = prev;
        }
        self.slots[i].prev = None;
        self.slots[i].next = self.head;
        if let Some(h) = self.head {
            self.slots[h].prev = Some(i);
        }
        self.head = Some(i);
        if self.tail.is_none() {
            self.tail = Some(i);
        }
    }
}
impl program::Cache for Cache {
    fn put(&mut self, key: &str, value: &str) -> bool {
        if let Some(&i) = self.index.get(key) {
            self.slots[i].value = value.as_bytes().into();
            self.touch(i);
            return false;
        }
        let evicted = self.slots.len() == 64;
        let i = if evicted {
            let i = self.tail.unwrap();
            self.index.remove(&self.slots[i].key);
            self.generation[i] += 1;
            i
        } else {
            let i = self.slots.len();
            self.slots.push(Entry {
                key: String::new(),
                value: Vec::new(),
                prev: None,
                next: None,
            });
            self.generation.push(0);
            i
        };
        self.slots[i].key = key.into();
        self.slots[i].value = value.as_bytes().into();
        self.index.insert(key.into(), i);
        self.touch(i);
        if self.first.is_none() {
            self.first = Some((i, self.generation[i]));
        }
        evicted
    }
    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        let i = *self.index.get(key)?;
        self.touch(i);
        Some(self.slots[i].value.clone())
    }
    fn order(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut p = self.head;
        while let Some(i) = p {
            out.push(self.slots[i].key.clone());
            p = self.slots[i].next;
        }
        out
    }
    fn first_is_stale(&self) -> bool {
        self.first.is_some_and(|(i, g)| self.generation[i] != g)
    }
}
fn main() {
    program::run::<Cache>();
}

// Reviewed-by: langs-numeric-920 2026-11-13; same owned keys/values, indexed nodes, LIFO reuse, LRU links, warmup/reset, and checked counters; overflow checks required.
// Matched twin of cache_simulation.hirsel. Reviewed below.
// Source: kostya/LangArena 3c9f84d benchmarks/cache_simulation.rs:5-207.
// Identical indexed nodes, LIFO free list, owned keys and values, explicit LCG.
mod text_support;
use std::collections::HashMap;

struct Node {
    key: String,
    value: String,
    prev: Option<usize>,
    next: Option<usize>,
}

impl Node {
    fn new(key: String, value: String) -> Self {
        Node {
            key,
            value,
            prev: None,
            next: None,
        }
    }
}

struct LruCache {
    capacity: usize,
    cache: HashMap<String, usize>,
    nodes: Vec<Node>,
    free: Vec<usize>,
    head: Option<usize>,
    tail: Option<usize>,
    size: usize,
}

impl LruCache {
    fn new(capacity: usize) -> Self {
        LruCache {
            capacity,
            cache: HashMap::new(),
            nodes: Vec::new(),
            free: Vec::new(),
            head: None,
            tail: None,
            size: 0,
        }
    }

    fn get(&mut self, key: &str) -> Option<&str> {
        let node = *self.cache.get(key)?;
        self.move_to_front(node);
        Some(&self.nodes[node].value)
    }

    fn put(&mut self, key: String, value: String) {
        if let Some(&node) = self.cache.get(&key) {
            self.nodes[node].value = value;
            self.move_to_front(node);
            return;
        }

        if self.size >= self.capacity {
            self.remove_oldest();
        }

        let node = match self.free.pop() {
            Some(slot) => {
                self.nodes[slot] = Node::new(key.clone(), value);
                slot
            }
            None => {
                self.nodes.push(Node::new(key.clone(), value));
                self.nodes.len() - 1
            }
        };

        self.cache.insert(key, node);
        self.add_to_front(node);
        self.size += 1;
    }

    fn move_to_front(&mut self, node: usize) {
        if Some(node) == self.head {
            return;
        }

        let prev = self.nodes[node].prev;
        let next = self.nodes[node].next;

        if let Some(p) = prev {
            self.nodes[p].next = next;
        }
        if let Some(n) = next {
            self.nodes[n].prev = prev;
        }

        if Some(node) == self.tail {
            self.tail = prev;
        }

        self.nodes[node].prev = None;
        self.nodes[node].next = self.head;

        if let Some(h) = self.head {
            self.nodes[h].prev = Some(node);
        }

        self.head = Some(node);

        if self.tail.is_none() {
            self.tail = Some(node);
        }
    }

    fn add_to_front(&mut self, node: usize) {
        self.nodes[node].next = self.head;

        if let Some(h) = self.head {
            self.nodes[h].prev = Some(node);
        }

        self.head = Some(node);

        if self.tail.is_none() {
            self.tail = Some(node);
        }
    }

    fn remove_oldest(&mut self) {
        if let Some(tail_node) = self.tail {
            self.cache.remove(&self.nodes[tail_node].key);

            let prev = self.nodes[tail_node].prev;
            if let Some(p) = prev {
                self.nodes[p].next = None;
            }

            self.tail = prev;

            if Some(tail_node) == self.head {
                self.head = None;
            }

            self.free.push(tail_node);
            self.size -= 1;
        }
    }
}

fn step(c: &mut LruCache, g: &mut i32, i: u64, values: i32, hits: &mut u32, misses: &mut u32) {
    for _ in 0..1000 {
        *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
        let k = (*g as f64 / 139968.0 * f64::from(values)) as i32;
        let key = format!("item_{k}");
        let value = if c.get(&key).is_some() {
            *hits = hits.checked_add(1).unwrap();
            format!("updated_{i}")
        } else {
            *misses = misses.checked_add(1).unwrap();
            format!("new_{i}")
        };
        c.put(key, value);
    }
}
fn main() {
    let a: Vec<String> = text_support::arguments(&["values", "size"], false);
    let values: i32 = a[1].parse().unwrap();
    let size: usize = a[2].parse().unwrap();
    let warmup: u64 = a[3].parse().unwrap();
    let iterations: u64 = a[4].parse().unwrap();
    assert!(values > 0);
    let mut c = LruCache::new(size);
    let mut g = 42;
    let mut hits = 0;
    let mut misses = 0;
    for i in 0..warmup {
        step(&mut c, &mut g, i, values, &mut hits, &mut misses);
    }
    g = 42;
    for i in 0..iterations {
        step(&mut c, &mut g, i, values, &mut hits, &mut misses);
    }
    let a = (5432u32 << 5).wrapping_add(hits);
    let b = (a << 5).wrapping_add(misses);
    text_support::output((b << 5).wrapping_add(u32::try_from(c.size).unwrap()));
}

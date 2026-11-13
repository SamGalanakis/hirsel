use super::super::{helper, Benchmark};
use crate::config_i64;
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
            cache: HashMap::with_capacity(capacity),
            nodes: Vec::with_capacity(capacity),
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

    fn len(&self) -> usize {
        self.size
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

pub struct CacheSimulation {
    result_val: u32,
    values_size: i64,
    cache_size: i64,
    cache: LruCache,
    hits: u32,
    misses: u32,
}

impl CacheSimulation {
    pub fn new() -> Self {
        let values_size = config_i64("Etc::CacheSimulation", "values");
        let cache_size = config_i64("Etc::CacheSimulation", "size");

        Self {
            result_val: 5432,
            values_size,
            cache_size,
            cache: LruCache::new(cache_size as usize),
            hits: 0,
            misses: 0,
        }
    }
}

impl Benchmark for CacheSimulation {
    fn name(&self) -> String {
        "Etc::CacheSimulation".to_string()
    }

    fn prepare(&mut self) {
        self.cache = LruCache::new(self.cache_size as usize);
        self.hits = 0;
        self.misses = 0;
    }

    fn run(&mut self, iteration_id: i64) {
        for _ in 0..1000 {
            let key = format!("item_{}", helper::next_int(self.values_size as i32));
            if self.cache.get(&key).is_some() {
                self.hits += 1;
                self.cache.put(key, format!("updated_{}", iteration_id));
            } else {
                self.misses += 1;
                self.cache.put(key, format!("new_{}", iteration_id));
            }
        }
    }

    fn checksum(&self) -> u32 {
        let mut result = self.result_val;
        result = (result << 5).wrapping_add(self.hits);
        result = (result << 5).wrapping_add(self.misses);
        result = (result << 5).wrapping_add(self.cache.len() as u32);
        result
    }
}

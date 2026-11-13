// Safe Rust LAYOUT twins, same deterministic schedules as generate.py.
// Reviewed-by: core-collections-918, 2026-11-13. Valid-input document and all four graph phases match their Hirsel sources.
// Pool live counter fix rechecked; checked arithmetic flags, three Id checks and retained snapshot traversals match.
use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

enum Item {
    Flag(bool),
    Name(String),
    Scores(Vec<u64>),
}
fn label(i: u64) -> &'static str {
    &"abcdefghijklmnopqrstuvw"[..(i % 24) as usize]
}
fn ordinary(n: u64) -> u64 {
    let mut items = Vec::with_capacity(n as usize);
    for i in 0..n {
        items.push(match i % 3 {
            0 => Item::Flag(i % 2 == 0),
            1 => Item::Name(label(i).to_owned()),
            _ => Item::Scores(vec![i, i + 1, i + 2]),
        });
    }
    items
        .iter()
        .map(|x| match x {
            Item::Flag(b) => u64::from(*b),
            Item::Name(s) => s.len() as u64,
            Item::Scores(v) => v[0] + v[1] + v[2],
        })
        .sum()
}
fn encoded_size(n: u64) -> u64 {
    8 + (0..n)
        .map(|i| {
            4 + match i % 3 {
                0 => 2,
                1 => 5 + i % 24,
                _ => 29,
            }
        })
        .sum::<u64>()
}
fn read32(v: &[u8], at: usize) -> usize {
    u32::from_le_bytes(v[at..at + 4].try_into().unwrap()) as usize
}
fn packed(n: u64, streaming: bool) -> u64 {
    let reserve = if streaming {
        64 * n + 8
    } else {
        encoded_size(n)
    };
    let mut v = Vec::with_capacity(reserve as usize);
    v.extend_from_slice(&(n as u32).to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    for i in 0..n {
        v.push((i % 3) as u8);
        match i % 3 {
            0 => v.push(u8::from(i % 2 == 0)),
            1 => {
                let s = label(i);
                v.extend_from_slice(&(s.len() as u32).to_le_bytes());
                v.extend_from_slice(s.as_bytes());
            }
            _ => {
                // Match the explicit FixedSpan source scratch on the Hirsel side.
                let source = vec![i, i + 1, i + 2];
                v.extend_from_slice(&3u32.to_le_bytes());
                for x in source {
                    v.extend_from_slice(&x.to_le_bytes());
                }
            }
        }
    }
    let end = v.len();
    v[4..8].copy_from_slice(&((end - 8) as u32).to_le_bytes());
    let mut at = 8usize;
    for _ in 0..n {
        v.extend_from_slice(&u32::try_from(at - 8).unwrap().to_le_bytes());
        at += match v[at] {
            0 => 2,
            1 => 5 + read32(&v, at + 1),
            2 => 29,
            _ => unreachable!(),
        };
    }
    // Keep reservation and bytes stationary, as finish_trim does. Vec retains physical
    // capacity; its logical admitted prefix is v.len(), reported separately.
    let mut sum = 0u64;
    at = 8;
    for _ in 0..n {
        match v[at] {
            0 => {
                sum += u64::from(v[at + 1]);
                at += 2;
            }
            1 => {
                let count = read32(&v, at + 1);
                sum += count as u64;
                at += 5 + count;
            }
            2 => {
                for j in 0..3 {
                    sum +=
                        u64::from_le_bytes(v[at + 5 + j * 8..at + 13 + j * 8].try_into().unwrap());
                }
                at += 29;
            }
            _ => unreachable!(),
        }
    }
    sum
}
#[derive(Clone)]
struct Node {
    value: u64,
    edge: u32,
}
#[derive(Clone, Copy)]
struct Id {
    pool: u64,
    index: u64,
    generation: u64,
}
enum Slot {
    Occupied(u64, Node),
    Vacant(u64, u64),
}
struct Pool {
    me: u64,
    slots: Vec<Slot>,
    free: u64,
    live: u64,
}
impl Pool {
    fn new(n: usize) -> Self {
        static UNIQUE: AtomicU64 = AtomicU64::new(1);
        Self {
            me: UNIQUE.fetch_add(1, Ordering::Relaxed),
            slots: Vec::with_capacity(n),
            free: u64::MAX,
            live: 0,
        }
    }
    fn insert(&mut self, x: Node) -> Id {
        if self.free == u64::MAX {
            let index = self.slots.len() as u64;
            self.slots.push(Slot::Occupied(0, x));
            self.live += 1;
            return Id {
                pool: self.me,
                index,
                generation: 0,
            };
        }
        let index = self.free;
        let (generation, next) = match self.slots[index as usize] {
            Slot::Vacant(g, n) => (g, n),
            _ => unreachable!(),
        };
        self.slots[index as usize] = Slot::Occupied(generation, x);
        self.free = next;
        self.live += 1;
        Id {
            pool: self.me,
            index,
            generation,
        }
    }
    fn get(&self, h: Id) -> Option<&Node> {
        if h.pool != self.me {
            return None;
        }
        match self.slots.get(h.index as usize) {
            Some(Slot::Occupied(g, x)) if *g == h.generation => Some(x),
            _ => None,
        }
    }
    fn remove(&mut self, h: Id) -> Option<Node> {
        self.get(h)?;
        let generation = h.generation + 1;
        let slot = std::mem::replace(
            &mut self.slots[h.index as usize],
            Slot::Vacant(generation, self.free),
        );
        if generation != u64::MAX {
            self.free = h.index;
        }
        self.live -= 1;
        match slot {
            Slot::Occupied(_, x) => Some(x),
            _ => unreachable!(),
        }
    }
}
fn exclusive(n: u64, rounds: u64) -> u64 {
    let mut nodes: Vec<Node> = (0..n)
        .map(|i| Node {
            value: i,
            edge: ((i + 1) % n) as u32,
        })
        .collect();
    for _ in 0..rounds {
        for node in &mut nodes {
            node.value += 1;
        }
    }
    let mut sum = 0;
    let mut at = 0;
    for _ in 0..n {
        sum += nodes[at].value;
        at = nodes[at].edge as usize;
    }
    sum
}
fn churn(n: u64, rounds: u64) -> u64 {
    let mut p = Pool::new(n as usize);
    let mut ids = Vec::with_capacity(n as usize);
    for i in 0..n {
        ids.push(p.insert(Node {
            value: i,
            edge: ((i + 1) % n) as u32,
        }));
    }
    let mut stale = 0;
    for _ in 0..rounds {
        for h in &mut ids {
            let old = *h;
            let mut node = p.remove(old).unwrap();
            node.value += 1;
            *h = p.insert(node);
            if p.get(old).is_none() {
                stale += 1;
            }
        }
    }
    stale + ids.iter().map(|h| p.get(*h).unwrap().value).sum::<u64>()
}
struct RNode {
    value: u64,
    tail: Option<Rc<RNode>>,
}
struct ANode {
    value: u64,
    tail: Option<Arc<ANode>>,
}
fn copy_rc(node: &Rc<RNode>, depth: u64) -> Rc<RNode> {
    Rc::new(RNode {
        value: node.value + 1,
        tail: if depth == 0 {
            node.tail.clone()
        } else {
            node.tail.as_ref().map(|n| copy_rc(n, depth - 1))
        },
    })
}
fn copy_arc(node: &Arc<ANode>, depth: u64) -> Arc<ANode> {
    Arc::new(ANode {
        value: node.value + 1,
        tail: if depth == 0 {
            node.tail.clone()
        } else {
            node.tail.as_ref().map(|n| copy_arc(n, depth - 1))
        },
    })
}
fn sum_rc(node: &Rc<RNode>) -> u64 {
    node.value + node.tail.as_ref().map_or(0, sum_rc)
}
fn sum_arc(node: &Arc<ANode>) -> u64 {
    node.value + node.tail.as_ref().map_or(0, sum_arc)
}
fn snapshots_rc(n: u64, versions: u64) -> u64 {
    let mut root = None;
    for i in 0..n {
        root = Some(Rc::new(RNode {
            value: i,
            tail: root,
        }));
    }
    let mut held = Vec::with_capacity(versions as usize);
    for _ in 0..versions {
        let next = copy_rc(root.as_ref().unwrap(), n / 2);
        held.push(next.clone());
        root = Some(next);
    }
    held.iter().map(sum_rc).sum()
}
fn snapshots_arc(n: u64, versions: u64) -> u64 {
    let mut root = None;
    for i in 0..n {
        root = Some(Arc::new(ANode {
            value: i,
            tail: root,
        }));
    }
    let mut held = Vec::with_capacity(versions as usize);
    for _ in 0..versions {
        let next = copy_arc(root.as_ref().unwrap(), n / 2);
        held.push(next.clone());
        root = Some(next);
    }
    held.iter().map(sum_arc).sum()
}
fn main() {
    let phase = std::env::args().nth(1).expect("phase");
    let answer = match phase.as_str() {
        "ordinary" => (0..64).map(|_| ordinary(8192)).sum(),
        "sized" => (0..64).map(|_| packed(8192, false)).sum(),
        "streaming" => (0..64).map(|_| packed(8192, true)).sum(),
        "exclusive" => exclusive(8192, 32),
        "churn" => churn(4096, 16),
        "rc" => snapshots_rc(64, 256),
        "arc" => snapshots_arc(64, 256),
        "lifetime" => {
            exclusive(8192, 32) + churn(4096, 16) + snapshots_rc(64, 256) + snapshots_arc(64, 256)
        }
        "laws" => {
            assert_eq!(ordinary(6), 33);
            assert_eq!(packed(6, false), 33);
            assert_eq!(exclusive(4, 2), 14);
            assert_eq!(churn(4, 2), 22);
            assert_eq!(snapshots_rc(3, 3), 21);
            assert_eq!(snapshots_arc(3, 3), 21);
            0
        }
        _ => panic!("unknown phase"),
    };
    println!("{answer}");
}

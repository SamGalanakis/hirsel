// Reviewed-by: twin-review-925, 2026-11-14. Generated valid phases/lifetime and malformed probes approved; Capacity and recording-sink controls excluded.
// Safe std-only Rust; checked arithmetic; u32 edges, full u64 identity/generation/index.
use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
include!("graph_inputs.rs");
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
// Idiomatic generational arena: separate live payloads, generations and free indices.
// Same full generation and pool-identity checks as the matched intrusive arena.
struct Pool {
    me: u64,
    nodes: Vec<Option<Node>>,
    generations: Vec<u64>,
    free: Vec<u32>,
}
impl Pool {
    fn new(n: usize) -> Self {
        static UNIQUE: AtomicU64 = AtomicU64::new(1);
        Self {
            me: UNIQUE.fetch_add(1, Ordering::Relaxed),
            nodes: Vec::with_capacity(n),
            generations: Vec::with_capacity(n),
            free: Vec::new(),
        }
    }
    fn insert(&mut self, node: Node) -> Id {
        let index = if let Some(i) = self.free.pop() {
            self.nodes[i as usize] = Some(node);
            u64::from(i)
        } else {
            let i = self.nodes.len();
            self.nodes.push(Some(node));
            self.generations.push(0);
            i as u64
        };
        Id {
            pool: self.me,
            index,
            generation: self.generations[index as usize],
        }
    }
    fn get(&self, h: Id) -> Option<&Node> {
        if h.pool != self.me
            || self.generations.get(h.index as usize).copied() != Some(h.generation)
        {
            return None;
        }
        self.nodes.get(h.index as usize)?.as_ref()
    }
    fn remove(&mut self, h: Id) -> Option<Node> {
        self.get(h)?;
        let index = h.index as usize;
        let node = self.nodes[index].take();
        self.generations[index] += 1;
        if self.generations[index] != u64::MAX {
            self.free.push(u32::try_from(index).unwrap())
        }
        node
    }
    fn clear(&mut self) {
        self.free.clear();
        for i in (0..self.nodes.len()).rev() {
            if self.nodes[i].take().is_some() {
                self.generations[i] += 1;
            }
            if self.generations[i] != u64::MAX {
                self.free.push(u32::try_from(i).unwrap());
            }
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
        "exclusive" => exclusive(EXCLUSIVE_NODES, EXCLUSIVE_ROUNDS),
        "churn" => churn(CHURN_NODES, CHURN_ROUNDS),
        "rc" => snapshots_rc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS),
        "arc" => snapshots_arc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS),
        "lifetime" => {
            exclusive(EXCLUSIVE_NODES, EXCLUSIVE_ROUNDS)
                + churn(CHURN_NODES, CHURN_ROUNDS)
                + snapshots_rc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS)
                + snapshots_arc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS)
        }
        "graph_malformed" => {
            assert_eq!(dimensions(MALFORMED_NODES), Err(0));
            assert_eq!(dimensions(MALFORMED_WIDTH_NODES), Err(1));
            let nodes = vec![
                Node { value: 0, edge: 1 },
                Node {
                    value: 1,
                    edge: u32::try_from(MALFORMED_EDGE).unwrap(),
                },
            ];
            assert_eq!(edges(&nodes), Err(1));
            let mut p = Pool::new(1);
            let other = Pool::new(1);
            let old = p.insert(Node { value: 7, edge: 0 });
            assert!(other.get(old).is_none());
            let node = p.remove(old).unwrap();
            assert!(p.remove(old).is_none());
            let fresh = p.insert(node);
            assert!(p.get(old).is_none());
            assert_eq!(p.get(fresh).unwrap().value, 7);
            p.clear();
            assert!(p.get(fresh).is_none());
            7
        }
        "laws" => {
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
fn dimensions(n: u64) -> Result<(), u64> {
    if n == 0 {
        Err(0)
    } else if n > u64::from(u32::MAX) {
        Err(1)
    } else {
        Ok(())
    }
}
fn edges(nodes: &[Node]) -> Result<(), u64> {
    for (i, node) in nodes.iter().enumerate() {
        if node.edge as usize >= nodes.len() {
            return Err(i as u64);
        }
    }
    Ok(())
}

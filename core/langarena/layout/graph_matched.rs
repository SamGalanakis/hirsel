// Reviewed-by: twin-review-925, graph-twin-recheck-929, orchestrator R1/R2 diff read, 2026-11-14.
// APPROVED for generated successful graph phases/lifetime and scoped malformed probes.
// Admission bookkeeping is lighter than Hirsel: analytic bias10^5..10^6 I/snapshot phase, favours Rust and raises H/R.
// Safe std-only Rust, one root funding domain, no extra counted-payload fields.
// Logical admission is separate from physical layout/OOM. A host OOM stays fatal.
use std::{
    cell::Cell,
    ops::{Index, IndexMut},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
include!("graph_inputs.rs");
const ROOT_AVAILABLE: u64 = 64 * 1024 * 1024 - 288;
thread_local! {static AVAILABLE:Cell<u64>=const {Cell::new(ROOT_AVAILABLE)};}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Capacity {
    requested: u64,
    available: u64,
}
fn charge(bytes: u64) -> Result<(), Capacity> {
    AVAILABLE.with(|b| {
        let available = b.get();
        if bytes > available {
            Err(Capacity {
                requested: bytes,
                available,
            })
        } else {
            b.set(available - bytes);
            Ok(())
        }
    })
}
fn refund(bytes: u64) {
    AVAILABLE.with(|b| b.set(b.get() + bytes));
}
fn extent(n: u64, element: u64) -> Result<u64, Capacity> {
    n.checked_mul(element)
        .and_then(|x| x.checked_add(7))
        .map(|x| x & !7)
        .ok_or_else(|| Capacity {
            requested: u64::MAX,
            available: AVAILABLE.with(Cell::get),
        })
}
// One Vec header, just like List. The charge is a type constant, not a field.
struct Values<T, const ELEMENT: u64> {
    data: Vec<T>,
}
impl<T, const ELEMENT: u64> Values<T, ELEMENT> {
    fn new() -> Self {
        Self { data: Vec::new() }
    }
    fn with_capacity(n: u64) -> Result<Self, Capacity> {
        charge(extent(n, ELEMENT)?)?;
        Ok(Self {
            data: Vec::with_capacity(usize::try_from(n).expect("capacity width")),
        })
    }
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn push(&mut self, value: T) -> Result<(), (Capacity, T)> {
        if self.data.len() == self.data.capacity() {
            let old = self.data.capacity() as u64;
            let target = match old.checked_mul(2) {
                Some(n) => n.max(4).max(self.len() + 1),
                None => {
                    return Err((
                        Capacity {
                            requested: u64::MAX,
                            available: AVAILABLE.with(Cell::get),
                        },
                        value,
                    ));
                }
            };
            let admitted = extent(target, ELEMENT).and_then(charge);
            if let Err(c) = admitted {
                return Err((c, value));
            }
            // Admit the full new extent before refunding the old, as GROWTH does.
            let mut next = Vec::with_capacity(usize::try_from(target).expect("capacity width"));
            for v in self.data.drain(..) {
                next.push(v);
            }
            self.data = next;
            if old != 0 {
                refund(extent(old, ELEMENT).expect("previous admitted extent"));
            }
        }
        self.data.push(value);
        Ok(())
    }
    fn get(&self, index: u64) -> Option<&T> {
        self.data.get(usize::try_from(index).expect("index width"))
    }
    fn set(&mut self, index: u64, value: T) -> T {
        std::mem::replace(&mut self[index], value)
    }
}
impl<T, const ELEMENT: u64> Index<u64> for Values<T, ELEMENT> {
    type Output = T;
    fn index(&self, i: u64) -> &T {
        &self.data[usize::try_from(i).expect("index width")]
    }
}
impl<T, const ELEMENT: u64> IndexMut<u64> for Values<T, ELEMENT> {
    fn index_mut(&mut self, i: u64) -> &mut T {
        &mut self.data[usize::try_from(i).expect("index width")]
    }
}
impl<T, const ELEMENT: u64> Drop for Values<T, ELEMENT> {
    fn drop(&mut self) {
        let values = std::mem::take(&mut self.data);
        let capacity = values.capacity();
        let bytes = extent(capacity as u64, ELEMENT).expect("admitted extent");
        drop(values);
        if capacity != 0 {
            refund(bytes);
        }
    }
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
#[derive(Debug, PartialEq, Eq)]
enum Stale {
    WrongPool,
    Removed,
}
enum Slot {
    Occupied(u64, Node),
    Vacant(u64, u64),
}
struct Pool {
    me: u64,
    slots: Values<Slot, 28>,
    free: u64,
    live: u64,
}
fn unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .expect("Unique overflow")
}
impl Pool {
    fn new() -> Self {
        Self {
            me: unique(),
            slots: Values::new(),
            free: u64::MAX,
            live: 0,
        }
    }
    fn with_capacity(n: u64) -> Result<Self, Capacity> {
        let slots = Values::with_capacity(n)?;
        Ok(Self {
            me: unique(),
            slots,
            free: u64::MAX,
            live: 0,
        })
    }
    fn insert(&mut self, value: Node) -> Result<Id, (Capacity, Node)> {
        if self.free == u64::MAX {
            let index = self.slots.len();
            if let Err((c, slot)) = self.slots.push(Slot::Occupied(0, value)) {
                return match slot {
                    Slot::Occupied(_, value) => Err((c, value)),
                    _ => unreachable!(),
                };
            }
            self.live += 1;
            return Ok(Id {
                pool: self.me,
                index,
                generation: 0,
            });
        }
        let index = self.free;
        let (generation, next) = match self.slots[index] {
            Slot::Vacant(g, n) => (g, n),
            _ => unreachable!(),
        };
        self.slots.set(index, Slot::Occupied(generation, value));
        self.free = next;
        self.live += 1;
        Ok(Id {
            pool: self.me,
            index,
            generation,
        })
    }
    fn get(&self, h: Id) -> Result<&Node, Stale> {
        if h.pool != self.me {
            return Err(Stale::WrongPool);
        }
        match self.slots.get(h.index) {
            Some(Slot::Occupied(g, n)) if *g == h.generation => Ok(n),
            _ => Err(Stale::Removed),
        }
    }
    fn remove(&mut self, h: Id) -> Result<Node, Stale> {
        self.get(h)?;
        let generation = h.generation + 1;
        let retired = generation == u64::MAX;
        let link = if retired { u64::MAX } else { self.free };
        let slot = self.slots.set(h.index, Slot::Vacant(generation, link));
        if !retired {
            self.free = h.index;
        }
        self.live -= 1;
        match slot {
            Slot::Occupied(_, n) => Ok(n),
            _ => unreachable!(),
        }
    }
    fn clear(&mut self) {
        self.free = u64::MAX;
        let mut i = self.slots.len();
        while i > 0 {
            i -= 1;
            let generation = match self.slots[i] {
                Slot::Occupied(g, _) => g + 1,
                Slot::Vacant(g, _) => g,
            };
            self.slots.set(i, Slot::Vacant(generation, self.free));
            if generation != u64::MAX {
                self.free = i;
            }
        }
        self.live = 0;
    }
}
fn inserted(value: Result<Id, (Capacity, Node)>) -> Result<Id, Capacity> {
    value.map_err(|(c, _)| c)
}
fn exclusive(n: u64, rounds: u64) -> Result<u64, Capacity> {
    let mut nodes: Values<Node, 12> = Values::with_capacity(n)?;
    for i in 0..n {
        let _ = nodes.push(Node {
            value: i,
            edge: u32::try_from((i + 1) % n).expect("edge width"),
        });
    }
    for _ in 0..rounds {
        for i in 0..n {
            nodes[i].value += 1;
        }
    }
    let mut sum = 0u64;
    let mut at = 0u64;
    for _ in 0..n {
        sum += nodes[at].value;
        at = u64::from(nodes[at].edge);
    }
    Ok(sum)
}
fn churn(n: u64, rounds: u64) -> Result<u64, Capacity> {
    let mut p = Pool::with_capacity(n)?;
    let mut ids: Values<Id, 24> = Values::with_capacity(n)?;
    for i in 0..n {
        let h = inserted(p.insert(Node {
            value: i,
            edge: u32::try_from((i + 1) % n).expect("edge width"),
        }))?;
        let _ = ids.push(h);
    }
    let mut stale = 0u64;
    for _ in 0..rounds {
        for i in 0..n {
            let old = ids[i];
            let mut node = p.remove(old).unwrap();
            node.value += 1;
            let fresh = inserted(p.insert(node))?;
            if p.get(old).is_err() {
                stale += 1;
            }
            ids.set(i, fresh);
        }
    }
    let mut sum = stale;
    for h in &ids.data {
        sum += p.get(*h).unwrap().value;
    }
    Ok(sum)
}
struct RNode {
    value: u64,
    tail: Option<Rc<RNode>>,
}
struct ANode {
    value: u64,
    tail: Option<Arc<ANode>>,
}
// Payload layout is unchanged. Refunding a counted admission occurs only when
// the final std strong holder drops the payload; no weak holders are constructed.
impl Drop for RNode {
    fn drop(&mut self) {
        drop(self.tail.take());
        refund(32);
    }
}
impl Drop for ANode {
    fn drop(&mut self) {
        drop(self.tail.take());
        refund(32);
    }
}
fn new_rc(value: u64, tail: Option<Rc<RNode>>) -> Result<Rc<RNode>, Capacity> {
    // On refusal the already-owned tail is dropped; an unadmitted node never exists.
    charge(32)?;
    Ok(Rc::new(RNode { value, tail }))
}
fn new_arc(value: u64, tail: Option<Arc<ANode>>) -> Result<Arc<ANode>, Capacity> {
    charge(32)?;
    Ok(Arc::new(ANode { value, tail }))
}
fn copy_rc(node: &Rc<RNode>, depth: u64) -> Result<Rc<RNode>, Capacity> {
    let tail = if depth == 0 {
        node.tail.clone()
    } else {
        match &node.tail {
            Some(t) => Some(copy_rc(t, depth - 1)?),
            None => None,
        }
    };
    new_rc(node.value + 1, tail)
}
fn copy_arc(node: &Arc<ANode>, depth: u64) -> Result<Arc<ANode>, Capacity> {
    let tail = if depth == 0 {
        node.tail.clone()
    } else {
        match &node.tail {
            Some(t) => Some(copy_arc(t, depth - 1)?),
            None => None,
        }
    };
    new_arc(node.value + 1, tail)
}
fn sum_rc(node: &Rc<RNode>) -> u64 {
    node.value + node.tail.as_ref().map_or(0, sum_rc)
}
fn sum_arc(node: &Arc<ANode>) -> u64 {
    node.value + node.tail.as_ref().map_or(0, sum_arc)
}
fn snapshots_rc(n: u64, versions: u64) -> Result<u64, Capacity> {
    let mut root = None;
    for i in 0..n {
        root = Some(new_rc(i, root.take())?);
    }
    let mut held: Values<Rc<RNode>, 8> = Values::with_capacity(versions)?;
    for _ in 0..versions {
        let next = copy_rc(root.as_ref().expect("empty graph"), n / 2)?;
        let _ = held.push(next.clone());
        root = Some(next);
    }
    let mut sum = 0u64;
    for head in &held.data {
        sum += sum_rc(head);
    }
    Ok(sum)
}
fn snapshots_arc(n: u64, versions: u64) -> Result<u64, Capacity> {
    let mut root = None;
    for i in 0..n {
        root = Some(new_arc(i, root.take())?);
    }
    let mut held: Values<Arc<ANode>, 8> = Values::with_capacity(versions)?;
    for _ in 0..versions {
        let next = copy_arc(root.as_ref().expect("empty graph"), n / 2)?;
        let _ = held.push(next.clone());
        root = Some(next);
    }
    let mut sum = 0u64;
    for head in &held.data {
        sum += sum_arc(head);
    }
    Ok(sum)
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
fn edges(nodes: &Values<Node, 12>) -> Result<(), u64> {
    for i in 0..nodes.len() {
        if u64::from(nodes[i].edge) >= nodes.len() {
            return Err(i);
        }
    }
    Ok(())
}
fn malformed() -> Result<u64, Capacity> {
    assert_eq!(dimensions(MALFORMED_NODES), Err(0));
    assert_eq!(dimensions(MALFORMED_WIDTH_NODES), Err(1));
    {
        let mut nodes: Values<Node, 12> = Values::new();
        nodes.push(Node { value: 0, edge: 1 }).map_err(|(c, _)| c)?;
        nodes
            .push(Node {
                value: 1,
                edge: u32::try_from(MALFORMED_EDGE).unwrap(),
            })
            .map_err(|(c, _)| c)?;
        assert_eq!(edges(&nodes), Err(1));
    }
    let mut p = Pool::new();
    let other = Pool::new();
    let old = inserted(p.insert(Node { value: 7, edge: 0 }))?;
    assert!(matches!(other.get(old), Err(Stale::WrongPool)));
    let node = p.remove(old).unwrap();
    assert!(matches!(p.remove(old), Err(Stale::Removed)));
    let fresh = inserted(p.insert(node))?;
    assert!(matches!(p.get(old), Err(Stale::Removed)));
    assert_eq!(p.get(fresh).unwrap().value, 7);
    p.clear();
    assert!(matches!(p.get(fresh), Err(Stale::Removed)));
    Ok(7)
}
// Only tests enter a synthetic bounded domain; no live node may cross its edge.
// The measured graph has one root domain throughout its complete lifetime.
fn limited<T>(bytes: u64, f: impl FnOnce() -> T) -> T {
    AVAILABLE.with(|b| {
        assert_eq!(b.get(), ROOT_AVAILABLE);
        b.set(bytes);
    });
    let result = f();
    AVAILABLE.with(|b| {
        assert_eq!(b.get(), bytes);
        b.set(ROOT_AVAILABLE);
    });
    result
}
fn allocation_laws() {
    assert_eq!(limited(48, || exclusive(4, 2)), Ok(14));
    assert_eq!(
        limited(47, || exclusive(4, 2)),
        Err(Capacity {
            requested: 48,
            available: 47
        })
    );
    assert_eq!(limited(208, || churn(4, 2)), Ok(22));
    assert_eq!(
        limited(207, || churn(4, 2)),
        Err(Capacity {
            requested: 96,
            available: 95
        })
    );
    for cap in 0..248 {
        let requested = if (96..120).contains(&cap) { 24 } else { 32 };
        let base = if cap < 32 {
            0
        } else if cap < 64 {
            32
        } else if cap < 96 {
            64
        } else if cap < 120 {
            96
        } else if cap < 152 {
            120
        } else if cap < 184 {
            152
        } else if cap < 216 {
            184
        } else {
            216
        };
        let expected = Err(Capacity {
            requested,
            available: cap - base,
        });
        assert_eq!(limited(cap, || snapshots_rc(3, 3)), expected);
        assert_eq!(limited(cap, || snapshots_arc(3, 3)), expected);
    }
    assert_eq!(limited(248, || snapshots_rc(3, 3)), Ok(21));
    assert_eq!(limited(248, || snapshots_arc(3, 3)), Ok(21));
    // Refused first GROWTH returns its input and leaves the empty pool unchanged.
    limited(111, || {
        let mut p = Pool::new();
        let (c, node) = p.insert(Node { value: 7, edge: 0 }).err().unwrap();
        assert_eq!(
            c,
            Capacity {
                requested: 112,
                available: 111
            }
        );
        assert_eq!(node.value, 7);
        assert_eq!(p.live, 0);
    });
}
fn main() {
    let phase = std::env::args().nth(1).expect("phase");
    let answer = match phase.as_str() {
        "exclusive" => exclusive(EXCLUSIVE_NODES, EXCLUSIVE_ROUNDS),
        "churn" => churn(CHURN_NODES, CHURN_ROUNDS),
        "rc" => snapshots_rc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS),
        "arc" => snapshots_arc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS),
        "lifetime" => (|| {
            Ok(exclusive(EXCLUSIVE_NODES, EXCLUSIVE_ROUNDS)?
                + churn(CHURN_NODES, CHURN_ROUNDS)?
                + snapshots_rc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS)?
                + snapshots_arc(SNAPSHOT_NODES, SNAPSHOT_VERSIONS)?)
        })(),
        "graph_malformed" => malformed(),
        "laws" => {
            assert_eq!(exclusive(4, 2), Ok(14));
            assert_eq!(churn(4, 2), Ok(22));
            assert_eq!(snapshots_rc(3, 3), Ok(21));
            assert_eq!(snapshots_arc(3, 3), Ok(21));
            allocation_laws();
            Ok(0)
        }
        _ => panic!("unknown phase"),
    }
    .expect("Capacity");
    println!("{answer}");
}

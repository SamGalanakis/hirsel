//! The store's in-memory work per commit, shared by `store_pcopy.rs` (R1, the indexed overwrite) and
//! `store_journal.rs` (R4, R5) through `#[path]`: a persistent path-copying B+ tree with the spec's
//! fan-out, a transaction over it, and `run` (STORE-SPEC v2.13 §1.2, §1.11, §3.1, §6 Q3).
//!
//! Node layout. A node is one `Arc<Node>` holding its id and one vector. `Kind::Leaf(Vec<Arc<Entry>>)`
//! holds 16 to 32 rows in key order; a row is one `Arc<Entry>` owning its key and value bytes
//! (`Box<[u8]>` each), as std.omap's one canonical `Arc[Entry[K, V]]` per row (998).
//! `Kind::Branch(Vec<Child>)` holds 16 to 32 children; a `Child` is its subtree's minimum row (the
//! same `Arc<Entry>` the leaf holds) and an `Arc<Node>`. Only the root may hold fewer. A node that
//! reaches 33 splits into 16 and 17, as std.omap's `split_leaves` and `split_branches` do, so the
//! two trees have the same depth and copy the same path per put.
//!
//! What is shared. A put copies the nodes on the path from the root to the leaf (new vectors whose
//! untouched elements are one `Arc` count increment each) and nothing else: every other node, every
//! row, and the catalog tree are shared with the base version. Each new node takes the next id from
//! the Db's counter (§3.1 step 2). The transaction keeps the base roots until publish, so the copy
//! is never skipped, and publish drops them, which drops the old path's nodes and their counts.
//!
//! Per commit (§3.1): one wall-clock read (`at`, in ms, never below the last commit's), the body under
//! `catch_unwind` (isolate), the journal's COMMIT encode when the store has one (none in memory, Q2),
//! the resident check counting the frames in flight (§1.11), publish at version + 1, then the
//! hand-off. Per put: the write check (I3: the table's catalog entry holds its id and schema epoch),
//! encode, the key and value limits, the COMMIT size against `Limit(Transaction)` by arithmetic (U1,
//! both modes, L29), the path copy, the op-list entry (one per key, replacing an earlier one), I4's
//! account update with the replaced entry's size from the insert (v2.10 T2), and each index's entry
//! (J1: none when the old and new index keys are byte-equal). Per fetch: the read check (I3: the id),
//! the lookup, the `Point` record of the data-tree key, and the decode.
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub type Bytes = Box<[u8]>;

pub struct Entry {
    pub key: Bytes,
    pub value: Bytes,
}
pub type Row = Arc<Entry>;
#[derive(Clone)]
pub struct Child {
    low: Row,
    node: Arc<Node>,
}
pub struct Node {
    id: u64,
    kind: Kind,
}
enum Kind {
    Leaf(Vec<Row>),
    Branch(Vec<Child>),
}
const MAX: usize = 32;
const MIN: usize = 16;

enum Ins {
    One(Arc<Node>),
    Two(Arc<Node>, Child),
}

fn node(ids: &mut u64, kind: Kind) -> Arc<Node> {
    *ids += 1;
    Arc::new(Node { id: *ids, kind })
}
fn route(cs: &[Child], k: &[u8]) -> usize {
    cs.partition_point(|c| *c.low.key <= *k).saturating_sub(1)
}
pub fn get<'a>(mut n: &'a Node, k: &[u8]) -> Option<&'a Entry> {
    loop {
        match &n.kind {
            Kind::Branch(cs) => n = &cs[route(cs, k)].node,
            Kind::Leaf(es) => {
                return es
                    .binary_search_by(|e| (*e.key).cmp(k))
                    .ok()
                    .map(|i| &*es[i]);
            }
        }
    }
}
fn split<T>(ids: &mut u64, mut xs: Vec<T>, wrap: fn(Vec<T>) -> Kind, low: fn(&T) -> Row) -> Ins {
    if xs.len() <= MAX {
        return Ins::One(node(ids, wrap(xs)));
    }
    let right = xs.split_off(MIN);
    let low = low(&right[0]);
    let left = node(ids, wrap(xs));
    Ins::Two(
        left,
        Child {
            low,
            node: node(ids, wrap(right)),
        },
    )
}
/// The new node for `n` with `e` put, and the entry it replaced. `n` is not changed.
fn insert(ids: &mut u64, n: &Node, e: Row) -> (Ins, Option<Row>) {
    match &n.kind {
        Kind::Leaf(es) => match es.binary_search_by(|x| (*x.key).cmp(&e.key)) {
            Ok(i) => {
                let mut copy = es.clone();
                let old = std::mem::replace(&mut copy[i], e);
                (Ins::One(node(ids, Kind::Leaf(copy))), Some(old))
            }
            Err(i) => {
                let mut copy = Vec::with_capacity(es.len() + 1);
                copy.extend_from_slice(&es[..i]);
                copy.push(e);
                copy.extend_from_slice(&es[i..]);
                (split(ids, copy, Kind::Leaf, Arc::clone), None)
            }
        },
        Kind::Branch(cs) => {
            let i = route(cs, &e.key);
            let (sub, old) = insert(ids, &cs[i].node, e);
            let ins = match sub {
                Ins::One(n) => {
                    let mut copy = cs.clone();
                    copy[i].node = n;
                    Ins::One(node(ids, Kind::Branch(copy)))
                }
                Ins::Two(left, right) => {
                    let mut copy = Vec::with_capacity(cs.len() + 1);
                    copy.extend_from_slice(&cs[..i]);
                    copy.push(Child {
                        low: cs[i].low.clone(),
                        node: left,
                    });
                    copy.push(right);
                    copy.extend_from_slice(&cs[i + 1..]);
                    split(ids, copy, Kind::Branch, |c| c.low.clone())
                }
            };
            (ins, old)
        }
    }
}
fn low(n: &Node) -> Row {
    match &n.kind {
        Kind::Leaf(es) => es[0].clone(),
        Kind::Branch(cs) => cs[0].low.clone(),
    }
}
fn put_root(ids: &mut u64, root: &Arc<Node>, e: Row) -> (Arc<Node>, Option<Row>) {
    match insert(ids, root, e) {
        (Ins::One(n), old) => (n, old),
        (Ins::Two(left, right), old) => {
            let low = low(&left);
            (
                node(ids, Kind::Branch(vec![Child { low, node: left }, right])),
                old,
            )
        }
    }
}
pub fn leaf(ids: &mut u64, rows: Vec<Row>) -> Arc<Node> {
    node(ids, Kind::Leaf(rows))
}
/// Rows in key order, checking §1.2's fan-out on every node but the root and that every node id was
/// issued by the counter (outside the timed phase).
pub fn walk(n: &Node, root: bool, issued: u64, prev: &mut Option<Row>, out: &mut Vec<Row>) {
    let len = match &n.kind {
        Kind::Leaf(es) => es.len(),
        Kind::Branch(cs) => cs.len(),
    };
    assert!(len <= MAX && (root || len >= MIN), "node of {len}");
    assert!(n.id >= 1 && n.id <= issued, "node id {} of {issued}", n.id);
    match &n.kind {
        Kind::Leaf(es) => {
            for e in es {
                assert!(prev.as_ref().is_none_or(|p| p.key < e.key));
                *prev = Some(e.clone());
                out.push(e.clone());
            }
        }
        Kind::Branch(cs) => {
            for c in cs {
                walk(&c.node, false, issued, prev, out);
            }
        }
    }
}

#[derive(Clone)]
pub struct Roots {
    pub data: Arc<Node>,
    pub catalog: Arc<Node>,
}
pub struct Db {
    pub head: Roots,
    pub version: u64,
    pub resident: u64,
    pub limit: u64,
    pub ids: u64,
    pub at: u64,
}
/// An index on `id % 4`-style extractors: §1.2's erased `by` and its `Key[u64]` (8 bytes BE).
pub struct Index {
    pub id: u32,
    pub by: fn(&Account) -> u64,
}
pub struct Table {
    pub id: u32,
    pub epoch: u64,
    pub complete: bool,
    pub catalog_key: Bytes,
    pub indexes: Vec<Index>,
}
#[derive(Debug)]
pub enum Error {
    Stale(u32),
    Limit(&'static str),
    Capacity { requested: u64, available: u64 },
    Panicked,
}
/// The op list: one entry per data-tree key, the last write winning (§2.4). A put holds the tree's
/// own row; a delete holds its key.
pub enum Op {
    Put(Row),
    Delete(Bytes),
}
impl Op {
    pub fn key(&self) -> &[u8] {
        match self {
            Op::Put(r) => &r.key,
            Op::Delete(k) => k,
        }
    }
    /// The op's bytes in a COMMIT payload (§2.4): tag, then the key and value blobs.
    fn size(&self) -> u64 {
        match self {
            Op::Put(r) => 9 + r.key.len() as u64 + r.value.len() as u64,
            Op::Delete(k) => 5 + k.len() as u64,
        }
    }
}
#[expect(
    dead_code,
    reason = "the read log is returned in Done as the store returns it; the callers drop it"
)]
pub enum ReadRec {
    Point(Bytes),
}
pub struct Tx {
    base: Roots,
    work: Roots,
    pub ops: Vec<Op>,
    reads: Vec<ReadRec>,
    delta: i64,
    ids: u64,
    /// The COMMIT frame's size so far: head 16, branch, version, at, sections, tag and op count (42).
    pub frame_bytes: u64,
}
pub struct Done<T> {
    pub value: T,
    pub version: u64,
    pub at: u64,
    pub ticket: u64,
    pub reads: Vec<ReadRec>,
}
/// What `run` hands a commit after its body: the memory store encodes nothing (Q2); the journal
/// encodes the COMMIT, counts it in flight and sends it after publish (§3.1 steps 3 to 5).
pub trait Journal {
    fn poll(&mut self) {}
    fn encode(&mut self, _ops: &mut Vec<Op>, _version: u64, _at: u64, _bytes: u64) -> u64 {
        0
    }
    fn inflight(&self) -> u64 {
        0
    }
    /// The ticket's sequence number.
    fn send(&mut self, version: u64) -> u64 {
        version
    }
}
pub struct Memory;
impl Journal for Memory {}

/// §1.11: two `Bytes` handles, each one's storage rounded up to 8, and the node share.
pub fn entry(k: usize, v: usize) -> i64 {
    i64::try_from(62 + k.next_multiple_of(8) + v.next_multiple_of(8)).unwrap()
}
pub fn run<T, J: Journal>(
    db: &mut Db,
    journal: &mut J,
    body: impl FnOnce(&mut Tx) -> Result<T, Error>,
) -> Result<Done<T>, Error> {
    journal.poll();
    let wall = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let at = wall.max(db.at);
    let mut tx = Tx {
        base: db.head.clone(),
        work: db.head.clone(),
        ops: Vec::new(),
        reads: Vec::new(),
        delta: 0,
        ids: db.ids,
        frame_bytes: 42,
    };
    let value = catch_unwind(AssertUnwindSafe(|| body(&mut tx))).map_err(|_| Error::Panicked)??;
    let version = db.version + 1;
    let frame = journal.encode(&mut tx.ops, version, at, tx.frame_bytes);
    let after = db.resident.checked_add_signed(tx.delta).unwrap();
    let admitted = after + journal.inflight() + frame;
    if admitted > db.limit {
        return Err(Error::Capacity {
            requested: admitted,
            available: db.limit,
        });
    }
    db.resident = after;
    db.head = tx.work;
    db.version = version;
    db.ids = tx.ids;
    db.at = at;
    drop(tx.base);
    let ticket = journal.send(version);
    Ok(Done {
        value,
        version,
        at,
        ticket,
        reads: tx.reads,
    })
}

/// The catalog value's prefix (§2.4): kind u8, id u32, owner u32, schema epoch u64, all LE.
pub fn catalog_value(kind: u8, id: u32, owner: u32, epoch: u64) -> Bytes {
    let mut b = Vec::with_capacity(17);
    b.push(kind);
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&owner.to_le_bytes());
    b.extend_from_slice(&epoch.to_le_bytes());
    b.into_boxed_slice()
}
fn catalog_id(v: &[u8]) -> u32 {
    u32::from_le_bytes(v[1..5].try_into().unwrap())
}
/// I3 on a read: the handle's id is live in the catalog of the roots it reads.
fn readable(roots: &Roots, catalog_key: &[u8], id: u32) -> Result<(), Error> {
    match get(&roots.catalog, catalog_key) {
        Some(e) if catalog_id(&e.value) == id => Ok(()),
        _ => Err(Error::Stale(id)),
    }
}
/// I3 on a write: the id is live and the Table holds every index the catalog lists (its epoch).
fn writable(roots: &Roots, t: &Table) -> Result<(), Error> {
    if !t.complete {
        return Err(Error::Stale(t.id));
    }
    match get(&roots.catalog, &t.catalog_key) {
        Some(e) if catalog_id(&e.value) == t.id && e.value[9..17] == t.epoch.to_le_bytes() => {
            Ok(())
        }
        _ => Err(Error::Stale(t.id)),
    }
}
fn logical_key(id: u32, key: &[u8]) -> Bytes {
    let mut b = Vec::with_capacity(4 + key.len());
    b.extend_from_slice(&id.to_be_bytes());
    b.extend_from_slice(key);
    b.into_boxed_slice()
}
/// One data-tree write: the limits, the COMMIT size, the path copy, the op list and I4. Returns the
/// replaced row.
fn put_logical_key(tx: &mut Tx, key: Bytes, value: Bytes) -> Result<Option<Row>, Error> {
    if key.len() > 4096 {
        return Err(Error::Limit("Key"));
    }
    if value.len() > 1_048_576 {
        return Err(Error::Limit("Value"));
    }
    let size = entry(key.len(), value.len());
    let row = Arc::new(Entry { key, value });
    let op = Op::Put(row.clone());
    let earlier = tx.ops.iter().position(|o| o.key() == &*row.key);
    let frame = tx.frame_bytes - earlier.map_or(0, |i| tx.ops[i].size()) + op.size();
    if frame - 16 > 16_777_280 {
        return Err(Error::Limit("Transaction"));
    }
    let (data, old) = put_root(&mut tx.ids, &tx.work.data, row);
    match earlier {
        Some(i) => tx.ops[i] = op,
        None => tx.ops.push(op),
    }
    tx.work.data = data;
    tx.frame_bytes = frame;
    tx.delta += size
        - old
            .as_ref()
            .map_or(0, |o| entry(o.key.len(), o.value.len()));
    Ok(old)
}
fn index_key(ix: &Index, a: &Account, key: &[u8; 8]) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[..4].copy_from_slice(&ix.id.to_be_bytes());
    b[4..12].copy_from_slice(&(ix.by)(a).to_be_bytes());
    b[12..].copy_from_slice(key);
    b
}
pub fn put(tx: &mut Tx, t: &Table, k: u64, a: &Account) -> Result<(), Error> {
    writable(&tx.work, t)?;
    let key = k.to_be_bytes();
    let old = put_logical_key(tx, logical_key(t.id, &key), encode(a).into_boxed_slice())?;
    if t.indexes.is_empty() {
        return Ok(());
    }
    let previous = old.map(|o| decode(&o.value));
    for ix in &t.indexes {
        let new = index_key(ix, a, &key);
        if let Some(p) = &previous {
            assert!(
                index_key(ix, p, &key) == new,
                "an index key change needs the entry's removal, which this twin does not implement"
            );
            continue;
        }
        put_logical_key(tx, Box::from(&new[..]), Box::from(&[][..]))?;
    }
    Ok(())
}
pub fn fetch(tx: &mut Tx, t: &Table, k: u64) -> Result<Option<Account>, Error> {
    readable(&tx.work, &t.catalog_key, t.id)?;
    let key = logical_key(t.id, &k.to_be_bytes());
    let found = get(&tx.work.data, &key).map(|e| decode(&e.value));
    tx.reads.push(ReadRec::Point(key));
    Ok(found)
}

pub struct Account {
    pub id: u64,
    pub owner: String,
    pub balance: i64,
}
/// Schema number 0 (u16 LE), then id u64, balance i64, owner length u32 (main's codec since 07c3aa57) and the owner's bytes: 30 bytes.
pub fn encode(a: &Account) -> Vec<u8> {
    let mut b = Vec::with_capacity(30);
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&a.id.to_le_bytes());
    b.extend_from_slice(&a.balance.to_le_bytes());
    b.extend_from_slice(&u32::try_from(a.owner.len()).unwrap().to_le_bytes());
    b.extend_from_slice(a.owner.as_bytes());
    b
}
pub fn decode(b: &[u8]) -> Account {
    assert_eq!(u16::from_le_bytes(b[0..2].try_into().unwrap()), 0);
    let id = u64::from_le_bytes(b[2..10].try_into().unwrap());
    let balance = i64::from_le_bytes(b[10..18].try_into().unwrap());
    let n = usize::try_from(u32::from_le_bytes(b[18..22].try_into().unwrap())).unwrap();
    let owner = String::from_utf8(b[22..22 + n].to_vec()).unwrap();
    assert_eq!(22 + n, b.len());
    Account { id, owner, balance }
}

/// A store with table "accounts" (id 1) and `indexes` indexes named i0, i1, i2 (ids 2 onward, each
/// raising the table's schema epoch by one, as `create_index` does). Returns the Db and the Table.
pub fn open(indexes: &[fn(&Account) -> u64]) -> (Db, Table) {
    let mut ids = 0;
    let epoch = 1 + indexes.len() as u64;
    let mut catalog = vec![Arc::new(Entry {
        key: Box::from(&b"\x01accounts"[..]),
        value: catalog_value(1, 1, 0, epoch),
    })];
    let mut resident = entry(catalog[0].key.len(), catalog[0].value.len());
    let mut list = Vec::new();
    for (i, by) in indexes.iter().enumerate() {
        let id = 2 + u32::try_from(i).unwrap();
        let mut key = vec![2u8];
        key.extend_from_slice(&1u32.to_be_bytes());
        key.extend_from_slice(format!("i{i}").as_bytes());
        let e = Arc::new(Entry {
            key: key.into_boxed_slice(),
            value: catalog_value(2, id, 1, 1),
        });
        resident += entry(e.key.len(), e.value.len());
        catalog.push(e);
        list.push(Index { id, by: *by });
    }
    catalog.sort_by(|a, b| a.key.cmp(&b.key));
    let head = Roots {
        catalog: leaf(&mut ids, catalog),
        data: leaf(&mut ids, Vec::new()),
    };
    let db = Db {
        head,
        version: 0,
        resident: u64::try_from(resident).unwrap(),
        limit: 1 << 30,
        ids,
        at: 0,
    };
    let t = Table {
        id: 1,
        epoch,
        complete: true,
        catalog_key: Box::from(&b"\x01accounts"[..]),
        indexes: list,
    };
    (db, t)
}
/// The data tree's rows, checked (fan-out, order, node ids).
pub fn rows(db: &Db) -> Vec<Row> {
    let mut out = Vec::new();
    walk(&db.head.data, true, db.ids, &mut None, &mut out);
    out
}

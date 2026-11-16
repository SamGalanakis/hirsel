//! R1's target twin (STORE-SPEC v2.12 §6, Q3): the work the store must do per commit on R1's path,
//! over a persistent path-copying B+ tree with the spec's fan-out.
//!
//! Node layout. `Node::Leaf(Vec<Arc<Entry>>)` holds 16 to 32 rows in key order; a row is one
//! `Arc<Entry>`, and an `Entry` owns its key and value bytes (`Box<[u8]>` each), as std.omap's one
//! canonical `Arc[Entry[K, V]]` per row (998). `Node::Branch(Vec<Child>)` holds 16 to 32 children; a
//! `Child` is its subtree's minimum row (the same `Arc<Entry>` the leaf holds) and an `Arc<Node>`.
//! Only the root may hold fewer. A node that reaches 33 splits into 16 and 17, as std.omap's
//! `split_leaves` and `split_branches` do, so ascending puts give both trees the same depth and the
//! same path copied per put. A node is one `Arc` allocation and one vector allocation.
//!
//! What is shared. A put copies the nodes on the path from the root to the leaf (new vectors whose
//! untouched elements are one `Arc` count increment each) and nothing else: every other node, every
//! row, and the catalog tree are shared with the base version. The transaction keeps the base roots
//! until publish, so the copy is never skipped, and publish drops them, which drops the old path's
//! nodes and their counts. The op list holds the put's own row `Arc`.
//!
//! Per commit, as §3.1: one wall-clock read (`at`), the body under `catch_unwind` (isolate), the
//! resident check (§1.11, I4), publish at version + 1. No COMMIT is encoded (memory store, Q2).
//! Per put: I3's catalog check, encode, the path copy, the op-list append, and I4's account update
//! with the replaced stored entry's size returned by the insert (v2.10 T2: no second lookup). Per fetch: I3's catalog check, the lookup, the
//! `Point` record of the data-tree key, and the decode.
//!
//! Account is id u64, balance i64, owner length u64 and "owner000": 32 bytes. Values carry schema
//! number 0 (u16 LE), keys table id 1 (u32 BE) then u64 BE, as store_mem.rs.
use std::{
    io::{Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::{Instant, SystemTime},
};

type Bytes = Box<[u8]>;

struct Entry {
    key: Bytes,
    value: Bytes,
}
type Row = Arc<Entry>;
#[derive(Clone)]
struct Child {
    low: Row,
    node: Arc<Node>,
}
enum Node {
    Leaf(Vec<Row>),
    Branch(Vec<Child>),
}
const MAX: usize = 32;
const MIN: usize = 16;

enum Ins {
    One(Arc<Node>),
    Two(Arc<Node>, Child),
}

fn route(cs: &[Child], k: &[u8]) -> usize {
    cs.partition_point(|c| *c.low.key <= *k).saturating_sub(1)
}
fn get<'a>(mut n: &'a Node, k: &[u8]) -> Option<&'a Entry> {
    loop {
        match n {
            Node::Branch(cs) => n = &cs[route(cs, k)].node,
            Node::Leaf(es) => {
                return es
                    .binary_search_by(|e| (*e.key).cmp(k))
                    .ok()
                    .map(|i| &*es[i]);
            }
        }
    }
}
fn split<T>(mut xs: Vec<T>, wrap: fn(Vec<T>) -> Node, low: fn(&T) -> Row) -> Ins {
    if xs.len() <= MAX {
        return Ins::One(Arc::new(wrap(xs)));
    }
    let right = xs.split_off(MIN);
    let low = low(&right[0]);
    Ins::Two(
        Arc::new(wrap(xs)),
        Child {
            low,
            node: Arc::new(wrap(right)),
        },
    )
}
/// The new node for `n` with `e` put, and the entry it replaced. `n` is not changed.
fn insert(n: &Node, e: Row) -> (Ins, Option<Row>) {
    match n {
        Node::Leaf(es) => match es.binary_search_by(|x| (*x.key).cmp(&e.key)) {
            Ok(i) => {
                let mut copy = es.clone();
                let old = std::mem::replace(&mut copy[i], e);
                (Ins::One(Arc::new(Node::Leaf(copy))), Some(old))
            }
            Err(i) => {
                let mut copy = Vec::with_capacity(es.len() + 1);
                copy.extend_from_slice(&es[..i]);
                copy.push(e);
                copy.extend_from_slice(&es[i..]);
                (split(copy, Node::Leaf, Arc::clone), None)
            }
        },
        Node::Branch(cs) => {
            let i = route(cs, &e.key);
            let (sub, old) = insert(&cs[i].node, e);
            let ins = match sub {
                Ins::One(node) => {
                    let mut copy = cs.clone();
                    copy[i].node = node;
                    Ins::One(Arc::new(Node::Branch(copy)))
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
                    split(copy, Node::Branch, |c| c.low.clone())
                }
            };
            (ins, old)
        }
    }
}
fn low(n: &Node) -> Row {
    match n {
        Node::Leaf(es) => es[0].clone(),
        Node::Branch(cs) => cs[0].low.clone(),
    }
}
fn put_root(root: &Arc<Node>, e: Row) -> (Arc<Node>, Option<Row>) {
    match insert(root, e) {
        (Ins::One(n), old) => (n, old),
        (Ins::Two(left, right), old) => {
            let low = low(&left);
            (
                Arc::new(Node::Branch(vec![Child { low, node: left }, right])),
                old,
            )
        }
    }
}
/// Rows in key order, checking §1.2's fan-out on every node but the root (outside the timed phase).
fn walk(n: &Node, root: bool, prev: &mut Option<Row>) -> u64 {
    let len = match n {
        Node::Leaf(es) => es.len(),
        Node::Branch(cs) => cs.len(),
    };
    assert!(len <= MAX && (root || len >= MIN), "node of {len}");
    match n {
        Node::Leaf(es) => {
            for e in es {
                assert!(prev.as_ref().is_none_or(|p| p.key < e.key));
                *prev = Some(e.clone());
            }
            es.len() as u64
        }
        Node::Branch(cs) => cs.iter().map(|c| walk(&c.node, false, prev)).sum(),
    }
}

#[derive(Clone)]
struct Roots {
    data: Arc<Node>,
    catalog: Arc<Node>,
}
struct Db {
    head: Roots,
    version: u64,
    resident: u64,
    limit: u64,
}
struct Table {
    id: u32,
    catalog_key: Bytes,
}
#[derive(Debug)]
#[expect(dead_code, reason = "read by the refusal's panic message only")]
enum Error {
    Stale(u32),
    Capacity { requested: u64, available: u64 },
    Panicked,
}
#[expect(
    dead_code,
    reason = "the op list is built as the store builds it; a memory store with no follower reads it at publish only"
)]
enum Op {
    Put(Row),
}
#[expect(
    dead_code,
    reason = "the read log is returned in Done as the store returns it; R1's caller drops it"
)]
enum ReadRec {
    Point(Bytes),
}
struct Tx {
    base: Roots,
    work: Roots,
    ops: Vec<Op>,
    reads: Vec<ReadRec>,
    delta: i64,
}
struct Done<T> {
    value: T,
    version: u64,
    at: SystemTime,
    ops: usize,
    reads: Vec<ReadRec>,
}

/// §1.11: two `Bytes` handles, each one's storage rounded up to 8, and the node share.
fn entry(k: usize, v: usize) -> i64 {
    i64::try_from(62 + k.next_multiple_of(8) + v.next_multiple_of(8)).unwrap()
}
fn run<T>(db: &mut Db, body: impl FnOnce(&mut Tx) -> Result<T, Error>) -> Result<Done<T>, Error> {
    let at = SystemTime::now();
    let mut tx = Tx {
        base: db.head.clone(),
        work: db.head.clone(),
        ops: Vec::new(),
        reads: Vec::new(),
        delta: 0,
    };
    let value = catch_unwind(AssertUnwindSafe(|| body(&mut tx))).map_err(|_| Error::Panicked)??;
    let after = db.resident.checked_add_signed(tx.delta).unwrap();
    if after > db.limit {
        return Err(Error::Capacity {
            requested: after,
            available: db.limit,
        });
    }
    db.resident = after;
    db.head = tx.work;
    db.version += 1;
    drop(tx.base);
    Ok(Done {
        value,
        version: db.version,
        at,
        ops: tx.ops.len(),
        reads: tx.reads,
    })
}
/// I3: the handle's id is live in the catalog of the roots it reads.
fn live(roots: &Roots, t: &Table) -> Result<(), Error> {
    match get(&roots.catalog, &t.catalog_key) {
        Some(e) if *e.value == t.id.to_be_bytes() => Ok(()),
        _ => Err(Error::Stale(t.id)),
    }
}
fn tree_key(t: &Table, k: u64) -> Bytes {
    let mut b = [0u8; 12];
    b[..4].copy_from_slice(&t.id.to_be_bytes());
    b[4..].copy_from_slice(&k.to_be_bytes());
    Box::from(&b[..])
}
fn put(tx: &mut Tx, t: &Table, k: u64, a: &Account) -> Result<(), Error> {
    live(&tx.work, t)?;
    let key = tree_key(t, k);
    let value = encode(a).into_boxed_slice();
    let size = entry(key.len(), value.len());
    let row = Arc::new(Entry { key, value });
    tx.ops.push(Op::Put(row.clone()));
    let (data, old) = put_root(&tx.work.data, row);
    tx.work.data = data;
    tx.delta += size - old.map_or(0, |o| entry(o.key.len(), o.value.len()));
    Ok(())
}
fn fetch(tx: &mut Tx, t: &Table, k: u64) -> Result<Option<Account>, Error> {
    live(&tx.work, t)?;
    let key = tree_key(t, k);
    let found = get(&tx.work.data, &key).map(|e| decode(&e.value));
    tx.reads.push(ReadRec::Point(key));
    Ok(found)
}

struct Account {
    id: u64,
    owner: String,
    balance: i64,
}
fn encode(a: &Account) -> Vec<u8> {
    let mut b = Vec::with_capacity(34);
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&a.id.to_le_bytes());
    b.extend_from_slice(&a.balance.to_le_bytes());
    b.extend_from_slice(&u64::try_from(a.owner.len()).unwrap().to_le_bytes());
    b.extend_from_slice(a.owner.as_bytes());
    b
}
fn decode(b: &[u8]) -> Account {
    assert_eq!(u16::from_le_bytes(b[0..2].try_into().unwrap()), 0);
    let id = u64::from_le_bytes(b[2..10].try_into().unwrap());
    let balance = i64::from_le_bytes(b[10..18].try_into().unwrap());
    let n = usize::try_from(u64::from_le_bytes(b[18..26].try_into().unwrap())).unwrap();
    let owner = String::from_utf8(b[26..26 + n].to_vec()).unwrap();
    assert_eq!(26 + n, b.len());
    Account { id, owner, balance }
}
fn insert_row(db: &mut Db, t: &Table, i: u64) -> u64 {
    let done = run(db, |tx| {
        let a = Account {
            id: i,
            owner: String::from("owner000"),
            balance: i64::try_from(i).unwrap(),
        };
        put(tx, t, i, &a)?;
        let back = fetch(tx, t, i)?.unwrap();
        if back.id != i || back.balance != i64::try_from(i).unwrap() || back.owner != "owner000" {
            panic!("store R1 wrong row");
        }
        Ok(back.id + u64::try_from(back.owner.len()).unwrap())
    });
    match done {
        Ok(d) => {
            assert!(
                d.version == i + 1
                    && d.ops == 1
                    && d.reads.len() == 1
                    && d.at >= SystemTime::UNIX_EPOCH
            );
            d.value
        }
        Err(e) => panic!("store R1 refused: {e:?}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    assert_eq!(mode, "R1", "store_pcopy is R1's twin only");
    let mut gate = std::fs::File::open(args.next().unwrap()).unwrap();
    let t = Table {
        id: 1,
        catalog_key: Box::from(&b"accounts"[..]),
    };
    let catalog = Arc::new(Node::Leaf(vec![Arc::new(Entry {
        key: t.catalog_key.clone(),
        value: Box::from(&t.id.to_be_bytes()[..]),
    })]));
    let resident = u64::try_from(entry(t.catalog_key.len(), 4)).unwrap();
    let mut db = Db {
        head: Roots {
            data: Arc::new(Node::Leaf(Vec::new())),
            catalog,
        },
        version: 0,
        resident,
        limit: 1 << 30,
    };
    println!("READY");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let start = Instant::now();
    let sum: u64 = (0..100000).map(|i| insert_row(&mut db, &t, i)).sum();
    let elapsed = start.elapsed().as_nanos();
    println!("DONE {sum} {elapsed}");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    assert_eq!(db.version, 100000);
    assert_eq!(walk(&db.head.data, true, &mut None), 100000);
    assert_eq!(db.resident, resident + 100000 * 118);
}

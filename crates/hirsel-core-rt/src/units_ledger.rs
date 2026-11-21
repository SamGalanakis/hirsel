//! The unit runtime's ledger: limit records, charges, values, the lock-free
//! arrival port, owned helpers, shared nodes and boards. Connections are in
//! `units_conn.rs`.
//!
//! Every byte a unit holds is a `Charge` with exactly one payer record. A
//! record's balance changes only at an operation of the unit whose books hold
//! it, or at that unit's take of an arrival (a credit). Shared state between
//! units is limited to the arrival port, a connection's word and lock, a
//! shared node's count and a board's lock; all of their atomics and locks go
//! through `units_sync.rs`.
//!
//! Records live in their unit's books for as long as the unit's root runs; a
//! helper's books are adopted by its owner at the end take. A `RecRef` is a
//! raw pointer into some books, and every resolve checks, in O(1), that the
//! record belongs to the resolving unit's books, so a cross-unit balance touch
//! panics instead of racing.
#![allow(dead_code)]

/// Name an arrival for a harness (test builds only; the names are never built otherwise).
#[cfg(test)]
macro_rules! named {
    ($a:expr, $src:expr, $id:expr) => {
        crate::units::ledger::name_it($a, || $src, || $id)
    };
}
#[cfg(not(test))]
macro_rules! named {
    ($a:expr, $src:expr, $id:expr) => {
        $a
    };
}

#[path = "units_conn.rs"]
pub(crate) mod conn;
#[cfg(test)]
#[path = "units_ledger_law.rs"]
mod law;

use super::sync::{AtomicPtr, AtomicU64};
use conn::ConnCore;
use std::{
    cell::{Cell, UnsafeCell},
    collections::VecDeque,
    ptr::{self, NonNull},
    sync::{Arc, atomic::Ordering},
};

/// Bytes a spawn reserves for the task itself, released at the end take.
pub(crate) const TASK_FIXED: u64 = 256;
/// A primary or replacement cause cell.
pub(crate) const CELL: u64 = 32;
/// A board's root.
pub(crate) const BOARD_ROOT: u64 = 32;

/// A refused admission: the bytes asked for and what the record had left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cap {
    pub(crate) req: u64,
    pub(crate) avail: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecRef(NonNull<Rec>);
// SAFETY: a RecRef is only dereferenced by the unit whose books hold the
// record, which every resolve checks.
unsafe impl Send for RecRef {}

pub(crate) enum Kind {
    Root,
    Bound,
    HelperRoot,
    /// The non-spendable 32-byte reservation that funds a helper's primary cell.
    CellRes,
    /// A connection's room; its balance is the byte field of the connection word.
    Room(*const ConnCore),
    /// A finalizer's grant, ended when the finalizer ends.
    Grant,
}

pub(crate) struct Rec {
    limit: u64,
    used: Cell<u64>,
    parent: Option<RecRef>,
    ended: Cell<bool>,
    retired: Cell<bool>,
    spendable: bool,
    kind: Kind,
    /// The books (by their unit's port address) that hold this record.
    owner: Cell<usize>,
    #[cfg(test)]
    name: Option<String>,
}

/// One payer for `size` bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Charge {
    pub(crate) size: u64,
    pub(crate) payer: RecRef,
}

/// A pending credit: `amount` bytes back to `to` (resolved at the take).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Credit {
    pub(crate) amount: u64,
    pub(crate) to: RecRef,
}

/// What a resource's finalizer does when the resource is dropped.
#[derive(Clone, Default, Debug)]
pub(crate) struct Fin {
    pub(crate) fails: bool,
    pub(crate) fail_msg: u64,
    pub(crate) spawn_shared: u64,
    /// The node it makes is handed out (to `UnitCtx::fin_made`), not dropped.
    pub(crate) share_out: bool,
    pub(crate) keep_survivor: u64,
    pub(crate) run_lifetime: u64,
}

struct Res {
    cell: usize,
    grant: Option<usize>,
    grant_size: u64,
    fin: Option<Fin>,
}

/// An owned value: its exclusive charges, the shares it holds, and, for a
/// resource, its cause cell, grant reserve and finalizer.
pub(crate) struct Value {
    charges: Vec<Charge>,
    shared: Vec<Shared>,
    res: Option<Box<Res>>,
    #[cfg(test)]
    pub(crate) tag: u64,
}

impl Value {
    fn plain(charges: Vec<Charge>) -> Value {
        Value {
            charges,
            shared: Vec::new(),
            res: None,
            #[cfg(test)]
            tag: 0,
        }
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.charges.iter().map(|c| c.size).sum()
    }
}

// ---------------------------------------------------------------------------
// The arrival port
// ---------------------------------------------------------------------------

/// The unit is parked or about to park: the publisher that replaces this wakes it.
const WAITING: *mut Arrival = std::ptr::without_provenance_mut(1);
/// The unit ended and its owner took the end: re-publish to `fwd`.
const FORWARD: *mut Arrival = std::ptr::without_provenance_mut(2);
/// The run root ended: drop the arrival.
const DEAD: *mut Arrival = std::ptr::without_provenance_mut(3);

fn marker(p: *mut Arrival) -> bool {
    (p as usize) <= 3
}

/// A unit's inbox: an intrusive stack of arrivals pushed with one
/// compare-exchange each and taken whole with one swap.
pub(crate) struct Port {
    head: AtomicPtr<Arrival>,
    fwd: AtomicPtr<Port>,
    /// The pool unit to wake when a publish replaces `WAITING`.
    wake: AtomicU64,
}

// SAFETY: the port's fields are atomics; the arrivals it owns are Send.
unsafe impl Send for Port {}
unsafe impl Sync for Port {}

impl Port {
    pub(crate) fn new() -> Arc<Port> {
        Arc::new(Port {
            head: AtomicPtr::new(ptr::null_mut()),
            fwd: AtomicPtr::new(ptr::null_mut()),
            wake: AtomicU64::new(0),
        })
    }

    pub(crate) fn set_wake(&self, unit: u32) {
        self.wake.store(unit as u64 + 1, Ordering::Release);
    }

    /// Push one arrival. Wakes the unit only if it was parked on the port.
    pub(crate) fn publish(&self, a: Box<Arrival>) {
        let a = Box::into_raw(a);
        let mut h = self.head.load(Ordering::Acquire);
        loop {
            if h == FORWARD {
                // SAFETY: `fwd` is stored before FORWARD and never changes after.
                let f = unsafe { &*self.fwd.load(Ordering::Acquire) };
                // SAFETY: `a` came from Box::into_raw above and was never shared.
                let a = unsafe { Box::from_raw(a) };
                if a.body.forwarded() {
                    f.publish(a);
                }
                return;
            }
            if h == DEAD {
                // SAFETY: as above.
                drop(unsafe { Box::from_raw(a) });
                return;
            }
            // SAFETY: `a` is not yet visible to any other thread.
            unsafe { (*a).next = if h == WAITING { ptr::null_mut() } else { h } };
            match self
                .head
                .compare_exchange(h, a, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    if h == WAITING {
                        let u = self.wake.load(Ordering::Acquire);
                        if u != 0 {
                            super::wake_id((u - 1) as u32);
                        }
                    }
                    return;
                }
                Err(x) => h = x,
            }
        }
    }

    /// Take every published arrival, oldest first.
    fn drain(&self, into: &mut VecDeque<Box<Arrival>>) {
        let h = self.head.load(Ordering::Acquire);
        if h.is_null() || marker(h) {
            return;
        }
        let mut p = self.head.swap(ptr::null_mut(), Ordering::Acquire);
        let mut list = Vec::new();
        while !p.is_null() {
            // SAFETY: every node in the list was published with Box::into_raw
            // and is owned by the taker after the swap.
            let a = unsafe { Box::from_raw(p) };
            p = a.next;
            list.push(a);
        }
        into.extend(list.into_iter().rev());
    }

    /// Install a marker, returning what was published before it.
    fn close_with(&self, m: *mut Arrival, into: &mut VecDeque<Box<Arrival>>) {
        let mut p = self.head.swap(m, Ordering::AcqRel);
        if marker(p) {
            return;
        }
        let mut list = Vec::new();
        while !p.is_null() {
            // SAFETY: as in `drain`.
            let a = unsafe { Box::from_raw(p) };
            p = a.next;
            list.push(a);
        }
        into.extend(list.into_iter().rev());
    }

    /// Arm the port before parking. False if something is already published.
    pub(crate) fn arm(&self) -> bool {
        self.head
            .compare_exchange(
                ptr::null_mut(),
                WAITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
            || self.head.load(Ordering::Acquire) == WAITING
    }

    /// Disarm after a wake or a timeout; true if nothing was published.
    pub(crate) fn disarm(&self) -> bool {
        self.head
            .compare_exchange(
                WAITING,
                ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        let mut rest = VecDeque::new();
        self.close_with(DEAD, &mut rest);
        let f = self.fwd.load(Ordering::Acquire);
        if !f.is_null() {
            // SAFETY: stored from Arc::into_raw in `forward_to`.
            drop(unsafe { Arc::from_raw(f) });
        }
    }
}

/// One arrival. The payload is written once by the publisher and read in
/// place by the taker.
pub(crate) struct Arrival {
    next: *mut Arrival,
    pub(crate) body: Body,
    #[cfg(test)]
    pub(crate) names: Option<Box<Names>>,
}

// SAFETY: an arrival is owned by exactly one port or one unit at a time.
unsafe impl Send for Arrival {}

#[cfg(test)]
pub(crate) struct Names {
    pub(crate) src: String,
    pub(crate) ident: String,
    /// What the arrival carried, written at its take (for the checker's
    /// determinism law: an arrival's content is part of its taker's input).
    pub(crate) content: String,
}

/// Name arrivals (test builds, when a harness asks for it).
#[cfg(test)]
pub(crate) static NAMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn naming() -> bool {
    NAMING.load(Ordering::Relaxed)
}

pub(crate) enum Settle {
    Accepted(Vec<Credit>),
    Closed(Value),
    Withdrawn(Value),
}

pub(crate) enum Body {
    /// A shared node's or board's last release.
    Credits(Vec<Credit>),
    /// A connection's retirement: its retained header and unused room.
    LastRelease(Credit),
    /// A helper's end, carrying the whole helper.
    HelperEnd(HelperEnd),
    /// The settlement of a waiting send, to its sender.
    Settlement(u32, Settle),
    Item(*const ConnCore, Value),
    Close(*const ConnCore),
    /// Made by the owner itself, after the items accepted before the seal.
    SealAnswer(*const ConnCore),
}

impl Body {
    #[cfg(test)]
    fn content(&self) -> String {
        let amounts = |cs: &[Credit]| cs.iter().map(|c| c.amount.to_string()).collect::<Vec<_>>().join(",");
        match self {
            Body::Credits(cs) => amounts(cs),
            Body::LastRelease(c) => c.amount.to_string(),
            // SAFETY: the end arrival owns the ended helper's context.
            Body::HelperEnd(e) => {
                let h = unsafe { e.0.as_ref() };
                let info = h.helper.as_ref().unwrap();
                format!("root {} ret {}", h.used(info.root), info.ret.as_ref().map_or(0, |v| v.bytes()))
            }
            Body::Settlement(_, Settle::Accepted(cs)) => amounts(cs),
            Body::Settlement(_, Settle::Closed(v) | Settle::Withdrawn(v)) | Body::Item(_, v) => {
                format!("{} bytes", v.bytes())
            }
            Body::Close(_) | Body::SealAnswer(_) => String::new(),
        }
    }

    /// Forwarded from an ended helper to its owner: credits and notices only.
    fn forwarded(&self) -> bool {
        matches!(self, Body::Credits(_) | Body::LastRelease(_))
    }
}

pub(crate) struct HelperEnd(NonNull<UnitCtx>);
// SAFETY: the helper's context moves to its owner with the arrival.
unsafe impl Send for HelperEnd {}
impl Drop for HelperEnd {
    fn drop(&mut self) {
        // SAFETY: made by Box::into_raw in `end_helper`; taken at most once
        // (the end take forgets it).
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl Arrival {
    pub(crate) fn new(body: Body) -> Box<Arrival> {
        Box::new(Arrival {
            next: ptr::null_mut(),
            body,
            #[cfg(test)]
            names: None,
        })
    }
}

#[cfg(test)]
pub(crate) fn name_it(mut a: Box<Arrival>, src: impl FnOnce() -> String, ident: impl FnOnce() -> String) -> Box<Arrival> {
    if naming() {
        a.names = Some(Box::new(Names {
            src: src(),
            ident: ident(),
            content: String::new(),
        }));
    }
    a
}

// ---------------------------------------------------------------------------
// Shared nodes and boards
// ---------------------------------------------------------------------------

struct Node {
    count: AtomicU64,
    charge: Charge,
    payload: UnsafeCell<(Vec<Charge>, Vec<Shared>)>,
    home: Arc<Port>,
    #[cfg(test)]
    label: String,
}

/// One counted share of a shared node.
pub(crate) struct Shared(NonNull<Node>);
// SAFETY: the node's count is atomic and its payload is touched only by the
// last releaser.
unsafe impl Send for Shared {}

impl Shared {
    pub(crate) fn share(&self) -> Shared {
        // SAFETY: a live share keeps the node alive.
        unsafe { self.0.as_ref() }.count.fetch_add(1, Ordering::Relaxed);
        Shared(self.0)
    }
    pub(crate) fn same(&self, o: &Shared) -> bool {
        self.0 == o.0
    }
}

pub(crate) struct BoardCore {
    root: Charge,
    home: Arc<Port>,
    st: super::sync::Mutex<BoardSt>,
    #[cfg(test)]
    label: String,
}
struct BoardSt {
    cur: Option<Shared>,
    poster: bool,
    handles: u64,
}
// SAFETY: the state is behind the lock; the root charge is read only at release.
unsafe impl Send for BoardCore {}
unsafe impl Sync for BoardCore {}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

struct Handle {
    fixed: Charge,
    result: Charge,
    state: HState,
}
enum HState {
    Running,
    Ended(Option<Value>),
    Consumed,
}

struct HelperInfo {
    k: usize,
    root: RecRef,
    cell_res: RecRef,
    owner: Arc<Port>,
    ret: Option<Value>,
}

/// What a take did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Took {
    Credits,
    LastRelease,
    HelperEnd,
    Accepted(u32),
    Closed(u32),
    Withdrawn(u32),
    Item,
    Close,
    SealAnswer,
}

/// One unit's runtime state, touched only by the thread running the unit.
pub(crate) struct UnitCtx {
    pub(crate) port: Arc<Port>,
    /// Boxed: a `RecRef` is the record's address, kept while the book grows.
    #[allow(clippy::vec_box)]
    books: Vec<Box<Rec>>,
    stack: Vec<RecRef>,
    reached: VecDeque<Box<Arrival>>,
    /// Test hook: arrivals published to the unit wait here until the harness
    /// names the source whose oldest one reaches the unit next.
    #[cfg(test)]
    held: Option<VecDeque<Box<Arrival>>>,
    helpers: Vec<Option<Handle>>,
    helper: Option<HelperInfo>,
    /// The active primary cell (helpers only).
    cell: Option<Charge>,
    settled: Vec<(u32, Value)>,
    run_lifetime: Vec<Charge>,
    next_conn: u32,
    next_spawn: u32,
    next_shared: u32,
    next_board: u32,
    next_send: u32,
    next_bound: u32,
    /// Nodes a finalizer made in this unit, in order: handed out, or None if
    /// it dropped them itself.
    pub(crate) fin_made: Vec<Option<Shared>>,
    #[cfg(test)]
    pub(crate) label: String,
}

// SAFETY: a context is used by one thread at a time; it moves between threads
// only whole (a helper's end).
unsafe impl Send for UnitCtx {}

impl UnitCtx {
    fn blank(#[allow(unused)] label: String) -> UnitCtx {
        UnitCtx {
            port: Port::new(),
            books: Vec::new(),
            stack: Vec::new(),
            reached: VecDeque::new(),
            #[cfg(test)]
            held: None,
            helpers: Vec::new(),
            helper: None,
            cell: None,
            settled: Vec::new(),
            run_lifetime: Vec::new(),
            next_conn: 0,
            next_spawn: 0,
            next_shared: 0,
            next_board: 0,
            next_send: 0,
            next_bound: 0,
            fin_made: Vec::new(),
            #[cfg(test)]
            label,
        }
    }

    /// A run root with `limit` bytes.
    pub(crate) fn root(limit: u64) -> UnitCtx {
        Self::root_as(limit, "U0")
    }

    /// A run root labelled `label` (labels name records and arrivals in
    /// test builds; elsewhere they are dropped).
    pub(crate) fn root_as(limit: u64, #[allow(unused)] label: &str) -> UnitCtx {
        #[cfg(test)]
        let mut u = UnitCtx::blank(label.into());
        #[cfg(not(test))]
        let mut u = UnitCtx::blank(String::new());
        let r = u.new_rec(limit, None, Kind::Root, true, Some(label.into()));
        u.stack.push(r);
        u
    }

    /// Bind the unit to the pool unit `id`: a publish to a parked port wakes it.
    pub(crate) fn bind(&self, id: u32) {
        self.port.set_wake(id);
    }

    /// Take the next arrival, parking the pool unit until one is published.
    /// Must run on the bound pool unit.
    pub(crate) fn wait(&mut self) -> (Took, Box<Arrival>) {
        loop {
            if let Some(t) = self.take() {
                return t;
            }
            if self.port.arm() {
                super::park(None);
                self.port.disarm();
            }
        }
    }

    /// The balance of the record that admits now (its `used`).
    pub(crate) fn current_used(&self) -> u64 {
        self.used(self.cur())
    }

    fn tag(&self) -> usize {
        Arc::as_ptr(&self.port) as usize
    }

    fn new_rec(
        &mut self,
        limit: u64,
        parent: Option<RecRef>,
        kind: Kind,
        spendable: bool,
        #[allow(unused)] name: Option<String>,
    ) -> RecRef {
        let mut b = Box::new(Rec {
            limit,
            used: Cell::new(0),
            parent,
            ended: Cell::new(false),
            retired: Cell::new(false),
            spendable,
            kind,
            owner: Cell::new(self.tag()),
            #[cfg(test)]
            name,
        });
        let r = RecRef(NonNull::from(&mut *b));
        self.books.push(b);
        r
    }

    fn rec(&self, r: RecRef) -> &Rec {
        // SAFETY: records are freed only with their books; the owner check
        // below makes a reference into another unit's books a panic.
        let rec = unsafe { r.0.as_ref() };
        assert_eq!(
            rec.owner.get(),
            self.tag(),
            "a balance touched outside its unit's books"
        );
        rec
    }

    fn resolve(&self, mut r: RecRef) -> RecRef {
        loop {
            let rec = self.rec(r);
            if rec.ended.get() {
                r = rec.parent.expect("an ended record without a parent");
            } else {
                return r;
            }
        }
    }

    fn used(&self, r: RecRef) -> u64 {
        let rec = self.rec(r);
        match rec.kind {
            // SAFETY: a live room's connection is held by its owner's inbox.
            Kind::Room(c) => unsafe { &*c }.room_used(),
            _ => rec.used.get(),
        }
    }

    fn add_used(&self, r: RecRef, n: u64) {
        let rec = self.rec(r);
        match rec.kind {
            // SAFETY: as in `used`.
            Kind::Room(c) => unsafe { &*c }.room_add(n),
            _ => {
                let u = rec.used.get() + n;
                assert!(u <= rec.limit, "record over its limit");
                rec.used.set(u);
            }
        }
    }

    fn sub_used(&self, r: RecRef, n: u64) {
        let rec = self.rec(r);
        match rec.kind {
            // SAFETY: as in `used`.
            Kind::Room(c) => unsafe { &*c }.room_sub(n),
            _ => rec.used.set(
                rec.used
                    .get()
                    .checked_sub(n)
                    .expect("a negative balance"),
            ),
        }
    }

    fn refund(&self, c: Charge) {
        let r = self.resolve(c.payer);
        self.sub_used(r, c.size);
    }

    fn apply(&self, c: Credit) {
        let r = self.resolve(c.to);
        self.sub_used(r, c.amount);
    }

    /// Hand a record over: its unused bytes go back to its parent, and it
    /// joins the parent for every later charge and credit.
    fn end_rec(&self, r: RecRef) {
        let rec = self.rec(r);
        assert!(!rec.ended.get(), "a record ended twice");
        let unused = rec.limit - self.used(r);
        let p = self.resolve(rec.parent.expect("a root cannot be handed over"));
        self.sub_used(p, unused);
        rec.ended.set(true);
    }

    fn cur(&self) -> RecRef {
        *self.stack.last().unwrap()
    }

    /// Admit `n` bytes into the current record.
    fn admit(&self, n: u64) -> Result<RecRef, Cap> {
        let r = self.cur();
        let rec = self.rec(r);
        let used = rec.used.get();
        if !rec.spendable || used + n > rec.limit {
            return Err(Cap {
                req: n,
                avail: rec.limit - used,
            });
        }
        rec.used.set(used + n);
        Ok(r)
    }

    // -- local operations -------------------------------------------------

    pub(crate) fn probe(&self) -> u64 {
        let rec = self.rec(self.cur());
        rec.limit - rec.used.get()
    }

    pub(crate) fn alloc_block(&mut self, size: u64) -> Result<Value, Cap> {
        let r = self.admit(size)?;
        Ok(Value::plain(vec![Charge { size, payer: r }]))
    }

    pub(crate) fn alloc_run_lifetime(&mut self, size: u64) -> Result<(), Cap> {
        let r = self.admit(size)?;
        self.run_lifetime.push(Charge { size, payer: r });
        Ok(())
    }

    pub(crate) fn make_resource(
        &mut self,
        fields: u64,
        grant: u64,
        fin: Option<Fin>,
    ) -> Result<Value, Cap> {
        let r = self.admit(fields + CELL + grant)?;
        let mut charges = Vec::new();
        if fields > 0 {
            charges.push(Charge {
                size: fields,
                payer: r,
            });
        }
        let cell = charges.len();
        charges.push(Charge {
            size: CELL,
            payer: r,
        });
        let g = (grant > 0).then(|| {
            charges.push(Charge {
                size: grant,
                payer: r,
            });
            charges.len() - 1
        });
        let mut v = Value::plain(charges);
        v.res = Some(Box::new(Res {
            cell,
            grant: g,
            grant_size: grant,
            fin,
        }));
        Ok(v)
    }

    /// One value holding all of these (no charge; a resource's finalizer is lost).
    pub(crate) fn combine(&mut self, vals: Vec<Value>) -> Value {
        let mut out = Value::plain(Vec::new());
        for v in vals {
            out.charges.extend(v.charges);
            out.shared.extend(v.shared);
        }
        out
    }

    /// Drop a value. A resource's finalizer runs inside a grant record; a
    /// failing one returns the cause chain, a passing one its survivors.
    pub(crate) fn drop_value(&mut self, v: Value) -> Option<Value> {
        let Value {
            charges,
            shared,
            res,
            ..
        } = v;
        for s in shared {
            self.drop_shared(s);
        }
        let Some((res, fin)) = res.and_then(|r| {
            let r = *r;
            r.fin.clone().map(|f| (r, f))
        }) else {
            for c in charges {
                self.refund(c);
            }
            return None;
        };
        let cell = charges[res.cell];
        let home = self.resolve(cell.payer);
        let grant = (res.grant_size > 0)
            .then(|| self.new_rec(res.grant_size, Some(home), Kind::Grant, true, None));
        let in_grant = |me: &Self, n: u64| -> Charge {
            let g = grant.expect("a finalizer charge without a grant");
            me.add_used(g, n);
            Charge { size: n, payer: g }
        };
        let mut survivors = Vec::new();
        if fin.spawn_shared > 0 {
            let c = in_grant(self, fin.spawn_shared);
            let s = self.new_node(c, Vec::new(), Vec::new());
            if fin.share_out {
                self.fin_made.push(Some(s));
            } else {
                self.fin_made.push(None);
                self.drop_shared(s);
            }
        }
        if fin.run_lifetime > 0 {
            let c = in_grant(self, fin.run_lifetime);
            self.run_lifetime.push(c);
        }
        if fin.keep_survivor > 0 {
            survivors.push(in_grant(self, fin.keep_survivor));
        }
        let msg = (fin.fails && fin.fail_msg > 0).then(|| in_grant(self, fin.fail_msg));
        if let Some(g) = grant {
            self.end_rec(g);
        }
        for (i, c) in charges.iter().enumerate() {
            if i != res.cell && Some(i) != res.grant {
                self.refund(*c);
            }
        }
        if fin.fails {
            let mut cs = vec![cell];
            cs.extend(msg);
            cs.extend(survivors);
            Some(Value::plain(cs))
        } else {
            self.refund(cell);
            (!survivors.is_empty()).then(|| Value::plain(survivors))
        }
    }

    pub(crate) fn enter_bound(&mut self, size: u64) -> Result<(), Cap> {
        let p = self.admit(size)?;
        let k = self.next_bound;
        self.next_bound += 1;
        #[cfg(test)]
        let name = Some(format!("{}/b{k}", self.label));
        #[cfg(not(test))]
        let name = {
            let _ = k;
            None
        };
        let r = self.new_rec(size, Some(p), Kind::Bound, true, name);
        self.stack.push(r);
        Ok(())
    }

    pub(crate) fn exit_bound(&mut self) {
        assert!(self.stack.len() > 1, "cannot exit the root record");
        let r = self.stack.pop().unwrap();
        self.end_rec(r);
    }

    /// Catch a failure inside an isolate: a replacement cell is admitted;
    /// in a helper, the active primary cell is caught and the replacement
    /// becomes the primary cell.
    pub(crate) fn isolate_fail(&mut self) -> Result<Value, Cap> {
        let r = self.admit(CELL)?;
        let new = Charge {
            size: CELL,
            payer: r,
        };
        let caught = match self.cell.as_mut() {
            Some(c) => std::mem::replace(c, new),
            None => new,
        };
        Ok(Value::plain(vec![caught]))
    }

    // -- helpers --------------------------------------------------------------

    /// Spawn an owned helper with limit `n` and an inline result slot of `r`
    /// bytes. The argument's blocks move into the helper's root.
    #[allow(clippy::type_complexity)]
    pub(crate) fn spawn_helper(
        &mut self,
        n: u64,
        r: u64,
        arg: Option<Value>,
    ) -> Result<(usize, Box<UnitCtx>, Option<Value>), (Cap, Option<Value>)> {
        let t = TASK_FIXED + r + CELL + n;
        let cur = self.cur();
        {
            let rec = self.rec(cur);
            let used = rec.used.get();
            if !rec.spendable || used + t > rec.limit {
                return Err((
                    Cap {
                        req: t,
                        avail: rec.limit - used,
                    },
                    arg,
                ));
            }
        }
        let ab = arg.as_ref().map_or(0, Value::bytes);
        if ab > n {
            return Err((Cap { req: ab, avail: n }, arg));
        }
        self.add_used(cur, t);
        let k = self.helpers.len();
        let spawn = self.next_spawn;
        self.next_spawn += 1;
        #[cfg(test)]
        let label = format!("{}.H{spawn}", self.label);
        #[cfg(not(test))]
        let label = {
            let _ = spawn;
            String::new()
        };
        let mut h = Box::new(UnitCtx::blank(label.clone()));
        let root = h.new_rec(n, Some(cur), Kind::HelperRoot, true, Some(label.clone()));
        let cell_res = h.new_rec(CELL, Some(cur), Kind::CellRes, false, Some(format!("{label}/cell")));
        h.rec(cell_res).used.set(CELL);
        h.cell = Some(Charge {
            size: CELL,
            payer: cell_res,
        });
        h.stack.push(root);
        let arg = arg.map(|mut v| {
            for c in &mut v.charges {
                self.refund(*c);
                c.payer = root;
                h.add_used(root, c.size);
            }
            v
        });
        h.helper = Some(HelperInfo {
            k,
            root,
            cell_res,
            owner: self.port.clone(),
            ret: None,
        });
        self.helpers.push(Some(Handle {
            fixed: Charge {
                size: TASK_FIXED,
                payer: cur,
            },
            result: Charge { size: r, payer: cur },
            state: HState::Running,
        }));
        Ok((k, h, arg))
    }

    /// End a helper: its unused primary cell goes back to the cell
    /// reservation (or joins the returned cause chain), and the whole helper
    /// travels to its owner as one arrival.
    /// End a helper. Publishing its end hands the context to the owner, so
    /// this is the helper's last touch of it; the pointer returned may be
    /// read only by a caller that knows the end has not been taken (the
    /// single-threaded ledger law).
    pub(crate) fn end_helper(mut self: Box<Self>, ret: Option<Value>, with_cell: bool) -> NonNull<UnitCtx> {
        let cell = self.cell.take().expect("only a helper ends this way");
        let ret = if with_cell {
            Some(match ret {
                Some(mut v) => {
                    v.charges.push(cell);
                    v
                }
                None => Value::plain(vec![cell]),
            })
        } else {
            self.refund(cell);
            ret
        };
        let info = self.helper.as_mut().unwrap();
        info.ret = ret;
        let owner = info.owner.clone();
        #[cfg(test)]
        let label = self.label.clone();
        // The helper's pool unit ends now: no publish may wake it again.
        self.port.wake.store(0, Ordering::Release);
        self.port.disarm();
        let p = NonNull::from(Box::leak(self));
        let a = named!(
            Arrival::new(Body::HelperEnd(HelperEnd(p))),
            format!("helper:{label}"),
            format!("helper_end:{label}")
        );
        owner.publish(a);
        p
    }

    /// The end take: release the task's 256 bytes, adopt the helper's books,
    /// hand over its cell reservation and root, then forward the credits and
    /// notices it had, in the order they reached it, then those still in its
    /// port.
    fn end_take(&mut self, e: HelperEnd) {
        let p = e.0;
        std::mem::forget(e);
        // SAFETY: made by Box::into_raw in `end_helper`; this take owns it.
        let mut h = unsafe { Box::from_raw(p.as_ptr()) };
        let info = h.helper.take().unwrap();
        let fixed = {
            let hd = self.helpers[info.k].as_ref().unwrap();
            assert!(matches!(hd.state, HState::Running), "a helper end taken twice");
            hd.fixed
        };
        self.refund(fixed);
        let tag = self.tag();
        for r in h.books.drain(..) {
            r.owner.set(tag);
            self.books.push(r);
        }
        self.end_rec(info.cell_res);
        self.end_rec(info.root);
        // Forward.
        let raw = Arc::into_raw(self.port.clone()) as *mut Port;
        let old = h.port.fwd.swap(raw, Ordering::Release);
        debug_assert!(old.is_null());
        let mut rest = VecDeque::new();
        #[cfg(test)]
        if let Some(held) = h.held.take() {
            rest.extend(held);
        }
        h.port.close_with(FORWARD, &mut rest);
        for a in h.reached.drain(..) {
            #[cfg(test)]
            let mut a = a;
            if a.body.forwarded() {
                #[cfg(test)]
                if let Some(n) = a.names.as_mut() {
                    n.src = format!("fwd:{}", h.label);
                }
                self.layer(a);
            }
        }
        for a in rest {
            if a.body.forwarded() {
                self.layer(a);
            }
        }
        self.run_lifetime.append(&mut h.run_lifetime);
        self.helpers[info.k].as_mut().unwrap().state = HState::Ended(info.ret);
    }

    /// Join (`drop_result` false) or abandon a helper whose end was taken:
    /// releases its result slot and returns or drops the result.
    pub(crate) fn consume_helper(&mut self, k: usize, drop_result: bool) -> Option<Value> {
        let hd = self.helpers[k].as_mut().unwrap();
        let HState::Ended(ret) = std::mem::replace(&mut hd.state, HState::Consumed) else {
            panic!("a helper consumed before its end was taken, or twice");
        };
        let result = hd.result;
        self.refund(result);
        match ret {
            Some(v) if drop_result => {
                self.drop_value(v);
                None
            }
            r => r,
        }
    }

    // -- shared nodes and boards ----------------------------------------------

    fn new_node(&mut self, charge: Charge, payload: Vec<Charge>, nested: Vec<Shared>) -> Shared {
        let s = self.next_shared;
        self.next_shared += 1;
        let n = Box::new(Node {
            count: AtomicU64::new(1),
            charge,
            payload: UnsafeCell::new((payload, nested)),
            home: self.port.clone(),
            #[cfg(test)]
            label: format!("{}.S{s}", self.label),
        });
        let _ = s;
        Shared(NonNull::from(Box::leak(n)))
    }

    pub(crate) fn make_shared(&mut self, size: u64, payload: Option<Value>) -> Result<Shared, (Cap, Option<Value>)> {
        let r = match self.admit(size) {
            Ok(r) => r,
            Err(c) => return Err((c, payload)),
        };
        let (p, nested) = payload.map_or((Vec::new(), Vec::new()), |v| (v.charges, v.shared));
        Ok(self.new_node(Charge { size, payer: r }, p, nested))
    }

    /// Release one share. The last release releases the nested nodes, then
    /// sends one credits arrival home, even when home is this unit.
    pub(crate) fn drop_shared(&mut self, s: Shared) {
        let p = s.0;
        // SAFETY: the share keeps the node alive until this count drops.
        if unsafe { p.as_ref() }.count.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        // SAFETY: the last share owns the node.
        let n = unsafe { Box::from_raw(p.as_ptr()) };
        let (payload, nested) = n.payload.into_inner();
        for s in nested {
            self.drop_shared(s);
        }
        let mut cs = Vec::with_capacity(1 + payload.len());
        cs.push(Credit {
            amount: n.charge.size,
            to: n.charge.payer,
        });
        cs.extend(payload.iter().map(|c| Credit {
            amount: c.size,
            to: c.payer,
        }));
        #[cfg(test)]
        let label = n.label.clone();
        let a = named!(
            Arrival::new(Body::Credits(cs)),
            format!("shared:{label}"),
            format!("credits:{label}")
        );
        n.home.publish(a);
    }

    pub(crate) fn make_board(&mut self, init: Shared) -> Result<Arc<BoardCore>, (Cap, Shared)> {
        let r = match self.admit(BOARD_ROOT) {
            Ok(r) => r,
            Err(c) => return Err((c, init)),
        };
        let b = self.next_board;
        self.next_board += 1;
        let _ = b;
        Ok(Arc::new(BoardCore {
            root: Charge {
                size: BOARD_ROOT,
                payer: r,
            },
            home: self.port.clone(),
            st: super::sync::Mutex::new(BoardSt {
                cur: Some(init),
                poster: true,
                handles: 1,
            }),
            #[cfg(test)]
            label: format!("{}.B{b}", self.label),
        }))
    }

    pub(crate) fn board_add_handle(b: &BoardCore) {
        b.st.lock().unwrap().handles += 1;
    }

    pub(crate) fn board_read(&self, b: &BoardCore) -> Shared {
        b.st.lock().unwrap().cur.as_ref().expect("a released board").share()
    }

    pub(crate) fn board_post(&mut self, b: &BoardCore, s: Shared) {
        let old = {
            let mut st = b.st.lock().unwrap();
            assert!(st.poster);
            st.cur.replace(s)
        };
        if let Some(o) = old {
            self.drop_shared(o);
        }
    }

    pub(crate) fn board_drop_poster(&mut self, b: &BoardCore) {
        let rel = {
            let mut st = b.st.lock().unwrap();
            assert!(st.poster);
            st.poster = false;
            (st.handles == 0).then(|| st.cur.take()).flatten()
        };
        self.board_release(b, rel);
    }

    pub(crate) fn board_drop_handle(&mut self, b: &BoardCore) {
        let rel = {
            let mut st = b.st.lock().unwrap();
            st.handles -= 1;
            (st.handles == 0 && !st.poster).then(|| st.cur.take()).flatten()
        };
        self.board_release(b, rel);
    }

    fn board_release(&mut self, b: &BoardCore, cur: Option<Shared>) {
        let Some(s) = cur else { return };
        self.drop_shared(s);
        #[cfg(test)]
        let label = b.label.clone();
        let a = named!(
            Arrival::new(Body::Credits(vec![Credit {
            amount: b.root.size,
            to: b.root.payer,
        }])),
            format!("board:{label}"),
            format!("credits:{label}")
        );
        b.home.publish(a);
    }

    // -- arrivals -------------------------------------------------------------

    /// Where an arrival goes once it has reached this unit.
    fn layer(&mut self, a: Box<Arrival>) {
        #[cfg(test)]
        if let Some(h) = self.held.as_mut() {
            h.push_back(a);
            return;
        }
        self.reached.push_back(a);
    }

    /// Move everything published to the port into the unit. Items are
    /// numbered here, and a seal answer is made once the items accepted
    /// before the seal have all come in.
    pub(crate) fn intake(&mut self) {
        let mut list = VecDeque::new();
        self.port.drain(&mut list);
        for a in list {
            self.intake_one(a);
        }
    }

    fn intake_one(&mut self, #[allow(unused_mut)] mut a: Box<Arrival>) {
        if let Body::Item(c, _) = a.body {
            // SAFETY: an item's connection is held by its owner, this unit.
            let c = unsafe { &*c };
            let side = c.side();
            let seq = side.intaken;
            side.intaken += 1;
            #[cfg(test)]
            if let Some(n) = a.names.as_mut() {
                n.ident = format!("item:{}#{seq}", c.label);
            }
            let _ = seq;
            self.layer(a);
            if side.seal_due > 0 {
                side.seal_due -= 1;
                if side.seal_due == 0 {
                    self.seal_answer(c);
                }
            }
            return;
        }
        self.layer(a);
    }

    /// Test hook: the oldest held arrival from `src` reaches the unit.
    #[cfg(test)]
    pub(crate) fn reach(&mut self, src: &str) -> Option<String> {
        if self.held.is_none() {
            self.held = Some(VecDeque::new());
        }
        // An intake can publish a seal answer to this port: drain until empty.
        loop {
            let mut list = VecDeque::new();
            self.port.drain(&mut list);
            if list.is_empty() {
                break;
            }
            for a in list {
                self.intake_one(a);
            }
        }
        let h = self.held.as_mut().unwrap();
        let i = h
            .iter()
            .position(|a| a.names.as_ref().is_some_and(|n| n.src == src))?;
        let a = h.remove(i).unwrap();
        let id = a.names.as_ref().unwrap().ident.clone();
        self.reached.push_back(a);
        Some(id)
    }

    #[cfg(test)]
    pub(crate) fn hook(&mut self) {
        self.held.get_or_insert_with(VecDeque::new);
    }

    #[cfg(test)]
    pub(crate) fn hooked(&self) -> bool {
        self.held.is_some()
    }

    /// Take the next arrival that has reached the unit and apply it.
    pub(crate) fn take(&mut self) -> Option<(Took, Box<Arrival>)> {
        #[cfg(test)]
        let hooked = self.held.is_some();
        #[cfg(not(test))]
        let hooked = false;
        if self.reached.is_empty() && !hooked {
            self.intake();
        }
        let mut a = self.reached.pop_front()?;
        #[cfg(test)]
        if let Some(n) = a.names.as_mut() {
            n.content = a.body.content();
        }
        let body = std::mem::replace(&mut a.body, Body::Credits(Vec::new()));
        let t = match body {
            Body::Credits(cs) => {
                for c in cs {
                    self.apply(c);
                }
                Took::Credits
            }
            Body::LastRelease(c) => {
                self.apply(c);
                Took::LastRelease
            }
            Body::HelperEnd(e) => {
                self.end_take(e);
                Took::HelperEnd
            }
            Body::Settlement(id, Settle::Accepted(cs)) => {
                for c in cs {
                    self.apply(c);
                }
                Took::Accepted(id)
            }
            Body::Settlement(id, Settle::Closed(v)) => {
                self.settled.push((id, v));
                Took::Closed(id)
            }
            Body::Settlement(id, Settle::Withdrawn(v)) => {
                self.drop_value(v);
                Took::Withdrawn(id)
            }
            Body::Item(c, v) => {
                // SAFETY: as in `intake_one`.
                unsafe { &*c }.side().taken.push_back(v);
                Took::Item
            }
            Body::Close(c) => {
                // SAFETY: as in `intake_one`.
                unsafe { &*c }.side().closed = true;
                Took::Close
            }
            Body::SealAnswer(c) => {
                // SAFETY: as in `intake_one`.
                unsafe { &*c }.side().seal_taken = true;
                Took::SealAnswer
            }
        };
        Some((t, a))
    }

    /// The value of a waiting send settled `Closed`, back to its sender.
    pub(crate) fn closed_value(&mut self, id: u32) -> Option<Value> {
        let i = self.settled.iter().position(|(s, _)| *s == id)?;
        Some(self.settled.swap_remove(i).1)
    }

    /// End the run root: its record retires with its run-lifetime storage,
    /// and every later arrival to it is dropped.
    pub(crate) fn end_root(&mut self) {
        let root = self.stack[0];
        self.rec(root).retired.set(true);
        self.run_lifetime.clear();
        let mut rest = VecDeque::new();
        self.port.close_with(DEAD, &mut rest);
        self.reached.clear();
        #[cfg(test)]
        {
            self.held = None;
        }
    }

    /// Every live, named record: `name=used/limit`.
    #[cfg(test)]
    pub(crate) fn snapshot(&self, out: &mut Vec<String>) {
        for r in &self.books {
            if r.ended.get() || r.retired.get() || matches!(r.kind, Kind::Grant) {
                continue;
            }
            let used = match r.kind {
                // SAFETY: as in `used`.
                Kind::Room(c) => unsafe { &*c }.room_used(),
                _ => r.used.get(),
            };
            out.push(format!("{}={used}/{}", r.name.as_ref().unwrap(), r.limit));
        }
    }
}

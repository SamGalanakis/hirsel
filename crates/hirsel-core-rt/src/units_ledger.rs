//! The unit runtime's ledger: limit records, charges, values, the lock-free
//! arrival port, owned helpers, shared nodes and boards. Connections are in
//! `units_conn.rs`.
//!
//! Byte accounting has one authority, the limit table (`limit.rs`). A unit
//! of the language keeps no books here: the program's limits admit its
//! connections and helpers (`units_lang.rs`). A model unit (the ledger law,
//! the pool checker, the ABI and its bench) holds a `Funding` of its own:
//! its root, its bounds (`enter`/`exit`), and for each helper a root and a
//! cell reservation prepaid from the record that admitted the spawn, ended
//! at the owner's end take.
//!
//! Every byte a model unit holds is a `Charge` with exactly one payer
//! `Account`: a limit record, or a connection's room, whose balance is the
//! byte field of the connection word until the clean and the record that
//! admitted the connection after it. A balance changes only at an operation
//! of the unit that holds the charge, or at that unit's take of an arrival (a
//! credit). Shared state between units is limited to the arrival port, a
//! connection's word and lock, a shared node's count and a board's lock; all
//! of their atomics and locks go through `units_sync.rs`. The model
//! harnesses run their units one at a time, so a helper's charges, which
//! are records in its owner's table, are never touched by two threads at once.
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

#[path = "units_ledger_abi.rs"]
pub(crate) mod abi;
#[cfg(not(test))]
#[path = "units_conn_bench.rs"]
pub(crate) mod bench;
#[path = "units_conn.rs"]
pub(crate) mod conn;
#[cfg(any(test, feature = "testing"))]
#[path = "units_ledger_hold.rs"]
mod hold;
#[cfg(test)]
#[path = "units_ledger_law.rs"]
mod law;

use super::sync::{AtomicPtr, AtomicU64};
use crate::limit::{Funding, Payer};
use conn::{ConnCore, Taken};
use std::{
    cell::UnsafeCell,
    collections::{HashMap, VecDeque},
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

/// Who pays for a charge: a limit record, or a connection's room. One
/// tagged word; a room is tagged in its low bit (`ConnCore` is 64-aligned).
/// A zero-byte charge pins nothing, so its account is never read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Account(NonNull<u8>);
// SAFETY: an account is read only by the unit that holds its charge (a
// room's survivors by the connection's owner); the model harnesses run
// their units one at a time.
unsafe impl Send for Account {}

enum Payee<'a> {
    Limit(Payer),
    Room(&'a ConnCore),
}

impl Account {
    fn limit(p: Payer) -> Self {
        Self(NonNull::new(p.record_pointer().cast()).unwrap())
    }

    fn room(c: &ConnCore) -> Self {
        Self(NonNull::from(c).cast::<u8>().map_addr(|a| a | 1))
    }

    /// The caller holds a nonzero charge on this account, which pins it: a
    /// limit record by its outstanding bytes, a room by its connection,
    /// which the owner keeps.
    fn payee<'a>(self) -> Payee<'a> {
        let p = self.0.as_ptr();
        if p.addr() & 1 != 0 {
            // SAFETY: by the contract.
            Payee::Room(unsafe { &*p.map_addr(|a| a & !1).cast::<ConnCore>() })
        } else {
            Payee::Limit(Payer::from_current(p.cast()))
        }
    }
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Account({:p})", self.0)
    }
}

/// Give `n` bytes back to `to`. The only refund of a model unit's charges
/// and credits; a zero refund touches nothing.
fn repay(to: Account, n: u64) {
    if n == 0 {
        return;
    }
    match to.payee() {
        Payee::Limit(p) => p.unit_refund(n),
        Payee::Room(c) => c.room_repay(n),
    }
}

/// One payer for `size` bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Charge {
    pub(crate) size: u64,
    pub(crate) payer: Account,
}

/// A pending credit: `amount` bytes back to `to`, at the take.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Credit {
    pub(crate) amount: u64,
    pub(crate) to: Account,
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

/// Plain blocks carry their charge with the value. Combining values or
/// adding a second charge spills the list in its original order.
#[derive(Default)]
enum Charges {
    #[default]
    Empty,
    One(Charge),
    Many(Vec<Charge>),
}

impl Charges {
    fn push(&mut self, charge: Charge) {
        match self {
            Self::Empty => *self = Self::One(charge),
            Self::One(first) => *self = Self::Many(vec![*first, charge]),
            Self::Many(charges) => charges.push(charge),
        }
    }

    fn extend(&mut self, charges: impl IntoIterator<Item = Charge>) {
        for charge in charges {
            self.push(charge);
        }
    }
}

impl std::ops::Deref for Charges {
    type Target = [Charge];

    fn deref(&self) -> &[Charge] {
        match self {
            Self::Empty => &[],
            Self::One(charge) => std::slice::from_ref(charge),
            Self::Many(charges) => charges,
        }
    }
}

impl std::ops::DerefMut for Charges {
    fn deref_mut(&mut self) -> &mut [Charge] {
        match self {
            Self::Empty => &mut [],
            Self::One(charge) => std::slice::from_mut(charge),
            Self::Many(charges) => charges,
        }
    }
}

#[derive(Default)]
struct ValueExtra {
    charges: Charges,
    shared: Vec<Shared>,
    res: Option<Res>,
    /// A helper's packed result, including its sequence contents.
    /// Empty for values the ledger makes itself.
    item: Vec<u8>,
    sockets: Option<crate::net::CrossingSockets>,
}

/// An owned value with exclusive charges and optional resource or share data.
pub(crate) struct Value {
    data: ValueData,
    #[cfg(test)]
    pub(crate) tag: u64,
}

type CrossRoom = Box<[std::mem::MaybeUninit<u64>]>;

struct SocketItem {
    room: CrossRoom,
    sockets: crate::net::CrossingSockets,
}

enum ValueData {
    Empty,
    One(Charge),
    Many(Vec<Charge>),
    Extra(Box<ValueExtra>),
    // Flat bytes live in the reserved arrival slot. The room allocation is
    // empty or begins with its logical charge followed by native owner bytes.
    Cross(CrossRoom),
    CrossSockets(Box<SocketItem>),
}

impl Value {
    /// A value with no charge (a helper's packed result, units_lang).
    pub(crate) fn empty() -> Value {
        Self::plain(Charges::Empty)
    }

    fn plain(charges: Charges) -> Value {
        let data = match charges {
            Charges::Empty => ValueData::Empty,
            Charges::One(charge) => ValueData::One(charge),
            Charges::Many(charges) => ValueData::Many(charges),
        };
        Value {
            data,
            #[cfg(test)]
            tag: 0,
        }
    }

    fn charges(&self) -> &[Charge] {
        match &self.data {
            ValueData::Empty | ValueData::Cross(_) | ValueData::CrossSockets(_) => &[],
            ValueData::One(charge) => std::slice::from_ref(charge),
            ValueData::Many(charges) => charges,
            ValueData::Extra(extra) => &extra.charges,
        }
    }

    fn charges_mut(&mut self) -> &mut [Charge] {
        match &mut self.data {
            ValueData::Empty | ValueData::Cross(_) | ValueData::CrossSockets(_) => &mut [],
            ValueData::One(charge) => std::slice::from_mut(charge),
            ValueData::Many(charges) => charges,
            ValueData::Extra(extra) => &mut extra.charges,
        }
    }

    fn extra_mut(&mut self) -> &mut ValueExtra {
        if !matches!(self.data, ValueData::Extra(_)) {
            let old = std::mem::replace(&mut self.data, ValueData::Empty);
            self.data = ValueData::Extra(Box::new(old.into_parts()));
        }
        let ValueData::Extra(extra) = &mut self.data else {
            unreachable!();
        };
        extra
    }

    fn push_charge(&mut self, charge: Charge) {
        match &mut self.data {
            ValueData::Empty => self.data = ValueData::One(charge),
            ValueData::One(first) => self.data = ValueData::Many(vec![*first, charge]),
            ValueData::Many(charges) => charges.push(charge),
            ValueData::Extra(extra) => extra.charges.push(charge),
            ValueData::Cross(_) | ValueData::CrossSockets(_) => unreachable!(),
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.cross_room().map_or_else(
            || self.charges().iter().map(|c| c.size).sum(),
            Self::cross_bytes,
        )
    }

    fn cross(room: CrossRoom, sockets: Option<crate::net::CrossingSockets>) -> Self {
        let data = match sockets {
            Some(sockets) => ValueData::CrossSockets(Box::new(SocketItem { room, sockets })),
            None => ValueData::Cross(room),
        };
        Self {
            data,
            #[cfg(test)]
            tag: 0,
        }
    }

    fn cross_bytes(room: &[std::mem::MaybeUninit<u64>]) -> u64 {
        // SAFETY: the room's first word, if present, is its logical charge.
        room.first()
            .map_or(0, |header| unsafe { header.assume_init() })
    }

    fn cross_room(&self) -> Option<&[std::mem::MaybeUninit<u64>]> {
        match &self.data {
            ValueData::Cross(room) => Some(room),
            ValueData::CrossSockets(item) => Some(&item.room),
            _ => None,
        }
    }

    /// A taken item's nonempty room storage, for reuse.
    fn into_room(self) -> Option<CrossRoom> {
        match self.data {
            ValueData::Cross(room) if !room.is_empty() => Some(room),
            _ => None,
        }
    }

    pub(crate) fn cross_parts(&self) -> (u64, *const u8) {
        let room = self.cross_room().unwrap();
        let content = if room.is_empty() {
            room.as_ptr()
        } else {
            // SAFETY: the owner's native bytes follow the charge word.
            unsafe { room.as_ptr().add(1) }
        };
        (Self::cross_bytes(room), content.cast())
    }

    /// A language item's bytes (units_lang); empty for a ledger value.
    pub(crate) fn item(&self) -> &[u8] {
        match &self.data {
            ValueData::Extra(extra) => &extra.item,
            _ => &[],
        }
    }

    pub(crate) fn item_sockets(&self) -> Option<&crate::net::CrossingSockets> {
        match &self.data {
            ValueData::Extra(extra) => extra.sockets.as_ref(),
            ValueData::CrossSockets(item) => Some(&item.sockets),
            _ => None,
        }
    }

    pub(crate) fn set_item(&mut self, item: Vec<u8>, sockets: Option<crate::net::CrossingSockets>) {
        let extra = self.extra_mut();
        extra.item = item;
        extra.sockets = sockets;
    }
}

impl ValueData {
    fn into_parts(self) -> ValueExtra {
        let charges = match self {
            Self::Empty => Charges::Empty,
            Self::One(charge) => Charges::One(charge),
            Self::Many(charges) => Charges::Many(charges),
            Self::Extra(extra) => return *extra,
            Self::Cross(_) | Self::CrossSockets(_) => unreachable!(),
        };
        ValueExtra {
            charges,
            ..ValueExtra::default()
        }
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
    // A tagged unit id for a ledger port, or one Arc count of the
    // language scheduler's waiter. Null means no wake target.
    wake: AtomicPtr<crate::tasks::Readies>,
    /// The last publisher that woke this port, a hint for waits with many
    /// possible senders. Pool unit records outlive their port references.
    last_waker: AtomicPtr<super::Unit>,
}

// SAFETY: the port's fields are atomics; the arrivals it owns are Send.
unsafe impl Send for Port {}
unsafe impl Sync for Port {}

impl Port {
    pub(crate) fn new() -> Arc<Port> {
        Arc::new(Port {
            head: AtomicPtr::new(ptr::null_mut()),
            fwd: AtomicPtr::new(ptr::null_mut()),
            wake: AtomicPtr::new(ptr::null_mut()),
            last_waker: AtomicPtr::new(ptr::null_mut()),
        })
    }

    /// Ready this scheduler waiter (one count of it moves into the port)
    /// when a publish replaces `WAITING`. Set once, before the port is armed.
    pub(crate) fn set_waker(&self, waker: &Arc<crate::tasks::Readies>) {
        let waker = Arc::into_raw(waker.clone()).cast_mut();
        debug_assert_eq!(waker.addr() & 1, 0);
        let old = self.wake.swap(waker, Ordering::AcqRel);
        debug_assert!(old.is_null());
    }

    /// Whether anything is published and not yet drained.
    pub(crate) fn pending(&self) -> bool {
        !marker(self.head.load(Ordering::Acquire))
    }

    pub(crate) fn set_wake(&self, unit: u32) {
        let tag = ptr::without_provenance_mut((unit as usize) << 1 | 1);
        self.wake.store(tag, Ordering::Release);
    }

    fn receiver(&self) -> Option<&'static super::Unit> {
        let wake = self.wake.load(Ordering::Acquire);
        let id = if wake.addr() & 1 != 0 {
            (wake.addr() >> 1) as u32
        } else if wake.is_null() {
            return None;
        } else {
            // SAFETY: the port pins its scheduler's waiter.
            unsafe { &*wake }.unit_id()
        };
        super::unit_ref(id)
    }

    pub(crate) fn spin_before_park(&self, settling_on: Option<&Self>) -> bool {
        if super::pool().connection_spins == 0 {
            return false;
        }
        let peer = match settling_on {
            Some(port) => port.receiver(),
            // SAFETY: the publisher stores a leaked pool unit record.
            None => unsafe { self.last_waker.load(Ordering::Acquire).as_ref() },
        };
        let Some(peer) = peer else { return false };
        // A language wait normally leaves the port armed. Disarm only when
        // about to spin so a publication caught here needs no scheduler wake.
        let mut armed = None;
        let ready = super::spin_connection(peer, || {
            armed.get_or_insert_with(|| self.disarm());
            self.pending()
        });
        if !ready && armed == Some(true) {
            // A publish that skipped the wake won the race with rearming.
            return !self.arm();
        }
        ready
    }

    /// Push one arrival. Wakes the unit only if it was parked on the port.
    /// Whether a publish here reaches `to`: the same port, or a closed port
    /// that forwards there. A helper's port forwards to its owner from the
    /// owner's end take on.
    pub(crate) fn leads_to(&self, to: &Port) -> bool {
        let mut p = self;
        loop {
            if ptr::eq(p, to) {
                return true;
            }
            if p.head.load(Ordering::Acquire) != FORWARD {
                return false;
            }
            // SAFETY: `fwd` is stored before FORWARD and never changes after.
            p = unsafe { &*p.fwd.load(Ordering::Acquire) };
        }
    }

    pub(crate) fn publish(&self, a: ArrivalRef) {
        let a = a.into_raw();
        // A parked publisher can see the same head address after a pop and
        // reuse. Links follow the live node at that address.
        a.expose_provenance();
        // An empty port is the common ready handoff. Try the publish
        // directly so a sender does not first ask for a shared copy.
        let mut h = ptr::null_mut();
        loop {
            if h == FORWARD {
                // SAFETY: `fwd` is stored before FORWARD and never changes after.
                let f = unsafe { &*self.fwd.load(Ordering::Acquire) };
                // SAFETY: publish still owns the node.
                let a = unsafe { ArrivalRef::from_raw(a) };
                if a.body.forwarded() {
                    f.publish(a);
                }
                return;
            }
            if h == DEAD {
                // SAFETY: as above.
                drop(unsafe { ArrivalRef::from_raw(a) });
                return;
            }
            // SAFETY: `a` is not yet visible to any other thread.
            unsafe {
                (*a).next = if h == WAITING {
                    ptr::null_mut()
                } else {
                    ptr::with_exposed_provenance_mut(h.addr())
                };
            }
            match self
                .head
                .compare_exchange(h, a, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    if h == WAITING {
                        let sender = super::connection_spinner();
                        if !sender.is_null() && sender != self.last_waker.load(Ordering::Relaxed) {
                            self.last_waker.store(sender, Ordering::Release);
                        }
                        let wake = self.wake.load(Ordering::Acquire);
                        if wake.addr() & 1 != 0 {
                            super::wake_id((wake.addr() >> 1) as u32);
                        } else if !wake.is_null() {
                            // SAFETY: the port pins its scheduler's waiter.
                            unsafe { &*wake }.ready_port();
                        }
                    }
                    return;
                }
                Err(x) => h = x,
            }
        }
    }

    /// Take and rearm in one exchange when the only published arrival is an
    /// item for this inbox. A racing publication takes the normal drain path.
    fn take_single_item(&self, c: *const ConnCore) -> Option<ArrivalRef> {
        let head = self.head.load(Ordering::Acquire);
        if marker(head) {
            return None;
        }
        // SAFETY: only this port's owner removes published nodes. A
        // publisher can prepend a node but cannot change the visible head.
        if unsafe {
            !(*head).next.is_null() || !matches!((*head).body, Body::Item(conn, _) if conn == c)
        } {
            return None;
        }
        self.head
            .compare_exchange(head, WAITING, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        // SAFETY: the successful exchange detached this one node.
        Some(unsafe { ArrivalRef::from_raw(head) })
    }

    /// Take every published arrival, oldest first: one swap, and the stack
    /// is reversed in place.
    fn drain(&self) -> Chain {
        let h = self.head.load(Ordering::Acquire);
        if h.is_null() || marker(h) {
            return Chain(ptr::null_mut());
        }
        let mut p = self.head.swap(ptr::null_mut(), Ordering::Acquire);
        // SAFETY: the head belongs to this taker after the swap.
        if unsafe { (*p).next.is_null() } {
            return Chain(p);
        }
        let mut prev = ptr::null_mut();
        while !p.is_null() {
            // SAFETY: the taker owns every detached node after the swap.
            unsafe {
                let next = (*p).next;
                (*p).next = prev;
                prev = p;
                p = next;
            }
        }
        Chain(prev)
    }

    /// Install a marker, returning what was published before it.
    fn close_with(&self, m: *mut Arrival, into: &mut VecDeque<ArrivalRef>) {
        let mut p = self.head.swap(m, Ordering::AcqRel);
        if marker(p) {
            return;
        }
        let mut list = Vec::new();
        while !p.is_null() {
            // SAFETY: as in `drain`.
            let a = unsafe { ArrivalRef::from_raw(p) };
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
        let wake = self.wake.load(Ordering::Acquire);
        if !wake.is_null() && wake.addr() & 1 == 0 {
            // SAFETY: set_waker transferred exactly one count to the port.
            drop(unsafe { Arc::from_raw(wake) });
        }
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
    body: Body,
    #[cfg(test)]
    pub(crate) names: Option<Box<Names>>,
}

#[cfg(all(not(test), target_arch = "x86_64"))]
const _: () = assert!(std::mem::size_of::<Arrival>() == 48);

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
    /// A connection's retirement: its retained header and unused room, and
    /// the retired connection, whose admitting record they go back to.
    LastRelease(u64, Arc<conn::ConnCore>),
    /// A helper's end, carrying the whole helper.
    HelperEnd(HelperEnd),
    /// The settlement of a waiting send, to its sender.
    Settlement(u32, Settle),
    Item(*const ConnCore, Value),
    Close(Arc<ConnCore>),
    /// Made by the owner itself, after the items accepted before the seal.
    SealAnswer(*const ConnCore),
    /// The owner asks a helper's root to cancel (units_lang).
    Cancel,
    /// A `Shared` node's last release away from home, which cleans it (CONC-15).
    SharedRelease(usize),
}

impl Body {
    /// Forwarded from an ended helper to its owner: credits and notices only.
    fn forwarded(&self) -> bool {
        matches!(
            self,
            Body::Credits(_) | Body::LastRelease(..) | Body::SharedRelease(_)
        )
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

/// Arrivals taken from a port, oldest first, owned by the taker.
struct Chain(*mut Arrival);

impl Iterator for Chain {
    type Item = ArrivalRef;
    fn next(&mut self) -> Option<ArrivalRef> {
        if self.0.is_null() {
            return None;
        }
        // SAFETY: the chain owns each node (see `Port::drain`).
        let a = unsafe { ArrivalRef::from_raw(self.0) };
        self.0 = a.next;
        Some(a)
    }
}

impl Drop for Chain {
    fn drop(&mut self) {
        for a in self.by_ref() {
            drop(a);
        }
    }
}

/// Owns an arrival until it is published or dropped. Items occupy slots
/// pinned by the receiving unit until it cleans the connection.
pub(crate) struct ArrivalRef(NonNull<Arrival>);

// SAFETY: the handle owns the node and transfers exclusive access with it.
unsafe impl Send for ArrivalRef {}

impl ArrivalRef {
    /// The caller owns this initialized node and its backing outlives the handle.
    unsafe fn from_raw(p: *mut Arrival) -> Self {
        Self(NonNull::new(p).unwrap())
    }

    fn into_raw(self) -> *mut Arrival {
        std::mem::ManuallyDrop::new(self).0.as_ptr()
    }

    pub(crate) fn boxed(body: Body) -> Self {
        assert!(!matches!(body, Body::Item(..)));
        Self(NonNull::from(Box::leak(Box::new(Arrival::new(body)))))
    }

    /// The caller reserved this slot and keeps its connection live through publish.
    unsafe fn item(node: *mut Arrival, c: &ConnCore, value: Value) -> Self {
        // SAFETY: the reservation grants exclusive access to the uninitialized slot.
        unsafe {
            node.write(Arrival::new(Body::Item(c, value)));
            Self::from_raw(node)
        }
    }

    /// Move an item out of its slot. The receiver still owns the reservation.
    fn into_item(self) -> Result<(*const ConnCore, Taken, TakeInfo), Self> {
        let node = self.0.as_ptr();
        // SAFETY: this handle owns the initialized node. Only the Value
        // and test name move out. The connection still owns the storage.
        unsafe {
            #[cfg(test)]
            super::sync::read_arrival(node);
            let Body::Item(c, value) = &(*node).body else {
                return Err(self);
            };
            let c = *c;
            let slot = (*c).arrival_index(node);
            let (info, value) = (Self::take_info(node), ptr::read(value));
            let _ = self.into_raw();
            Ok((c, Taken { slot, value }, info))
        }
    }

    /// Move the detached notice out and free the allocation that carried it.
    fn into_notice(self) -> (TakeInfo, Body) {
        let node = self.into_raw();
        // SAFETY: the handle owns this boxed, non-item node.
        unsafe {
            let arrival = (Self::take_info(node), ptr::addr_of!((*node).body).read());
            drop(Box::from_raw(node.cast::<std::mem::MaybeUninit<Arrival>>()));
            arrival
        }
    }

    /// The caller owns this initialized arrival and takes its name once.
    unsafe fn take_info(_node: *mut Arrival) -> TakeInfo {
        #[cfg(test)]
        // SAFETY: the caller still owns the live body the name describes.
        unsafe {
            ptr::addr_of!((*_node).names).read().map(|mut names| {
                names.content = (*_node).body.content();
                names
            })
        }
    }
}

impl std::ops::Deref for ArrivalRef {
    type Target = Arrival;

    fn deref(&self) -> &Arrival {
        // SAFETY: the handle owns the node.
        unsafe { self.0.as_ref() }
    }
}

impl std::ops::DerefMut for ArrivalRef {
    fn deref_mut(&mut self) -> &mut Arrival {
        // SAFETY: the handle owns the node.
        unsafe { self.0.as_mut() }
    }
}

impl Drop for ArrivalRef {
    fn drop(&mut self) {
        let node = self.0.as_ptr();
        if matches!(self.body, Body::Item(..)) {
            // SAFETY: the handle owns the node. The slot is still reserved
            // and the backing is pinned by the receiver or publisher.
            unsafe { node.drop_in_place() };
        } else {
            // SAFETY: all other arrivals are allocated by `boxed`.
            drop(unsafe { Box::from_raw(node) });
        }
    }
}

impl Arrival {
    fn new(body: Body) -> Self {
        Self {
            next: ptr::null_mut(),
            body,
            #[cfg(test)]
            names: None,
        }
    }
}

// The production take needs only its kind and the unit's took_ident; the
// laws also consume the arrival's name and content.
#[cfg(test)]
pub(crate) type TakeInfo = Option<Box<Names>>;
#[cfg(not(test))]
pub(crate) type TakeInfo = ();

#[cfg(test)]
pub(crate) fn name_it(
    mut a: ArrivalRef,
    src: impl FnOnce() -> String,
    ident: impl FnOnce() -> String,
) -> ArrivalRef {
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
    payload: UnsafeCell<(Charges, Vec<Shared>)>,
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
        unsafe { self.0.as_ref() }
            .count
            .fetch_add(1, Ordering::Relaxed);
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
    /// A model helper's task record and result slot, charged to its owner.
    charges: Option<[Charge; 2]>,
    state: HState,
}
enum HState {
    Running,
    Ended(Option<Value>),
    Consumed,
}

struct HelperInfo {
    k: usize,
    /// A model helper's cell reservation; its root is the helper's funding.
    cell_res: Option<Funding>,
    owner: Arc<Port>,
    ret: Option<Value>,
}

/// A named balance a snapshot reports (test builds).
#[cfg(test)]
enum Book {
    Root(Payer),
    Bound(Payer),
    Scope(Payer),
    Room(Arc<ConnCore>),
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
    Cancel,
    SharedRelease,
}

/// One take made inside an owner's own release: what was taken, its
/// `took_ident`, and its names in test builds.
pub(crate) type Released = (Took, usize, TakeInfo);

/// One unit's runtime state, touched only by the thread running the unit.
pub(crate) struct UnitCtx {
    pub(crate) port: Arc<Port>,
    /// A model unit's current scope; none for a language unit.
    funding: Option<Funding>,
    /// A model unit's cleaned connections, kept for their room's survivors.
    rooms: Vec<Arc<ConnCore>>,
    #[cfg(test)]
    books: Vec<(String, Book)>,
    reached: VecDeque<ArrivalRef>,
    /// Pins connection slots until all their arrivals have been taken or dropped.
    connections: HashMap<usize, Arc<ConnCore>>,
    /// Test hook: arrivals published to the unit wait here until the harness
    /// names the source whose oldest one reaches the unit next.
    #[cfg(any(test, feature = "testing"))]
    held: Option<VecDeque<ArrivalRef>>,
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
    /// What the last take was about: a settlement's send id, an item's,
    /// close's, seal answer's or last release's connection (`ConnCore`
    /// address), or a helper end's helper index; 0 otherwise.
    pub(crate) took_ident: usize,
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
            funding: None,
            rooms: Vec::new(),
            #[cfg(test)]
            books: Vec::new(),
            reached: VecDeque::new(),
            connections: HashMap::new(),
            #[cfg(any(test, feature = "testing"))]
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
            took_ident: 0,
            #[cfg(test)]
            label,
        }
    }

    /// A language unit: the program's limits account for its bytes.
    pub(crate) fn unfunded() -> UnitCtx {
        UnitCtx::blank(String::new())
    }

    /// A model run root with `limit` bytes.
    pub(crate) fn root(limit: u64) -> UnitCtx {
        Self::root_as(limit, "U0")
    }

    /// A model run root labelled `label` (labels name balances and arrivals
    /// in test builds; elsewhere they are dropped).
    pub(crate) fn root_as(limit: u64, #[allow(unused)] label: &str) -> UnitCtx {
        #[cfg(test)]
        let mut u = UnitCtx::blank(label.into());
        #[cfg(not(test))]
        let mut u = UnitCtx::blank(String::new());
        let f = Funding::root(limit);
        #[cfg(test)]
        u.books.push((label.into(), Book::Root(f.payer.get())));
        u.funding = Some(f);
        u
    }

    /// Bind the unit to the pool unit `id`: a publish to a parked port wakes it.
    pub(crate) fn bind(&self, id: u32) {
        self.port.set_wake(id);
    }

    /// The running receiver can take its own item without dispatching an
    /// inbox wake. Older arrivals and batches still go through `take`.
    /// Inlined: it is the receive fast path in the dispatcher and in both
    /// receive forms, and a call here costs `x_pingpong` about 2% of its Ir.
    #[inline(always)]
    pub(crate) fn take_single_item(&mut self, c: &ConnCore) -> bool {
        if !self.reached.is_empty() {
            return false;
        }
        #[cfg(any(test, feature = "testing"))]
        if self.hooked() {
            return false;
        }
        let Some(arrival) = self.port.take_single_item(c) else {
            return false;
        };
        let Ok((_, item, _)) = self.intake_one(arrival).into_item() else {
            unreachable!();
        };
        self.took_ident = ptr::from_ref(c).addr();
        c.side().taken.push_back(item);
        true
    }

    /// Take the next arrival, parking the pool unit until one is published.
    /// Must run on the bound pool unit.
    pub(crate) fn wait(&mut self) -> (Took, TakeInfo) {
        loop {
            if let Some(t) = self.take() {
                return t;
            }
            if !self.port.spin_before_park(None) && self.port.arm() {
                super::park(None);
            }
        }
    }

    fn funding(&self) -> &Funding {
        self.funding
            .as_ref()
            .expect("a language unit admits through its program's limits")
    }

    /// The bytes charged to the scope that admits now.
    pub(crate) fn current_used(&self) -> u64 {
        self.funding.as_ref().map_or(0, |f| f.payer.get().usage().0)
    }

    /// Admit `n` bytes into the current scope. A refusal raises no capacity
    /// event: the caller reports it.
    fn admit(&self, n: u64) -> Result<Account, Cap> {
        self.funding()
            .unit_charge(n)
            .map(Account::limit)
            .map_err(|avail| Cap { req: n, avail })
    }

    // -- local operations -------------------------------------------------

    pub(crate) fn probe(&self) -> u64 {
        self.funding().available()
    }

    pub(crate) fn alloc_block(&mut self, size: u64) -> Result<Value, Cap> {
        let r = self.admit(size)?;
        Ok(Value::plain(Charges::One(Charge { size, payer: r })))
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
        let mut charges = Charges::Empty;
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
        v.extra_mut().res = Some(Res {
            cell,
            grant: g,
            grant_size: grant,
            fin,
        });
        Ok(v)
    }

    /// One value holding all of these (no charge; a resource's finalizer is lost).
    pub(crate) fn combine(&mut self, vals: Vec<Value>) -> Value {
        let mut charges = Charges::Empty;
        let mut shared = Vec::new();
        for v in vals {
            let parts = v.data.into_parts();
            charges.extend(parts.charges.iter().copied());
            shared.extend(parts.shared);
        }
        let mut out = Value::plain(charges);
        if !shared.is_empty() {
            out.extra_mut().shared = shared;
        }
        out
    }

    /// Drop a value. A resource's finalizer charges its grant: each charge
    /// it makes is a part of the grant's charge, and the unused part goes
    /// back when it ends. A failing finalizer returns the cause chain, a
    /// passing one its survivors.
    pub(crate) fn drop_value(&mut self, v: Value) -> Option<Value> {
        let ValueExtra {
            charges,
            shared,
            res,
            ..
        } = v.data.into_parts();
        for s in shared {
            self.drop_shared(s);
        }
        let Some((res, fin)) = res.and_then(|r| r.fin.clone().map(|f| (r, f))) else {
            for c in charges.iter() {
                repay(c.payer, c.size);
            }
            return None;
        };
        let cell = charges[res.cell];
        let grant = res.grant.map(|g| charges[g]);
        let mut unused = res.grant_size;
        let mut in_grant = |n: u64| -> Charge {
            let g = grant.expect("a finalizer charge without a grant");
            unused = unused.checked_sub(n).expect("a finalizer over its grant");
            Charge {
                size: n,
                payer: g.payer,
            }
        };
        let mut survivors = Charges::Empty;
        if fin.spawn_shared > 0 {
            let c = in_grant(fin.spawn_shared);
            let s = self.new_node(c, Charges::Empty, Vec::new());
            if fin.share_out {
                self.fin_made.push(Some(s));
            } else {
                self.fin_made.push(None);
                self.drop_shared(s);
            }
        }
        if fin.run_lifetime > 0 {
            let c = in_grant(fin.run_lifetime);
            self.run_lifetime.push(c);
        }
        if fin.keep_survivor > 0 {
            survivors.push(in_grant(fin.keep_survivor));
        }
        let msg = (fin.fails && fin.fail_msg > 0).then(|| in_grant(fin.fail_msg));
        if let Some(g) = grant {
            repay(g.payer, unused);
        }
        for (i, c) in charges.iter().enumerate() {
            if i != res.cell && Some(i) != res.grant {
                repay(c.payer, c.size);
            }
        }
        if fin.fails {
            let mut cs = Charges::One(cell);
            cs.extend(msg);
            cs.extend(survivors.iter().copied());
            Some(Value::plain(cs))
        } else {
            repay(cell.payer, cell.size);
            (!survivors.is_empty()).then(|| Value::plain(survivors))
        }
    }

    pub(crate) fn enter_bound(&mut self, size: u64) -> Result<(), Cap> {
        let f = self
            .funding
            .as_mut()
            .expect("a language unit has no bounds here");
        let avail = f.available();
        if size > avail {
            return Err(Cap { req: size, avail });
        }
        f.enter(size).expect("a bound within the available bytes");
        let k = self.next_bound;
        self.next_bound += 1;
        #[cfg(test)]
        {
            let b = Book::Bound(f.payer.get());
            self.books.push((format!("{}/b{k}", self.label), b));
        }
        let _ = k;
        Ok(())
    }

    /// Leave the innermost bound: its unused bytes go back to the scope
    /// around it, which pays for its charges from now on.
    pub(crate) fn exit_bound(&mut self) {
        #[cfg(test)]
        {
            let i = self
                .books
                .iter()
                .rposition(|(_, b)| matches!(b, Book::Bound(_)));
            self.books.remove(i.expect("cannot exit the root scope"));
        }
        self.funding.as_mut().unwrap().exit();
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
        Ok(Value::plain(Charges::One(caught)))
    }

    // -- helpers --------------------------------------------------------------

    /// Spawn an owned helper whose bytes the program's limits account for
    /// (units_lang): its handle index and its unit.
    pub(crate) fn spawn_unit(&mut self) -> (usize, Box<UnitCtx>) {
        self.new_helper(None)
    }

    fn new_helper(&mut self, charges: Option<[Charge; 2]>) -> (usize, Box<UnitCtx>) {
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
        let mut h = Box::new(UnitCtx::blank(label));
        h.helper = Some(HelperInfo {
            k,
            cell_res: None,
            owner: self.port.clone(),
            ret: None,
        });
        self.helpers.push(Some(Handle {
            charges,
            state: HState::Running,
        }));
        (k, h)
    }

    /// Spawn an owned model helper with limit `n` and an inline result slot
    /// of `r` bytes. The spawn charges `256 + r + 32 + n` to the current
    /// scope: the task record and the result slot stay with the owner; `n`
    /// and the 32-byte cell are reservations prepaid from that charge,
    /// ended at the end take. The argument's blocks move into the root.
    #[allow(clippy::type_complexity)]
    pub(crate) fn spawn_helper(
        &mut self,
        n: u64,
        r: u64,
        arg: Option<Value>,
    ) -> Result<(usize, Box<UnitCtx>, Option<Value>), (Cap, Option<Value>)> {
        let t = TASK_FIXED + r + CELL + n;
        let avail = self.funding().available();
        if t > avail {
            return Err((Cap { req: t, avail }, arg));
        }
        let ab = arg.as_ref().map_or(0, Value::bytes);
        if ab > n {
            return Err((Cap { req: ab, avail: n }, arg));
        }
        let Ok(payer) = self.funding().unit_charge(t) else {
            unreachable!()
        };
        let charge = |size| Charge {
            size,
            payer: Account::limit(payer),
        };
        let (k, mut h) = self.new_helper(Some([charge(TASK_FIXED), charge(r)]));
        let root = payer.prepaid(n);
        let cell_res = payer.prepaid(CELL);
        let Ok(cell) = cell_res.unit_charge(CELL) else {
            unreachable!()
        };
        h.cell = Some(Charge {
            size: CELL,
            payer: Account::limit(cell),
        });
        #[cfg(test)]
        {
            let label = h.label.clone();
            h.books.push((label.clone(), Book::Scope(root.payer.get())));
            h.books
                .push((format!("{label}/cell"), Book::Scope(cell_res.payer.get())));
        }
        let arg = arg.map(|mut v| {
            for c in v.charges_mut().iter_mut() {
                repay(c.payer, c.size);
                let Ok(p) = root.unit_charge(c.size) else {
                    unreachable!()
                };
                c.payer = Account::limit(p);
            }
            v
        });
        h.helper.as_mut().unwrap().cell_res = Some(cell_res);
        h.funding = Some(root);
        Ok((k, h, arg))
    }

    /// End a helper: a model helper's unused primary cell goes back to the
    /// cell reservation (or joins the returned cause chain), and the whole
    /// helper travels to its owner as one arrival. Publishing its end hands
    /// the context to the owner, so this is the helper's last touch of it;
    /// the pointer returned may be read only by a caller that knows the end
    /// has not been taken (the single-threaded ledger law).
    pub(crate) fn end_helper(
        mut self: Box<Self>,
        ret: Option<Value>,
        with_cell: bool,
    ) -> NonNull<UnitCtx> {
        assert!(self.helper.is_some(), "only a helper ends this way");
        let cell = self.cell.take();
        let ret = match cell {
            Some(cell) if with_cell => Some(match ret {
                Some(mut v) => {
                    v.push_charge(cell);
                    v
                }
                None => Value::plain(Charges::One(cell)),
            }),
            Some(cell) => {
                repay(cell.payer, cell.size);
                ret
            }
            None => ret,
        };
        let info = self.helper.as_mut().unwrap();
        info.ret = ret;
        let owner = info.owner.clone();
        #[cfg(test)]
        let label = self.label.clone();
        // Retire a directly bound unit id. A language scheduler's waiter
        // remains pinned until the port drops, as a publish may already own
        // a reference to it.
        if self.port.wake.load(Ordering::Relaxed).addr() & 1 != 0 {
            self.port.wake.store(ptr::null_mut(), Ordering::Release);
        }
        self.port.disarm();
        let p = NonNull::from(Box::leak(self));
        let a = named!(
            ArrivalRef::boxed(Body::HelperEnd(HelperEnd(p))),
            format!("helper:{label}"),
            format!("helper_end:{label}")
        );
        owner.publish(a);
        p
    }

    /// The end take: release the task's 256 bytes, hand over a model
    /// helper's cell reservation and root to the scope that paid for them,
    /// then forward the credits and notices it had, in the order they
    /// reached it, then those still in its port.
    fn end_take(&mut self, e: HelperEnd) {
        let p = e.0;
        std::mem::forget(e);
        // SAFETY: made by Box::into_raw in `end_helper`; this take owns it.
        let mut h = unsafe { Box::from_raw(p.as_ptr()) };
        let info = h.helper.take().unwrap();
        let hd = self.helpers[info.k].as_ref().unwrap();
        assert!(
            matches!(hd.state, HState::Running),
            "a helper end taken twice"
        );
        if let Some([fixed, _]) = hd.charges {
            repay(fixed.payer, fixed.size);
        }
        // An ended reservation joins its parent: unused bytes go back now,
        // and its charges are paid there from now on. The union may move
        // the owner's own record, so its handle is brought up to date.
        if let Some(cell) = info.cell_res {
            cell.end();
        }
        if let Some(root) = h.funding.take() {
            root.end();
        }
        if let Some(f) = &self.funding {
            f.normalize();
        }
        self.rooms.append(&mut h.rooms);
        // Forward.
        let raw = Arc::into_raw(self.port.clone()) as *mut Port;
        let old = h.port.fwd.swap(raw, Ordering::Release);
        debug_assert!(old.is_null());
        let mut rest = VecDeque::new();
        #[cfg(any(test, feature = "testing"))]
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
        if let Some([_, result]) = hd.charges {
            repay(result.payer, result.size);
        }
        match ret {
            Some(v) if drop_result => {
                self.drop_value(v);
                None
            }
            r => r,
        }
    }

    // -- shared nodes and boards ----------------------------------------------

    fn new_node(&mut self, charge: Charge, payload: Charges, nested: Vec<Shared>) -> Shared {
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

    pub(crate) fn make_shared(
        &mut self,
        size: u64,
        payload: Option<Value>,
    ) -> Result<Shared, (Cap, Option<Value>)> {
        let r = match self.admit(size) {
            Ok(r) => r,
            Err(c) => return Err((c, payload)),
        };
        let parts = payload.map_or_else(ValueExtra::default, |v| v.data.into_parts());
        Ok(self.new_node(Charge { size, payer: r }, parts.charges, parts.shared))
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
            ArrivalRef::boxed(Body::Credits(cs)),
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
        b.st.lock()
            .unwrap()
            .cur
            .as_ref()
            .expect("a released board")
            .share()
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
            (st.handles == 0 && !st.poster)
                .then(|| st.cur.take())
                .flatten()
        };
        self.board_release(b, rel);
    }

    fn board_release(&mut self, b: &BoardCore, cur: Option<Shared>) {
        let Some(s) = cur else { return };
        self.drop_shared(s);
        #[cfg(test)]
        let label = b.label.clone();
        let a = named!(
            ArrivalRef::boxed(Body::Credits(vec![Credit {
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
    fn layer(&mut self, a: ArrivalRef) {
        #[cfg(any(test, feature = "testing"))]
        if let Some(h) = self.held.as_mut() {
            h.push_back(a);
            return;
        }
        self.reached.push_back(a);
    }

    /// Move everything published to the port into the unit. Items are
    /// numbered here, and a seal answer is made once the items accepted
    /// before the seal have all come in.
    fn intake_remaining(&mut self, arrivals: impl Iterator<Item = ArrivalRef>) {
        for a in arrivals {
            let a = self.intake_one(a);
            self.layer(a);
        }
    }

    fn intake_one(&mut self, #[allow(unused_mut)] mut a: ArrivalRef) -> ArrivalRef {
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
            if side.seal_due > 0 {
                side.seal_due -= 1;
                if side.seal_due == 0 {
                    self.seal_answer(c);
                }
            }
        }
        a
    }

    /// Test hook: the oldest held arrival from `src` reaches the unit.
    #[cfg(test)]
    pub(crate) fn reach(&mut self, src: &str) -> Option<String> {
        if self.held.is_none() {
            self.held = Some(VecDeque::new());
        }
        // An intake can publish a seal answer to this port: drain until empty.
        loop {
            let mut list = self.port.drain().peekable();
            if list.peek().is_none() {
                break;
            }
            self.intake_remaining(list);
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

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn hook(&mut self) {
        self.held.get_or_insert_with(VecDeque::new);
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn hooked(&self) -> bool {
        self.held.is_some()
    }

    /// Take the next arrival that has reached the unit and apply it. Inlined
    /// into the language pump, its hot caller; `release_take` is cold.
    #[inline(always)]
    pub(crate) fn take(&mut self) -> Option<(Took, TakeInfo)> {
        #[cfg(any(test, feature = "testing"))]
        let hooked = self.held.is_some();
        #[cfg(not(any(test, feature = "testing")))]
        let hooked = false;
        let arrival = match self.reached.pop_front() {
            Some(a) => a,
            None if hooked => return None,
            None => {
                let mut arrivals = self.port.drain();
                let first = self.intake_one(arrivals.next()?);
                // Intake accounted for the whole batch before the first take
                // in the queue path. Keep that order when taking directly.
                self.intake_remaining(arrivals);
                first
            }
        };
        match arrival.into_item() {
            Ok((c, item, info)) => {
                // SAFETY: as in `intake_one`.
                unsafe { &*c }.side().taken.push_back(item);
                self.took_ident = c.addr();
                Some((Took::Item, info))
            }
            Err(notice) => Some(self.take_notice(notice)),
        }
    }

    #[cold]
    fn take_notice(&mut self, notice: ArrivalRef) -> (Took, TakeInfo) {
        let (info, body) = notice.into_notice();
        let (t, ident) = match body {
            Body::Credits(cs) => {
                for c in cs {
                    repay(c.to, c.amount);
                }
                (Took::Credits, 0)
            }
            Body::SharedRelease(node) => (Took::SharedRelease, node),
            Body::LastRelease(retained, conn) => {
                conn.retire(retained);
                (Took::LastRelease, Arc::as_ptr(&conn).addr())
            }
            Body::HelperEnd(e) => {
                // SAFETY: the end arrival owns the ended helper's context.
                let ident = unsafe { e.0.as_ref() }.helper.as_ref().unwrap().k;
                self.end_take(e);
                (Took::HelperEnd, ident)
            }
            Body::Settlement(id, Settle::Accepted(cs)) => {
                for c in cs {
                    repay(c.to, c.amount);
                }
                (Took::Accepted(id), id as usize)
            }
            Body::Settlement(id, Settle::Closed(v)) => {
                self.settled.push((id, v));
                (Took::Closed(id), id as usize)
            }
            Body::Settlement(id, Settle::Withdrawn(v)) => {
                self.drop_value(v);
                (Took::Withdrawn(id), id as usize)
            }
            Body::Item(..) => unreachable!(),
            Body::Close(c) => {
                c.side().closed = true;
                (Took::Close, Arc::as_ptr(&c).addr())
            }
            Body::SealAnswer(c) => {
                // SAFETY: as in `intake_one`.
                unsafe { &*c }.side().seal_taken = true;
                (Took::SealAnswer, c.addr())
            }
            Body::Cancel => (Took::Cancel, 0),
        };
        self.took_ident = ident;
        (t, info)
    }

    /// An owner's own release is a take for its own retirement: take, in
    /// port order, everything up to and including the retirement notice of
    /// `conn` just published to this unit. The takes are returned in order
    /// for the caller to act on, as if it had taken them itself. A notice
    /// never jumps the queue; a peer's release waits for the next take.
    #[cold]
    pub(crate) fn release_take(&mut self, conn: usize) -> Vec<Released> {
        #[cfg(any(test, feature = "testing"))]
        self.reach_notice(conn);
        let mut took = Vec::new();
        while let Some((t, info)) = self.take() {
            let done = t == Took::LastRelease && self.took_ident == conn;
            took.push((t, self.took_ident, info));
            if done {
                break;
            }
        }
        took
    }

    /// The value of a waiting send settled `Closed`, back to its sender.
    pub(crate) fn closed_value(&mut self, id: u32) -> Option<Value> {
        let i = self.settled.iter().position(|(s, _)| *s == id)?;
        Some(self.settled.swap_remove(i).1)
    }

    /// End the run root: it retires with its run-lifetime storage, and
    /// every later arrival to it is dropped.
    pub(crate) fn end_root(&mut self) {
        #[cfg(test)]
        self.books.retain(|(_, b)| !matches!(b, Book::Root(_)));
        self.run_lifetime.clear();
        self.discard_arrivals();
    }

    fn discard_arrivals(&mut self) {
        if !matches!(self.port.head.load(Ordering::Acquire), DEAD | FORWARD) {
            self.port.close_with(DEAD, &mut VecDeque::new());
        }
        self.reached.clear();
        #[cfg(any(test, feature = "testing"))]
        {
            self.held = None;
        }
    }

    /// Every live, named balance: `name=used/limit`. A scope's use is its
    /// record's capacity less what it has available; a room's is the byte
    /// field of its connection word.
    #[cfg(test)]
    pub(crate) fn snapshot(&self, out: &mut Vec<String>) {
        for (name, b) in &self.books {
            let (used, limit) = match b {
                Book::Root(p) | Book::Bound(p) | Book::Scope(p) => p.usage(),
                Book::Room(c) => (c.room_used(), c.room_limit()),
            };
            out.push(format!("{name}={used}/{limit}"));
        }
    }

    /// A model connection's room is a named balance until its clean.
    #[cfg(test)]
    fn book_room(&mut self, c: &Arc<ConnCore>) {
        self.books
            .push((format!("{}/room", c.label), Book::Room(c.clone())));
    }

    #[cfg(test)]
    fn close_room(&mut self, c: &ConnCore) {
        self.books
            .retain(|(_, b)| !matches!(b, Book::Room(r) if ptr::eq(&**r, c)));
    }
}

impl Drop for UnitCtx {
    fn drop(&mut self) {
        self.discard_arrivals();
    }
}

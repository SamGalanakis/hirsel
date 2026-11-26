//! Connections between units: an inbox owned by one unit, client handles
//! held by any, and a bounded room that pays for the items in flight.
//!
//! The connection word packs the room's byte balance, the slots in use and two
//! flags. A send accepted at issue is one update of the word, one publish to
//! the owner's port, and a wake only if the owner is parked; a ready receiver
//! takes the item with no lock and frees its slot and bytes with one
//! subtraction. Up to 22 slots, the word records exactly which ones are in
//! use. Larger connections keep the count in the word and use a free list.
//! The connection lock is taken only on the slow paths: a send that has to
//! wait, the service of waiting sends, a withdrawal, the seal, the last client
//! and the cleaning's end.
use super::super::sync::{AtomicU64, Mutex};
use super::{
    Account, Arrival, ArrivalRef, Body, Cap, Credit, CrossRoom, Released, Settle, UnitCtx, Value,
    repay,
};
use crate::limit::Payer;
pub(crate) use hirsel_core_layout::charge::{
    CONNECTION_MAX_CAPACITY as MAX_CAP, CONNECTION_MAX_ROOM as MAX_ROOM,
};
use std::{
    cell::{Cell, UnsafeCell},
    collections::VecDeque,
    mem::MaybeUninit,
    sync::{Arc, atomic::Ordering},
};

const SEALED: u64 = 1 << 63;
const WAITERS: u64 = 1 << 62;
const SLOT_SHIFT: u32 = 40;
const SLOT: u64 = 1 << SLOT_SHIFT;
const BYTES: u64 = MAX_ROOM;
const BITMAP_CAP: u32 = 22;
const SLOTS: u64 = MAX_CAP << SLOT_SHIFT;

fn first_free_bit(w: u64) -> u64 {
    let free = !w & SLOTS;
    free & free.wrapping_neg()
}

struct Waiter {
    id: u32,
    sender: Arc<super::Port>,
    item: Outgoing,
    bytes: u64,
    #[cfg(test)]
    label: String,
}

enum Outgoing {
    Ledger(Value),
    Cross(crate::units_lang::CrossSend),
}

impl Outgoing {
    fn rejected(self) -> Value {
        match self {
            Self::Ledger(v) => v,
            Self::Cross(_) => Value::plain(Default::default()),
        }
    }
}

struct Slow {
    waiters: VecDeque<Waiter>,
    clients: u64,
    cleaned: bool,
    close_sent: bool,
    retained: u64,
}

/// The owner's side, touched only by the owner.
pub(crate) struct Side {
    pub(crate) taken: VecDeque<Taken>,
    /// Items that have come into the owner, and of those, received or cleaned.
    pub(crate) intaken: u64,
    freed: u64,
    /// Items accepted before the seal that have not come in yet.
    pub(crate) seal_due: u64,
    pub(crate) closed: bool,
    pub(crate) seal_taken: bool,
    sealed: bool,
    cleaned: bool,
}

type ArrivalSlot = UnsafeCell<MaybeUninit<Arrival>>;

#[repr(C, align(64))]
struct Hot {
    word: AtomicU64,
    first: ArrivalSlot,
}

#[cfg(all(not(test), target_arch = "x86_64"))]
const _: () = assert!(std::mem::size_of::<Hot>() == 64);

struct FreeArrivals {
    returned: Vec<u32>,
    next: u32,
}

pub(crate) struct Taken {
    pub(super) slot: u32,
    pub(super) value: Value,
}

enum FlatSlots {
    // A one-slot scalar connection needs no payload allocation.
    Inline(UnsafeCell<MaybeUninit<[u8; 8]>>),
    Arena(Box<[UnsafeCell<MaybeUninit<u8>>]>),
}

impl Taken {
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<FlatSlots>() == 16);

pub(crate) struct ConnCore {
    hot: Hot,
    spares: Box<[ArrivalSlot]>,
    flat: FlatSlots,
    /// Slot 0's room storage, kept from its last take for its next send.
    /// Only the holder of slot 0's reservation touches it: the taker stores
    /// it before the release, and a sender takes it after its admission.
    spare: UnsafeCell<CrossRoom>,
    pub(crate) stride: usize,
    pub(crate) rows: Box<[[u64; 4]]>,
    pub(crate) seqs: Box<[[u64; 4]]>,
    pub(crate) socket_offsets: Box<[u64]>,
    free: Option<Box<Mutex<FreeArrivals>>>,
    cap: u64,
    b: u64,
    h: u64,
    owner: Arc<super::Port>,
    /// The record a model unit admitted `h + b` from; none when the
    /// program's limits hold the connection's charge (units_lang).
    admitting: Option<Payer>,
    /// Cleared at the clean: the room's survivors are paid by the admitting
    /// record from then on. Owner only.
    room_open: Cell<bool>,
    slow: Mutex<Slow>,
    side: UnsafeCell<Side>,
    #[cfg(test)]
    pub(crate) label: String,
}

// SAFETY: slots pass from sender to port to taker. Reuse follows the
// connection word's bitmap, or the free-list lock on larger connections.
// The owner alone touches `side`, `room_open` and the admitting record.
unsafe impl Send for ConnCore {}
unsafe impl Sync for ConnCore {}

impl ConnCore {
    fn arrival_slot(&self, before: u64) -> (u32, *mut Arrival) {
        let i = if let Some(free) = &self.free {
            let mut free = free.lock().unwrap();
            free.returned.pop().unwrap_or_else(|| {
                let i = free.next;
                free.next += 1;
                i
            })
        } else {
            first_free_bit(before).trailing_zeros() - SLOT_SHIFT
        };
        let node = if i == 0 {
            self.hot.first.get().cast()
        } else {
            self.spares[i as usize - 1].get().cast()
        };
        (i, node)
    }

    fn flat_ptr(&self, i: u32) -> *mut u8 {
        let base = match &self.flat {
            FlatSlots::Inline(first) => first.get().cast::<u8>(),
            FlatSlots::Arena(slots) => slots.as_ptr().cast_mut().cast(),
        };
        base.wrapping_add(i as usize * self.stride)
    }

    /// The caller owns this connection's published slot.
    pub(super) unsafe fn arrival_index(&self, node: *mut Arrival) -> u32 {
        if node == self.hot.first.get().cast() {
            0
        } else {
            // SAFETY: all other slot addresses come from this slice.
            unsafe { node.cast::<ArrivalSlot>().offset_from(self.spares.as_ptr()) as u32 + 1 }
        }
    }

    fn admit_fast(&self, bytes: u64) -> Result<u64, u64> {
        self.hot
            .word
            .compare_exchange(0, SLOT + bytes, Ordering::AcqRel, Ordering::Acquire)
            .or_else(|w| {
                if w & (SEALED | WAITERS) != 0 {
                    return Err(w);
                }
                self.hot
                    .word
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                        if w & (SEALED | WAITERS) == 0 {
                            self.admit_item(w, bytes)
                        } else {
                            None
                        }
                    })
            })
    }

    fn occupied(&self, w: u64) -> u64 {
        let field = (w & SLOTS) >> SLOT_SHIFT;
        if self.cap <= u64::from(BITMAP_CAP) {
            u64::from(field.count_ones())
        } else {
            field
        }
    }

    fn admit_item(&self, w: u64, bytes: u64) -> Option<u64> {
        let bit = if self.cap <= u64::from(BITMAP_CAP) {
            let bit = first_free_bit(w);
            if bit == 0 || bit.trailing_zeros() - SLOT_SHIFT >= self.cap as u32 {
                return None;
            }
            bit
        } else {
            if self.occupied(w) >= self.cap {
                return None;
            }
            SLOT
        };
        ((w & BYTES) + bytes <= self.b).then(|| w + bit + bytes)
    }

    fn release_item(&self, index: u32, bytes: u64) -> u64 {
        // A slot stays reserved through take. Receive or cleaning releases it
        // only after all reads of the node have finished.
        let bit = if let Some(free) = &self.free {
            free.lock().unwrap().returned.push(index);
            SLOT
        } else {
            SLOT << index
        };
        self.hot.word.fetch_sub(bit + bytes, Ordering::AcqRel)
    }

    pub(crate) fn room_used(&self) -> u64 {
        self.hot.word.load(Ordering::Acquire) & BYTES
    }
    pub(crate) fn room_add(&self, n: u64) {
        let w = self.hot.word.fetch_add(n, Ordering::AcqRel);
        assert!((w & BYTES) + n <= self.b, "room over its limit");
    }
    pub(crate) fn room_sub(&self, n: u64) {
        let w = self.hot.word.fetch_sub(n, Ordering::AcqRel);
        assert!(w & BYTES >= n, "a negative room balance");
    }
    #[cfg(test)]
    pub(crate) fn room_limit(&self) -> u64 {
        self.b
    }
    /// A refund to the room: to its word until the clean, then to the
    /// record that admitted the connection.
    pub(super) fn room_repay(&self, n: u64) {
        if self.room_open.get() {
            self.room_sub(n);
        } else {
            self.admitting
                .expect("a room survivor of a connection the program's limits hold")
                .unit_refund(n);
        }
    }
    /// The header a cleaned connection retains, and all it retains.
    #[cfg(feature = "record")]
    pub(crate) fn retained(&self) -> (u64, u64) {
        (self.h, self.slow.lock().unwrap().retained)
    }

    /// The last release's take: the retained header and unused room go back
    /// to a model unit's admitting record.
    pub(super) fn retire(&self, retained: u64) {
        if let Some(p) = self.admitting {
            p.unit_refund(retained);
        }
    }
    #[allow(clippy::mut_from_ref)]
    pub(crate) fn side(&self) -> &mut Side {
        // SAFETY: only the owner calls this, from its own thread, and never
        // holds two of these at once.
        unsafe { &mut *self.side.get() }
    }
}

/// The owner's handle: receive, seal, clean.
pub(crate) struct Inbox(pub(crate) Arc<ConnCore>);

impl Inbox {
    /// The caller owns the inbox and holds this item's reservation.
    pub(crate) fn flat_ptr(&self, t: &Taken) -> *const u8 {
        self.0.flat_ptr(t.slot)
    }

    #[cfg(test)]
    pub(crate) fn front_tag(&self) -> Option<u64> {
        self.0.side().taken.front().map(|t| t.value.tag)
    }

    /// Admit and copy the oldest taken item before making its slot reusable.
    /// If admission fails, the callback leaves the destination and queue alone.
    pub(crate) fn receive_cross<E>(
        &self,
        copy: impl FnOnce(
            u64,
            *const u8,
            *const u8,
            Option<&crate::net::CrossingSockets>,
        ) -> Result<(), E>,
    ) -> Option<Result<(), E>> {
        let c = &*self.0;
        let side = c.side();
        let front = side.taken.front()?;
        let ptr = c.flat_ptr(front.slot);
        #[cfg(test)]
        crate::units::sync::read_arrival(ptr);
        let (bytes, content) = front.value.cross_parts();
        if let Err(e) = copy(bytes, ptr, content, front.value.item_sockets()) {
            return Some(Err(e));
        }
        let slot = front.slot;
        let taken = side.taken.pop_front().unwrap();
        if slot == 0 {
            if let Some(room) = taken.value.into_room() {
                // SAFETY: this taker holds slot 0's reservation until the release.
                unsafe { *c.spare.get() = room };
            }
        }
        side.freed += 1;
        let w = c.release_item(slot, bytes);
        if w & WAITERS != 0 {
            UnitCtx::serve(c, &mut c.slow.lock().unwrap());
        }
        Some(Ok(()))
    }
}
/// A sender's handle. The ABI boxes it, so alignment keeps the client's
/// reads off cache lines that the receiver might write.
#[repr(align(64))]
pub(crate) struct Client(pub(crate) Arc<ConnCore>);

impl Client {
    pub(crate) fn receiver_port(&self) -> &super::Port {
        &self.0.owner
    }

    pub(crate) fn clone_client(&self) -> Client {
        self.0.slow.lock().unwrap().clients += 1;
        Client(self.0.clone())
    }
}

#[derive(Debug)]
pub(crate) enum Sent {
    /// Bigger than the whole room: refused before issue, the value dropped.
    Bounds,
    Closed(Value),
    Accepted,
    Full(Value),
    Waiting(u32),
}

#[derive(Debug)]
pub(crate) enum Received {
    Ok(Value),
    Empty,
    Closed,
    Capacity(Cap),
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Value({} bytes)", self.bytes())
    }
}

impl UnitCtx {
    /// A connection with `cap` slots of `elem` bytes of header each and a
    /// room of `b` bytes, both admitted here; this unit owns its inbox.
    pub(crate) fn conn_new(&mut self, cap: u64, elem: u64, b: u64) -> Result<(Inbox, Client), Cap> {
        self.conn_new_cross(cap, elem, b, 0, &[])
    }

    /// As `conn_new`, for items laid out by `stride` and `owners`.
    pub(crate) fn conn_new_cross(
        &mut self,
        cap: u64,
        elem: u64,
        b: u64,
        stride: usize,
        owners: &[[u64; 4]],
    ) -> Result<(Inbox, Client), Cap> {
        assert!(cap > 0 && cap <= MAX_CAP && b <= MAX_ROOM);
        let h = 64 + cap * elem;
        let payer = self
            .funding()
            .unit_charge(h + b)
            .map_err(|avail| Cap { req: h + b, avail })?;
        let (inbox, client) = self.make_conn(cap, elem, b, stride, owners, Some(payer));
        #[cfg(test)]
        self.book_room(&inbox.0);
        Ok((inbox, client))
    }

    /// A connection whose charge the program's limits hold (units_lang).
    pub(crate) fn conn_open(
        &mut self,
        cap: u64,
        elem: u64,
        b: u64,
        stride: usize,
        owners: &[[u64; 4]],
    ) -> (Inbox, Client) {
        self.make_conn(cap, elem, b, stride, owners, None)
    }

    fn make_conn(
        &mut self,
        cap: u64,
        elem: u64,
        b: u64,
        stride: usize,
        owners: &[[u64; 4]],
        admitting: Option<Payer>,
    ) -> (Inbox, Client) {
        assert!(cap > 0 && cap <= MAX_CAP && b <= MAX_ROOM);
        let h = 64 + cap * elem;
        let k = self.next_conn;
        self.next_conn += 1;
        #[cfg(test)]
        let label = format!("{}.C{k}", self.label);
        let _ = k;
        let owner = self.port.clone();
        let c = Arc::new(ConnCore {
            hot: Hot {
                word: AtomicU64::new(0),
                first: UnsafeCell::new(MaybeUninit::uninit()),
            },
            spares: (1..cap)
                .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                .collect(),
            flat: match (cap as usize)
                .checked_mul(stride)
                .expect("connection slot extent")
            {
                0..=8 => FlatSlots::Inline(UnsafeCell::new(MaybeUninit::uninit())),
                // Each UnsafeCell contains MaybeUninit, so all bit
                // patterns are valid and the arena need not be zeroed.
                bytes => FlatSlots::Arena(unsafe {
                    Box::<[UnsafeCell<MaybeUninit<u8>>]>::new_uninit_slice(bytes).assume_init()
                }),
            },
            spare: UnsafeCell::new(Box::default()),
            stride,
            rows: owners.into(),
            seqs: owners
                .iter()
                .copied()
                .filter(|o| o[1] == hirsel_core_layout::cross::SEQ)
                .collect(),
            socket_offsets: owners
                .iter()
                .filter(|o| o[1] == hirsel_core_layout::cross::SOCKET)
                .map(|o| o[0])
                .collect(),
            free: (cap > u64::from(BITMAP_CAP)).then(|| {
                Box::new(Mutex::new(FreeArrivals {
                    returned: Vec::with_capacity(cap as usize),
                    next: 0,
                }))
            }),
            cap,
            b,
            h,
            owner,
            admitting,
            room_open: Cell::new(true),
            slow: Mutex::new(Slow {
                waiters: VecDeque::new(),
                clients: 1,
                cleaned: false,
                close_sent: false,
                retained: 0,
            }),
            side: UnsafeCell::new(Side {
                taken: VecDeque::new(),
                intaken: 0,
                freed: 0,
                seal_due: 0,
                closed: false,
                seal_taken: false,
                sealed: false,
                cleaned: false,
            }),
            #[cfg(test)]
            label,
        });
        self.connections.insert(Arc::as_ptr(&c).addr(), c.clone());
        (Inbox(c.clone()), Client(c))
    }

    /// The room pays for an item from here on; the sender's own payers are
    /// credited now (accepted at issue).
    fn pay_into_room(c: &ConnCore, v: &mut Value) {
        for ch in v.charges_mut() {
            repay(ch.payer, ch.size);
            ch.payer = Account::room(c);
        }
    }

    fn publish_item(c: &ConnCore, before: u64, item: Outgoing, bytes: u64) {
        let (index, node) = c.arrival_slot(before);
        let v = match item {
            Outgoing::Ledger(v) => v,
            // SAFETY: admission owns this slot. The sender pins the source
            // through publication or through the settlement of its wait.
            Outgoing::Cross(source) => unsafe {
                #[cfg(test)]
                let tag = source.tag;
                let mut empty = Box::default();
                let spare = if index == 0 {
                    // SAFETY: this sender holds slot 0's reservation.
                    &mut *c.spare.get()
                } else {
                    &mut empty
                };
                let room = source.copy_into(c.flat_ptr(index), c, bytes, spare);
                let v = Value::cross(room, source.sockets);
                #[cfg(test)]
                let v = Value { tag, ..v };
                v
            },
        };
        let a = named!(
            // SAFETY: the connection admitted this item before choosing the slot.
            unsafe { ArrivalRef::item(node, c, v) },
            format!("conn:{}", c.label),
            String::new()
        );
        c.owner.publish(a);
    }

    /// Issue a send (`blocking`) or a try-send.
    pub(crate) fn send(&mut self, to: &Client, mut v: Value, blocking: bool) -> Sent {
        let c = &*to.0;
        let bytes = v.bytes();
        if bytes > c.b {
            self.drop_value(v);
            return Sent::Bounds;
        }
        match c.admit_fast(bytes) {
            Ok(w) => {
                Self::pay_into_room(c, &mut v);
                Self::publish_item(c, w, Outgoing::Ledger(v), bytes);
                Sent::Accepted
            }
            Err(w) if w & SEALED != 0 => Sent::Closed(v),
            Err(_) => self.issue_slow(c, Outgoing::Ledger(v), bytes, blocking),
        }
    }

    /// The caller keeps the source and its owners live and immutable until
    /// it observes acceptance or takes the settlement of a waiting send.
    pub(crate) unsafe fn send_cross<'a>(
        to: &Client,
        source: crate::units_lang::CrossSend,
        bytes: u64,
        blocking: bool,
        unit: impl FnOnce() -> &'a mut UnitCtx,
    ) -> Sent {
        let c = &*to.0;
        if bytes > c.b {
            return Sent::Bounds;
        }
        let item = Outgoing::Cross(source);
        match c.admit_fast(bytes) {
            Ok(w) => {
                Self::publish_item(c, w, item, bytes);
                Sent::Accepted
            }
            Err(w) if w & SEALED != 0 => Sent::Closed(item.rejected()),
            Err(_) => unit().issue_slow(c, item, bytes, blocking),
        }
    }

    #[cold]
    fn issue_slow(&mut self, c: &ConnCore, mut item: Outgoing, bytes: u64, blocking: bool) -> Sent {
        let mut s = c.slow.lock().unwrap();
        let mut w = c.hot.word.load(Ordering::Acquire);
        loop {
            if w & SEALED != 0 {
                return Sent::Closed(item.rejected());
            }
            if w & WAITERS == 0
                && let Some(updated) = c.admit_item(w, bytes)
            {
                match c
                    .hot
                    .word
                    .compare_exchange(w, updated, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => {
                        drop(s);
                        if let Outgoing::Ledger(v) = &mut item {
                            Self::pay_into_room(c, v);
                        }
                        Self::publish_item(c, w, item, bytes);
                        return Sent::Accepted;
                    }
                    Err(x) => {
                        w = x;
                        continue;
                    }
                }
            }
            if !blocking {
                return Sent::Full(item.rejected());
            }
            if w & WAITERS == 0 {
                if let Err(x) =
                    c.hot
                        .word
                        .compare_exchange(w, w | WAITERS, Ordering::AcqRel, Ordering::Acquire)
                {
                    w = x;
                    continue;
                }
            }
            let id = self.next_send;
            self.next_send += 1;
            s.waiters.push_back(Waiter {
                id,
                sender: self.port.clone(),
                item,
                bytes,
                #[cfg(test)]
                label: format!("{}.W{id}", self.label),
            });
            return Sent::Waiting(id);
        }
    }

    /// Accept waiting sends, oldest first, while the oldest fits. The room
    /// pays for each item at once; the sender's charges stay as a
    /// reservation, released at its take of the settlement.
    fn serve(c: &ConnCore, s: &mut Slow) {
        loop {
            let Some(head) = s.waiters.front() else {
                let _ = c
                    .hot
                    .word
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                        (w & WAITERS != 0).then_some(w & !WAITERS)
                    });
                return;
            };
            let bytes = head.bytes;
            let Ok(w) = c
                .hot
                .word
                .try_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                    if w & SEALED == 0 {
                        c.admit_item(w, bytes)
                    } else {
                        None
                    }
                })
            else {
                return;
            };
            let mut wt = s.waiters.pop_front().unwrap();
            let credits = match &mut wt.item {
                Outgoing::Ledger(v) => v
                    .charges_mut()
                    .iter_mut()
                    .map(|ch| {
                        let cr = Credit {
                            amount: ch.size,
                            to: ch.payer,
                        };
                        ch.payer = Account::room(c);
                        cr
                    })
                    .collect(),
                Outgoing::Cross(_) => Vec::new(),
            };
            Self::publish_item(c, w, wt.item, bytes);
            let a = named!(
                ArrivalRef::boxed(Body::Settlement(wt.id, Settle::Accepted(credits))),
                format!("conn:{}", c.label),
                format!("settlement:{}:Accepted", wt.label)
            );
            wt.sender.publish(a);
        }
    }

    /// Withdraw a waiting send on cancellation. True if it was still waiting:
    /// its settlement (`Withdrawn`, carrying the value) comes to this unit.
    pub(crate) fn withdraw(&mut self, on: &Client, id: u32) -> bool {
        let c = &*on.0;
        let mut s = c.slow.lock().unwrap();
        let Some(i) = s
            .waiters
            .iter()
            .position(|w| w.id == id && Arc::ptr_eq(&w.sender, &self.port))
        else {
            return false;
        };
        let wt = s.waiters.remove(i).unwrap();
        let a = named!(
            ArrivalRef::boxed(Body::Settlement(
                wt.id,
                Settle::Withdrawn(wt.item.rejected())
            )),
            format!("conn:{}", c.label),
            format!("settlement:{}:Withdrawn", wt.label)
        );
        self.port.publish(a);
        Self::serve(c, &mut s);
        true
    }

    /// Receive the oldest taken item into the current record.
    pub(crate) fn receive(&mut self, inbox: &Inbox) -> Received {
        let c = &*inbox.0;
        let side = c.side();
        let Some(front) = side.taken.front() else {
            return if side.closed {
                Received::Closed
            } else {
                Received::Empty
            };
        };
        let bytes = front.value.bytes();
        let r = match self.admit(bytes) {
            Ok(r) => r,
            Err(cap) => return Received::Capacity(cap),
        };
        let Taken { slot, value: mut v } = side.taken.pop_front().unwrap();
        for ch in v.charges_mut() {
            ch.payer = r;
        }
        side.freed += 1;
        let w = c.release_item(slot, bytes);
        if w & WAITERS != 0 {
            Self::serve(c, &mut c.slow.lock().unwrap());
        }
        Received::Ok(v)
    }

    /// Seal: every waiting send settles `Closed`; the seal answer comes after
    /// the items accepted before the seal.
    pub(crate) fn seal(&mut self, inbox: &Inbox) {
        let c = &*inbox.0;
        let side = c.side();
        assert!(!side.sealed, "sealed twice");
        side.sealed = true;
        let mut s = c.slow.lock().unwrap();
        let w = c
            .hot
            .word
            .try_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                Some((w | SEALED) & !WAITERS)
            })
            .unwrap();
        for wt in s.waiters.drain(..) {
            let a = named!(
                ArrivalRef::boxed(Body::Settlement(wt.id, Settle::Closed(wt.item.rejected()))),
                format!("conn:{}", c.label),
                format!("settlement:{}:Closed", wt.label)
            );
            wt.sender.publish(a);
        }
        drop(s);
        let due = c.occupied(w) - (side.intaken - side.freed);
        side.seal_due = due;
        if due == 0 {
            self.seal_answer(c);
        }
    }

    pub(super) fn seal_answer(&mut self, c: &ConnCore) {
        let a = named!(
            ArrivalRef::boxed(Body::SealAnswer(c as *const ConnCore)),
            format!("conn:{}", c.label),
            format!("seal_answer:{}", c.label)
        );
        // Through the port, so it follows everything already published on
        // this connection (the seal's own `Closed` settlements, a close).
        self.port.publish(a);
    }

    /// After the seal answer: drop the unreceived items, last accepted first,
    /// then split the room. The room joins the admitting record (its
    /// survivors are charged there from now on) and the header plus the
    /// unused room stay charged until the last-release notice is taken. When
    /// no client is left the cleaning is the owner's own release: it takes
    /// through its notice before returning (`release_take`).
    pub(crate) fn clean(&mut self, inbox: &Inbox) -> (Vec<Value>, Vec<Released>) {
        let c = &*inbox.0;
        let side = c.side();
        assert!(
            side.seal_taken && !side.cleaned,
            "cleaning before the seal answer, or twice"
        );
        let mut causes = Vec::new();
        while let Some(Taken { slot, value }) = side.taken.pop_back() {
            side.freed += 1;
            if let Some(room) = value.cross_room() {
                c.release_item(slot, Value::cross_bytes(room));
            } else {
                c.release_item(slot, 0);
                if let Some(cause) = self.drop_value(value) {
                    causes.push(cause);
                }
            }
        }
        side.cleaned = true;
        let retained = c.h + (c.b - c.room_used());
        c.room_open.set(false);
        #[cfg(test)]
        self.close_room(c);
        if c.admitting.is_some() {
            self.rooms.push(inbox.0.clone());
        }
        let last = {
            let mut s = c.slow.lock().unwrap();
            s.cleaned = true;
            s.retained = retained;
            s.clients == 0
        };
        self.connections.remove(&Arc::as_ptr(&inbox.0).addr());
        let took = if last {
            Self::last_release(&inbox.0, retained);
            self.own_release(&inbox.0)
        } else {
            Vec::new()
        };
        (causes, took)
    }

    fn last_release(c: &Arc<ConnCore>, retained: u64) {
        let a = named!(
            ArrivalRef::boxed(Body::LastRelease(retained, c.clone())),
            format!("retire:{}", c.label),
            format!("last_release:{}", c.label)
        );
        c.owner.publish(a);
    }

    /// Drop a client. The last one closes an unsealed inbox, and retires a
    /// cleaned connection.
    /// cleaned connection; when this unit owns it, that is its own release
    /// and it takes through the notice before returning.
    pub(crate) fn drop_client(&mut self, cl: Client) -> Vec<Released> {
        let c = &*cl.0;
        let (close, last) = {
            let mut s = c.slow.lock().unwrap();
            s.clients -= 1;
            if s.clients > 0 {
                return Vec::new();
            }
            let sealed = c.hot.word.load(Ordering::Acquire) & SEALED != 0;
            let close = !sealed && !s.close_sent;
            s.close_sent |= close;
            (close, s.cleaned.then_some(s.retained))
        };
        if close {
            let a = named!(
                ArrivalRef::boxed(Body::Close(cl.0.clone())),
                format!("conn:{}", c.label),
                format!("close:{}", c.label)
            );
            c.owner.publish(a);
        }
        match last {
            Some(r) => {
                Self::last_release(&cl.0, r);
                self.own_release(&cl.0)
            }
            None => Vec::new(),
        }
    }

    /// After a retirement notice was published: when it went to this unit
    /// (the owner, or a helper whose port now forwards here), this was the
    /// owner's own release and it takes through the notice. A peer's release
    /// takes nothing; its notice waits for the owner's next take.
    fn own_release(&mut self, c: &Arc<ConnCore>) -> Vec<Released> {
        if !c.owner.leads_to(&self.port) {
            return Vec::new();
        }
        self.release_take(Arc::as_ptr(c).addr())
    }
}

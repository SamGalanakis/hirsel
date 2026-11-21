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
use super::{Arrival, ArrivalRef, Body, Cap, Credit, Kind, RecRef, Settle, UnitCtx, Value};
use std::{
    cell::UnsafeCell,
    collections::VecDeque,
    mem::MaybeUninit,
    sync::{Arc, atomic::Ordering},
};

const SEALED: u64 = 1 << 63;
const WAITERS: u64 = 1 << 62;
const SLOT_SHIFT: u32 = 40;
const SLOT: u64 = 1 << SLOT_SHIFT;
const BYTES: u64 = SLOT - 1;
const BITMAP_CAP: u32 = 22;
const SLOTS: u64 = ((1 << BITMAP_CAP) - 1) << SLOT_SHIFT;
/// The largest capacity and room the word can hold.
pub(crate) const MAX_CAP: u64 = (1 << 22) - 1;
pub(crate) const MAX_ROOM: u64 = BYTES;

fn first_free_bit(w: u64) -> u64 {
    let free = !w & SLOTS;
    free & free.wrapping_neg()
}

struct Waiter {
    id: u32,
    sender: Arc<super::Port>,
    value: Value,
    bytes: u64,
    #[cfg(test)]
    label: String,
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

pub(crate) struct ConnCore {
    hot: Hot,
    spares: Box<[ArrivalSlot]>,
    free: Option<Box<Mutex<FreeArrivals>>>,
    cap: u64,
    b: u64,
    h: u64,
    owner: Arc<super::Port>,
    room: RecRef,
    admitting: RecRef,
    slow: Mutex<Slow>,
    side: UnsafeCell<Side>,
    #[cfg(test)]
    pub(crate) label: String,
}

// SAFETY: slots pass from sender to port to taker. Reuse follows the
// connection word's bitmap, or the free-list lock on larger connections.
// The owner alone touches `side`. Other units only copy the record pointers.
unsafe impl Send for ConnCore {}
unsafe impl Sync for ConnCore {}

impl ConnCore {
    fn arrival_slot(&self, before: u64) -> *mut Arrival {
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
        if i == 0 {
            self.hot.first.get().cast()
        } else {
            self.spares[i as usize - 1].get().cast()
        }
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
    #[allow(clippy::mut_from_ref)]
    pub(crate) fn side(&self) -> &mut Side {
        // SAFETY: only the owner calls this, from its own thread, and never
        // holds two of these at once.
        unsafe { &mut *self.side.get() }
    }
}

/// The owner's handle: receive, seal, clean.
pub(crate) struct Inbox(pub(crate) Arc<ConnCore>);
/// A sender's handle.
pub(crate) struct Client(pub(crate) Arc<ConnCore>);

impl Client {
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
        assert!(cap > 0 && cap <= MAX_CAP && b <= MAX_ROOM);
        let h = 64 + cap * elem;
        let adm = self.admit(h + b)?;
        let k = self.next_conn;
        self.next_conn += 1;
        #[cfg(test)]
        let label = format!("{}.C{k}", self.label);
        let _ = k;
        let owner = self.port.clone();
        #[cfg(test)]
        let room_name = Some(format!("{label}/room"));
        #[cfg(not(test))]
        let room_name = None;
        let c = Arc::new_cyclic(|w| {
            let r = self.new_rec(b, Some(adm), Kind::Room(w.as_ptr()), true, room_name);
            ConnCore {
                hot: Hot {
                    word: AtomicU64::new(0),
                    first: UnsafeCell::new(MaybeUninit::uninit()),
                },
                spares: (1..cap)
                    .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
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
                room: r,
                admitting: adm,
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
            }
        });
        self.connections.insert(Arc::as_ptr(&c).addr(), c.clone());
        Ok((Inbox(c.clone()), Client(c)))
    }

    /// The room pays for an item from here on; the sender's own payers are
    /// credited now (accepted at issue).
    fn pay_into_room(&self, c: &ConnCore, v: &mut Value) {
        for ch in v.charges_mut() {
            self.refund(*ch);
            ch.payer = c.room;
        }
    }

    fn publish_item(c: &ConnCore, before: u64, v: Value) {
        let a = named!(
            // SAFETY: the connection admitted this item before choosing the slot.
            unsafe { ArrivalRef::item(c.arrival_slot(before), c, v) },
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
        let fast = c
            .hot
            .word
            .compare_exchange(0, SLOT + bytes, Ordering::AcqRel, Ordering::Acquire)
            .or_else(|w| {
                if w & (SEALED | WAITERS) != 0 {
                    return Err(w);
                }
                c.hot
                    .word
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                        if w & (SEALED | WAITERS) == 0 {
                            c.admit_item(w, bytes)
                        } else {
                            None
                        }
                    })
            });
        match fast {
            Ok(w) => {
                self.pay_into_room(c, &mut v);
                Self::publish_item(c, w, v);
                return Sent::Accepted;
            }
            Err(w) if w & SEALED != 0 => return Sent::Closed(v),
            Err(_) => {}
        }
        let mut s = c.slow.lock().unwrap();
        let mut w = c.hot.word.load(Ordering::Acquire);
        loop {
            if w & SEALED != 0 {
                return Sent::Closed(v);
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
                        self.pay_into_room(c, &mut v);
                        Self::publish_item(c, w, v);
                        return Sent::Accepted;
                    }
                    Err(x) => {
                        w = x;
                        continue;
                    }
                }
            }
            if !blocking {
                return Sent::Full(v);
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
                value: v,
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
            let credits = wt
                .value
                .charges_mut()
                .iter_mut()
                .map(|ch| {
                    let cr = Credit {
                        amount: ch.size,
                        to: ch.payer,
                    };
                    ch.payer = c.room;
                    cr
                })
                .collect();
            Self::publish_item(c, w, wt.value);
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
            ArrivalRef::boxed(Body::Settlement(wt.id, Settle::Withdrawn(wt.value))),
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
                ArrivalRef::boxed(Body::Settlement(wt.id, Settle::Closed(wt.value))),
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
    /// unused room stay charged until the last-release notice is taken.
    pub(crate) fn clean(&mut self, inbox: &Inbox) -> Vec<Value> {
        let c = &*inbox.0;
        let side = c.side();
        assert!(side.seal_taken && !side.cleaned, "cleaning before the seal answer, or twice");
        let mut causes = Vec::new();
        while let Some(Taken { slot, value }) = side.taken.pop_back() {
            c.release_item(slot, 0);
            side.freed += 1;
            if let Some(cause) = self.drop_value(value) {
                causes.push(cause);
            }
        }
        side.cleaned = true;
        let retained = c.h + (c.b - c.room_used());
        self.rec(c.room).ended.set(true);
        let last = {
            let mut s = c.slow.lock().unwrap();
            s.cleaned = true;
            s.retained = retained;
            s.clients == 0
        };
        if last {
            Self::last_release(c, retained);
        }
        self.connections.remove(&Arc::as_ptr(&inbox.0).addr());
        causes
    }

    fn last_release(c: &ConnCore, retained: u64) {
        let a = named!(
            ArrivalRef::boxed(Body::LastRelease(Credit {
                amount: retained,
                to: c.admitting,
            })),
            format!("retire:{}", c.label),
            format!("last_release:{}", c.label)
        );
        c.owner.publish(a);
    }

    /// Drop a client. The last one closes an unsealed inbox, and retires a
    /// cleaned connection.
    pub(crate) fn drop_client(&mut self, cl: Client) {
        let c = &*cl.0;
        let (close, last) = {
            let mut s = c.slow.lock().unwrap();
            s.clients -= 1;
            if s.clients > 0 {
                return;
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
        if let Some(r) = last {
            Self::last_release(c, r);
        }
    }
}

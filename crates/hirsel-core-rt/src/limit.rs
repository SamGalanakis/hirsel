//! Task-local paged limit table. Bytes retain payer slots; only scope handles
//! and union edges use structural pins. The free list retains each unused slot's
//! pin for its next scope. Remote events never touch local fields.
//! Native balance updates wrap because admission/refund invariants already prove
//! their range; they do not add guards against incorrect generated bookkeeping.
//! User-visible size arithmetic, Capacity and count-increment overflow stay checked.
use crate::Capacity;
#[path = "limit_refunds.rs"]
mod refunds;
use refunds::{Inbox, Remote};
use std::{
    cell::{Cell, UnsafeCell},
    ptr::NonNull,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy)]
struct Live {
    owner: u64,
    capacity: u64,
    parent: Option<Payer>,
    inbox: NonNull<Inbox>,
}
impl Live {
    const EMPTY: Self = Self {
        owner: 0,
        capacity: 0,
        parent: None,
        inbox: NonNull::dangling(),
    };
}
/// Frozen local record prefix. Emitters compare/subtract available and add the
/// admitted bytes to outstanding. Neither operation is atomic.
#[repr(C)]
pub struct LimitRecord {
    pub available: u64,
    pub outstanding: u64,
    refs: usize,
    parent: Option<usize>,
    rank: u32,
    live: Live,
    next_free: Option<NonNull<Slot>>,
    #[cfg(any(test, feature = "testing"))]
    occupied: bool,
}
#[repr(C, align(32))]
pub(crate) struct Slot {
    local: UnsafeCell<LimitRecord>,
    pending: AtomicU64,
    pending_zeros: AtomicUsize,
    next_pending: UnsafeCell<Option<NonNull<Slot>>>,
    handover: Cell<Option<u64>>,
    domain: NonNull<Domain>,
    id: usize,
}
struct Forest {
    pages: Vec<Box<[Slot; 64]>>,
    length: usize,
    free: Option<NonNull<Slot>>,
    // Read/written only after the last local handle retires.
    retired_bytes: u64,
    zeros: usize,
    #[cfg(any(test, feature = "testing"))]
    attempts: usize,
    #[cfg(any(test, feature = "testing"))]
    fail: Option<(usize, bool)>,
}
impl Forest {
    #[inline(always)]
    fn slot(&self, id: usize) -> &Slot {
        &self.pages[id / 64][id % 64]
    }
    #[inline(always)]
    fn record(&self, id: usize) -> &LimitRecord {
        // SAFETY: task-exclusive forest access, or the retired mailbox owner.
        unsafe { &*self.slot(id).local.get() }
    }
    #[inline(always)]
    fn record_mut(&mut self, id: usize) -> &mut LimitRecord {
        // SAFETY: task-exclusive forest access, or the retired mailbox owner.
        unsafe { &mut *self.slot(id).local.get() }
    }
    #[inline]
    fn retain(&mut self, id: usize) {
        self.record_mut(id).refs += 1;
    }
    fn release(&mut self, mut id: usize) {
        loop {
            let slot = NonNull::from(self.slot(id));
            // SAFETY: this task owns the table; a structural pin or charge retains slot.
            let r = unsafe { &mut *slot.as_ref().local.get() };
            r.refs -= 1;
            if r.refs != 0 || r.outstanding != 0 {
                return;
            }
            let parent = r.parent.or_else(|| r.live.parent.map(|p| p.slot().id));
            // The free list takes over the final slot pin until its next use.
            r.refs = 1;
            r.parent = None;
            r.rank = 0;
            // SAFETY: the initialized record and its domain stay live through release.
            unsafe { r.live.inbox.as_ref().release(slot.as_ref().domain.as_ref()) };
            r.live = Live::EMPTY;
            #[cfg(any(test, feature = "testing"))]
            {
                r.occupied = false;
            }
            r.next_free = self.free;
            self.free = Some(slot);
            if let Some(p) = parent {
                id = p;
            } else {
                return;
            }
        }
    }
    #[cold]
    fn fresh(&mut self, domain: NonNull<Domain>) -> NonNull<Slot> {
        let id = self.length;
        if id.is_multiple_of(64) {
            self.pages.push(Box::new(std::array::from_fn(|i| Slot {
                local: UnsafeCell::new(LimitRecord {
                    available: 0,
                    outstanding: 0,
                    refs: 1,
                    parent: None,
                    rank: 0,
                    live: Live::EMPTY,
                    next_free: None,
                    #[cfg(any(test, feature = "testing"))]
                    occupied: false,
                }),
                pending: AtomicU64::new(0),
                pending_zeros: AtomicUsize::new(0),
                next_pending: UnsafeCell::new(None),
                handover: Cell::new(None),
                domain,
                id: id + i,
            })));
        }
        self.length += 1;
        NonNull::from(self.slot(id))
    }
    #[inline(always)]
    fn take(&mut self, domain: NonNull<Domain>, live: Live) -> Payer {
        let slot = if let Some(slot) = self.free {
            // SAFETY: only this task takes records; every returned record is uncharged.
            self.free = unsafe { (*slot.as_ref().local.get()).next_free };
            slot
        } else {
            self.fresh(domain)
        };
        Self::initialize(slot, live)
    }
    #[inline(always)]
    fn initialize(slot: NonNull<Slot>, live: Live) -> Payer {
        // SAFETY: the free list transfers its sole pin; charge and union rank are zero.
        let r = unsafe { &mut *slot.as_ref().local.get() };
        r.available = live.capacity;
        r.parent = None;
        r.rank = 0;
        r.next_free = None;
        // SAFETY: the domain-owned inbox is pinned by the parent/task creating this record.
        unsafe { live.inbox.as_ref().retain() };
        r.live = live;
        #[cfg(any(test, feature = "testing"))]
        {
            r.occupied = true;
        }
        Payer(slot)
    }
    fn insert(&mut self, domain: NonNull<Domain>, live: Live) -> usize {
        self.take(domain, live).slot().id
    }
    fn root(&self, mut id: usize) -> usize {
        while let Some(parent) = self.record(id).parent {
            id = parent;
        }
        id
    }
    #[inline(always)]
    fn find(&mut self, id: usize) -> usize {
        match self.record(id).parent {
            None => id,
            Some(parent) if self.record(parent).parent.is_none() => parent,
            Some(_) => self.compress(id),
        }
    }
    #[cold]
    fn compress(&mut self, id: usize) -> usize {
        let root = self.root(id);
        let mut node = id;
        while let Some(p) = self.record(node).parent {
            if p == root {
                break;
            }
            self.retain(root);
            self.record_mut(node).parent = Some(root);
            // Transfer the old edge to this traversal until its next step.
            if node != id {
                self.release(node);
            }
            node = p;
        }
        if node != id {
            self.release(node);
        }
        root
    }
    fn merge(&mut self, child: usize, original_parent: usize, unused: u64) -> usize {
        let parent = self.find(original_parent);
        let parent_available = self.record(parent).available.wrapping_add(unused);
        let parent_live = self.record(parent).live;
        let (winner, loser) = if self.record(child).rank > self.record(parent).rank {
            (child, parent)
        } else {
            if self.record(child).rank == self.record(parent).rank {
                self.record_mut(parent).rank += 1;
            }
            (parent, child)
        };
        let previous_inbox = self.record(winner).live.inbox;
        if previous_inbox != parent_live.inbox {
            // SAFETY: the winner's replacement transfers its live-record pin.
            unsafe {
                parent_live.inbox.as_ref().retain();
                previous_inbox
                    .as_ref()
                    .release(self.slot(winner).domain.as_ref());
            }
        }
        self.record_mut(winner).live = parent_live;
        self.record_mut(winner).available = parent_available;
        self.retain(winner);
        self.record_mut(loser).parent = Some(winner);
        self.release(original_parent);
        winner
    }

    fn refund(&mut self, id: usize, bytes: u64) {
        if bytes == 0 {
            self.zeros -= 1;
            self.release(id);
        } else {
            let root = self.find(id);
            self.refund_rooted(id, root, bytes);
        }
    }
    #[inline(always)]
    fn refund_rooted(&mut self, id: usize, root: usize, bytes: u64) {
        if bytes == 0 {
            self.zeros -= 1;
            self.release(id);
            return;
        }
        self.record_mut(root).available = self.record(root).available.wrapping_add(bytes);
        let r = self.record_mut(id);
        r.outstanding = r.outstanding.wrapping_sub(bytes);
        if r.refs == 0 && r.outstanding == 0 {
            r.refs = 1;
            self.release(id);
        }
    }
}
struct Domain {
    forest: UnsafeCell<Forest>,
    handles: Cell<usize>,
    owner: Option<usize>,
    remote: Mutex<Remote>,
    pending: AtomicBool,
    active_inbox: Cell<NonNull<Inbox>>,
    reap_inboxes: Cell<Option<NonNull<Inbox>>>,
}
thread_local! { static THREAD:u8=const{0}; static CURRENT_DOMAIN: Cell<usize> = const { Cell::new(0) }; }

pub(crate) fn preserve_binding<R>(run: impl FnOnce() -> R) -> R {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_DOMAIN.set(self.0);
        }
    }
    let _restore = Restore(CURRENT_DOMAIN.get());
    run()
}
#[inline(always)]
fn thread() -> usize {
    THREAD.with(|v| v as *const u8 as usize)
}
/// One pointer to a stable task-table slot. The slot's outstanding bytes keep
/// both it and its table alive. No per-block reference count or allocation.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Payer(NonNull<Slot>);
impl Payer {
    #[inline(always)]
    fn slot(self) -> &'static Slot {
        // SAFETY: a local handle, structural edge or outstanding bytes pin this slot.
        unsafe { &*self.0.as_ptr() }
    }
    #[inline(always)]
    fn domain(self) -> &'static Domain {
        // SAFETY: every occupied slot pins its table's domain.
        unsafe { self.slot().domain.as_ref() }
    }
    #[inline(always)]
    #[allow(clippy::mut_from_ref)] // Task-exclusive UnsafeCell; remote uses only mailbox atomics.
    fn forest(self) -> &'static mut Forest {
        // SAFETY: local task is exclusive; orphan access owns the mailbox mutex.
        unsafe { &mut *self.domain().forest.get() }
    }
    #[inline(always)]
    pub(crate) fn record_pointer(self) -> *mut LimitRecord {
        self.local()
    }
    pub(crate) fn from_current(pointer: *mut LimitRecord) -> Self {
        // SAFETY: Slot is repr(C), with the local record at offset zero. Context
        // refreshes this pointer at each limit/finalizer transition.
        Self(unsafe { NonNull::new_unchecked(pointer.cast()) })
    }
    #[inline(always)]
    fn local(self) -> *mut LimitRecord {
        self.slot().local.get()
    }
    #[inline(always)]
    pub(crate) fn canonical(self) -> Self {
        // SAFETY: local task exclusively owns the local record fields.
        if unsafe { (*self.local()).parent.is_none() } {
            // SAFETY: canonical identities are untagged stable slot pointers.
            Self(unsafe { NonNull::new_unchecked(self.local().cast()) })
        } else {
            let f = self.forest();
            let id = f.find(self.slot().id);
            Self(NonNull::from(f.slot(id)))
        }
    }
    fn root(self) -> Self {
        let f = self.forest();
        Self(NonNull::from(f.slot(f.root(self.slot().id))))
    }
    #[inline(always)]
    pub(crate) fn admit(self, bytes: Option<u64>) -> Result<(), Capacity> {
        let payer = self;
        #[cfg(any(test, feature = "testing"))]
        let injected = payer.injected();
        #[cfg(not(any(test, feature = "testing")))]
        let injected = false;
        // SAFETY: admission takes a canonical live payer under the
        // task's exclusive custody. Block words canonicalize before admission.
        let r = unsafe { &mut *payer.0.as_ptr().cast::<LimitRecord>() };
        match bytes {
            Some(n) if !injected && n <= r.available => {
                r.available = r.available.wrapping_sub(n);
                r.outstanding = r.outstanding.wrapping_add(n);
                #[cfg(any(test, feature = "charge-observation"))]
                crate::peak::sample();
                Ok(())
            }
            n => Err(crate::tasks::capacity_refused(
                n.unwrap_or(u64::MAX),
                r.available,
            )),
        }
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn injected(self) -> bool {
        let f = self.forest();
        f.attempts += 1;
        f.fail
            .is_some_and(|(n, later)| f.attempts == n || later && f.attempts >= n)
    }
    pub(crate) fn pin_zero(self) {
        let f = self.forest();
        f.retain(self.slot().id);
        f.zeros += 1;
    }
    #[inline(always)]
    pub(crate) fn refund(self, bytes: u64) {
        if bytes != 0 {
            self.settle(bytes);
        }
    }
    /// A pointer match proves this is the canonical current record. The slow
    /// path follows unions.
    #[inline(always)]
    pub(crate) fn settle_in(self, context: &crate::Context, bytes: u64) {
        if bytes != 0 && self.0.as_ptr().cast::<LimitRecord>() == context.limit_record() {
            // SAFETY: current is canonical and task-exclusive; this block's
            // outstanding bytes prevent reuse of its stored payer slot.
            unsafe {
                let r = &mut *context.limit_record();
                r.available = r.available.wrapping_add(bytes);
                r.outstanding = r.outstanding.wrapping_sub(bytes);
            }
        } else {
            self.settle(bytes);
        }
    }
    #[inline]
    pub(crate) fn settle(self, bytes: u64) {
        let d = self.domain();
        let local = d.owner == Some(thread())
            || CURRENT_DOMAIN.with(|v| v.get() == self.slot().domain.as_ptr().addr());
        if local && d.handles.get() != 0 {
            let root = self.root();
            // SAFETY: scheduler execution serializes reads of live routing.
            let mut live = unsafe { (*root.local()).live };
            if bytes != 0 && live.owner != crate::tasks::current_id() {
                crate::tasks::before_commit();
                let root = self.root();
                // SAFETY: an outstanding charge pins the current route.
                live = unsafe { (*root.local()).live };
            }
            if live.owner != crate::tasks::current_id() {
                let publish = || {
                    d.remote
                        .lock()
                        .unwrap()
                        .enqueue(self, bytes, Some(live.inbox))
                };
                if bytes == 0 {
                    publish();
                } else {
                    crate::tasks::commit_refund(live.owner, bytes, publish);
                    crate::tasks::poll_after_refund();
                }
                return;
            }
            let root = self.canonical();
            // SAFETY: this task owns the table; outstanding stays on its original slot.
            let r = unsafe { &mut *self.local() };
            if bytes != 0 && r.parent.is_none() && r.refs != 0 {
                r.available = r.available.wrapping_add(bytes);
                r.outstanding = r.outstanding.wrapping_sub(bytes);
            } else {
                self.forest()
                    .refund_rooted(self.slot().id, root.slot().id, bytes);
            }
        } else {
            self.remote_refund(bytes);
        }
    }
    #[cold]
    fn remote_refund(self, bytes: u64) {
        let d = self.domain();
        let mut remote = d.remote.lock().unwrap();
        if remote.retired {
            let f = self.forest();
            f.refund(self.slot().id, bytes);
            f.retired_bytes -= bytes;
            let empty = f.retired_bytes == 0 && f.zeros == 0;
            let domain = self.slot().domain;
            drop(remote);
            if empty {
                // SAFETY: all handles, zero leases and outstanding bytes are gone.
                unsafe {
                    drop(Box::from_raw(domain.as_ptr()));
                }
            }
        } else {
            remote.enqueue(self, bytes, None);
        }
    }

    pub(crate) fn prepaid(self, bytes: u64) -> Funding {
        let parent = self.canonical();
        let f = self.forest();
        f.record_mut(parent.slot().id).outstanding -= bytes;
        f.retain(parent.slot().id);
        let id = f.insert(
            self.slot().domain,
            Live {
                owner: f.record(parent.slot().id).live.owner,
                inbox: f.record(parent.slot().id).live.inbox,
                capacity: bytes,
                parent: Some(parent),
            },
        );
        self.domain().handles.set(self.domain().handles.get() + 1);
        Funding {
            payer: Cell::new(Self(NonNull::from(f.slot(id)))),
        }
    }

    /// Hand a reservation's record over to the scope it leads to, returning the
    /// cell's charge with it. The record is its own root until this handover
    /// and its cell is its only charge, so the charge goes back to the record's
    /// balance and `end_id` hands it to the live limit together with the unused
    /// bytes (MEM-20), whichever task drops the cell: never to the inbox of the
    /// task that reserved it, which may have ended.
    pub(crate) fn finish_reserved(self, charge: u64) {
        let child = self.slot().id;
        // SAFETY: the cell owns this unmerged record; no other handle charges it.
        unsafe {
            let r = &mut *self.local();
            debug_assert!(r.parent.is_none() && r.outstanding == charge);
            r.available += charge;
            r.outstanding = 0;
        }
        let funding = Funding {
            payer: Cell::new(self),
        };
        // The hand-over merges the record or leaves a prepaid queue pin for
        // the recipient. The reservation cell releases its own pin in either case.
        funding.end();
        funding.payer.get().forest().release(child);
        std::mem::forget(funding);
    }
}
/// Safe task-local scope handle. Allocations retain bytes, never these handles.

#[repr(C)]
pub struct Funding {
    // An owner event can install a ranked union while a caller borrows Context.
    pub(crate) payer: Cell<Payer>,
}
impl Funding {
    /// Reserve an isolate cell without replacing the context's current scope.
    /// The returned payer is a child record whose charge is later installed in
    /// an obligation or refunded and merged back into this scope.
    ///
    /// Unlike `enter`, the context keeps its own pin on this scope, so the
    /// record's parent edge takes a pin of its own: the cell may outlive the
    /// scope (a caught failure's replacement cell lives until the task's
    /// Context drops), and `end_id` releases that edge when the cell finishes.
    pub(crate) fn isolate_reserve(&mut self, bytes: u64) -> Result<Payer, Capacity> {
        let live = self.reserve_scope(bytes)?;
        let domain = unsafe { self.payer.get().0.as_ref().domain };
        let forest = unsafe { &mut *domain.as_ref().forest.get() };
        forest.retain(self.payer.get().slot().id);
        let payer = forest.take(domain, live);
        unsafe {
            let record = &mut *payer.local();
            record.available = 0;
            record.outstanding = bytes;
        }
        Ok(payer)
    }

    pub fn root(capacity: u64) -> Self {
        let owner = crate::tasks::current_id();
        let mut remote = Remote::new();
        let inbox = remote.inbox(owner);
        // SAFETY: the new domain's bound cache owns one inbox reference.
        unsafe { inbox.as_ref().retain() };
        let domain = Box::new(Domain {
            forest: UnsafeCell::new(Forest {
                pages: Vec::new(),
                length: 0,
                free: None,
                retired_bytes: 0,
                zeros: 0,
                #[cfg(any(test, feature = "testing"))]
                attempts: 0,
                #[cfg(any(test, feature = "testing"))]
                fail: None,
            }),
            handles: Cell::new(1),
            owner: Some(thread()),
            remote: Mutex::new(remote),
            pending: AtomicBool::new(false),
            active_inbox: Cell::new(inbox),
            reap_inboxes: Cell::new(None),
        });
        let domain = NonNull::from(Box::leak(domain));
        // SAFETY: the fresh domain is exclusively owned until its first handle returns.
        let f = unsafe { &mut *domain.as_ref().forest.get() };
        let id = f.insert(
            domain,
            Live {
                owner,
                inbox,
                capacity,
                parent: None,
            },
        );
        Self {
            payer: Cell::new(Payer(NonNull::from(f.slot(id)))),
        }
    }
    pub(crate) fn native(&self) {
        // SAFETY: context_new calls this before publishing the fresh native context.
        unsafe {
            (*self.payer.get().slot().domain.as_ptr()).owner = None;
        }
        self.bind();
    }
    #[inline(always)]
    pub(crate) fn bind(&self) {
        // SAFETY: Funding holds an untagged slot under exclusive execution.
        let domain = unsafe { self.payer.get().0.as_ref().domain };
        CURRENT_DOMAIN.with(|v| v.set(domain.as_ptr().addr()));
        let payer = self.payer.get().canonical();
        // SAFETY: bind holds the table lease. The cache owns an inbox pin even
        // if handover has replaced the last record referring to its old owner.
        unsafe {
            let domain = domain.as_ref();
            let inbox = (*payer.local()).live.inbox;
            if inbox != domain.active_inbox.get() {
                inbox.as_ref().retain();
                domain.active_inbox.replace(inbox).as_ref().release(domain);
            }
        };
    }
    pub(crate) fn unbind() {
        CURRENT_DOMAIN.with(|v| v.set(0));
    }
    /// Apply only the running owner's ordered refunds. No admission or allocation.
    #[inline(always)]
    pub fn drain(&self) {
        // SAFETY: a Funding handle is untagged and retains its entire domain.
        let domain = unsafe { self.payer.get().0.as_ref().domain.as_ref() };
        let pending = if CURRENT_DOMAIN.with(|v| v.get() == domain as *const Domain as usize) {
            // SAFETY: bind cached the running task's domain-owned inbox.
            unsafe { domain.active_inbox.get().as_ref().pending() }
        } else {
            domain.pending.load(Ordering::Acquire)
        };
        if pending {
            self.drain_notified();
        }
    }
    /// Owner-event delivery only. Routing and folding share the domain mutex.
    /// A union may move the current representative.
    #[cold]
    pub(crate) fn drain_notified(&self) {
        self.drain_pending();
        self.normalize();
    }
    #[cold]
    fn drain_pending(&self) {
        let domain = self.payer.get().domain();
        let mut remote = domain.remote.lock().unwrap();
        self.drain_locked(&mut remote, false);
        domain.pending.store(remote.pending(), Ordering::Release);
        remote.reap(domain);
    }
    fn drain_locked(&self, remote: &mut Remote, retired: bool) {
        remote.route_unassigned();
        let owner = (!retired).then(crate::tasks::current_id);
        let f = self.payer.get().forest();
        while let Some(slot) = remote.pop(owner) {
            // SAFETY: the dirty node retains a charge/zero pin until delivery;
            // pop cleared its intrusive link under the same domain mutex.
            let slot = unsafe { slot.as_ref() };
            if let Some(unused) = slot.handover.take() {
                let original_parent = f.record_mut(slot.id).parent.take().unwrap();
                f.merge(slot.id, original_parent, unused);
                f.release(slot.id);
            }
            let bytes = slot.pending.swap(0, Ordering::Relaxed);
            let zeros = slot.pending_zeros.swap(0, Ordering::Relaxed);
            if bytes != 0 {
                f.refund(slot.id, bytes);
                if retired {
                    f.retired_bytes -= bytes;
                }
            }
            for _ in 0..zeros {
                f.refund(slot.id, 0);
            }
        }
    }
    pub(crate) fn fast(&self) -> *mut LimitRecord {
        self.payer.get().canonical().local()
    }
    #[inline(always)]
    pub fn available(&self) -> u64 {
        // SAFETY: this handle pins its canonical local record.
        unsafe { (*self.fast()).available }
    }
    #[cfg(any(test, feature = "charge-observation"))]
    pub(crate) fn observation_capacity(&self) -> u64 {
        // SAFETY: the run's root handle pins its canonical live limit record.
        unsafe { (*self.fast()).live.capacity }
    }
    #[cfg(feature = "testing")]
    pub(crate) fn set_test_available(&self, bytes: u64) {
        // SAFETY: the process fixture exclusively owns its live root record.
        let record = unsafe { &mut *self.fast() };
        let reserved = record.live.capacity.wrapping_sub(record.available);
        // A sample may choose u64::MAX after startup. Its synthetic allowance
        // and prepaid startup use the same modular balances as the fixture.
        record.live.capacity = bytes.wrapping_add(reserved);
        record.available = bytes;
    }
    pub(crate) fn admit(&self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.payer.get().canonical().admit(bytes)
    }
    #[cfg(test)]
    pub(crate) fn refund(&self, bytes: u64) {
        self.payer.get().refund(bytes);
    }
    pub fn reserve(&self, bytes: u64) -> Result<Self, Capacity> {
        self.admit(Some(bytes))?;
        Ok(self.payer.get().prepaid(bytes))
    }
    pub(crate) fn owner(&self) -> u64 {
        self.payer
            .get()
            .forest()
            .record(self.payer.get().slot().id)
            .live
            .owner
    }
    pub(crate) fn task_owner(&self, owner: u64) {
        let payer = self.payer.get().canonical();
        let old = payer.forest().record(payer.slot().id).live;
        if old.owner != owner {
            let domain = payer.domain();
            let mut remote = domain.remote.lock().unwrap();
            remote.reap(domain);
            let inbox = remote.inbox(owner);
            // SAFETY: the replacing live record takes over this inbox pin.
            unsafe { inbox.as_ref().retain() };
            let live = &mut payer.forest().record_mut(payer.slot().id).live;
            live.owner = owner;
            live.inbox = inbox;
            // SAFETY: the map retains the previous inbox until the next reap.
            unsafe { old.inbox.as_ref().release(domain) };
            remote.reroute(old.inbox);
        }
    }

    /// The receiving limit and its task at the current hand-over position.
    fn handover_destination(&self) -> (Payer, u64) {
        let child = self.payer.get();
        let parent = child
            .forest()
            .record(child.slot().id)
            .live
            .parent
            .unwrap()
            .root();
        let owner = parent.forest().record(parent.slot().id).live.owner;
        (parent, owner)
    }

    /// Close an unused grant or a cause cell's private reservation.
    pub fn end(&self) {
        let (_, owner) = self.handover_destination();
        if owner != crate::tasks::current_id() {
            crate::tasks::before_commit();
        }
        let (_, owner) = self.handover_destination();
        if owner == crate::tasks::current_id() {
            self.end_id();
        } else {
            self.drain_notified();
            self.transfer();
        }
    }

    /// The old owner folds before publishing the hand-over. The link routes
    /// later refunds immediately. Only the recipient completes the ranked union
    /// and applies the unused reservation, at its next event.
    pub(crate) fn handover(&mut self, creator: u64) {
        let active = self.owner();
        let (_, owner) = self.handover_destination();
        if owner != active || creator != active {
            crate::tasks::before_commit();
        }
        self.drain();
        let child = self.payer.get();
        let live = child.forest().record(child.slot().id).live;
        let (_, owner) = self.handover_destination();
        if owner != live.owner {
            self.transfer();
        } else if creator != live.owner {
            let unused = child.forest().record(child.slot().id).available;
            crate::tasks::commit_refund(owner, unused, || self.exit());
        } else {
            self.exit();
        }
    }

    fn transfer(&self) {
        let child = self.payer.get();
        let d = child.domain();
        let (parent, owner) = self.handover_destination();
        let unused = child.forest().record(child.slot().id).available;
        crate::tasks::commit_refund(owner, unused, || {
            let mut remote = d.remote.lock().unwrap();
            let f = child.forest();
            let id = child.slot().id;
            let live = f.record(id).live;
            let target = f.record(parent.slot().id).live.inbox;
            f.retain(id);
            f.record_mut(id).parent = Some(live.parent.unwrap().slot().id);
            remote.enqueue_handover(child, unused, target);
            if live.inbox != target {
                remote.reroute(live.inbox);
            }
        });
    }

    fn end_id(&self) -> usize {
        let f = self.payer.get().forest();
        let child = f.find(self.payer.get().slot().id);
        let original_parent = f.record(child).live.parent.unwrap().slot().id;
        let unused = f.record(child).available;
        f.merge(child, original_parent, unused)
    }

    /// Move a retained forwarding handle to its canonical record at a context switch.
    #[inline(always)]
    pub(crate) fn normalize(&self) {
        // SAFETY: Funding handles contain untagged slot pointers, unlike block words.
        if unsafe { (*self.raw_local()).parent.is_some() } {
            self.normalize_merged();
        }
    }
    #[cold]
    fn normalize_merged(&self) {
        let canonical = self.payer.get().canonical();
        let f = self.payer.get().forest();
        f.retain(canonical.slot().id);
        f.release(self.payer.get().slot().id);
        self.payer.set(canonical);
    }
    #[inline(always)]
    pub(crate) fn raw_local(&self) -> *mut LimitRecord {
        self.payer.get().0.as_ptr().cast()
    }
    #[inline(always)]
    fn reserve_scope(&self, bytes: u64) -> Result<Live, Capacity> {
        // Active contexts always hold a canonical record. A reservation moves
        // available bytes, not outstanding charges, and transfers the parent's pin.
        let parent = self.payer.get();
        #[cfg(any(test, feature = "testing"))]
        let injected = {
            let f = parent.forest();
            f.attempts += 1;
            f.fail
                .is_some_and(|(n, later)| f.attempts == n || later && f.attempts >= n)
        };
        #[cfg(not(any(test, feature = "testing")))]
        let injected = false;
        // SAFETY: the task exclusively owns this live canonical context record.
        let r = unsafe { &mut *self.raw_local() };
        if injected || bytes > r.available {
            return Err(crate::tasks::capacity_refused(bytes, r.available));
        }
        r.available = r.available.wrapping_sub(bytes);
        let live = Live {
            owner: r.live.owner,
            inbox: r.live.inbox,
            capacity: bytes,
            parent: Some(parent),
        };
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::sample();
        Ok(live)
    }
    #[inline(always)]
    pub(crate) fn enter(&mut self, bytes: u64) -> Result<(), Capacity> {
        let live = self.reserve_scope(bytes)?;
        // SAFETY: an active Funding handle is untagged and task-exclusive.
        let domain = unsafe { self.payer.get().0.as_ref().domain };
        // SAFETY: that same execution custody owns this domain's local table.
        let forest = unsafe { &mut *domain.as_ref().forest.get() };
        self.payer.set(forest.take(domain, live));
        Ok(())
    }
    #[inline(always)]
    pub(crate) fn reusable_slot(&self) -> Option<NonNull<Slot>> {
        // SAFETY: the scheduler gives the context exclusive local execution.
        unsafe { (*self.payer.get().0.as_ref().domain.as_ref().forest.get()).free }
    }
    #[inline(always)]
    pub(crate) fn enter_reused(&mut self, bytes: u64, slot: NonNull<Slot>) -> Result<(), Capacity> {
        let live = self.reserve_scope(bytes)?;
        // SAFETY: caller just selected this context's free-list head; reserve_scope
        // changes no pool links and cannot suspend or call a provider.
        unsafe {
            let f = &mut *self.payer.get().0.as_ref().domain.as_ref().forest.get();
            f.free = (*slot.as_ref().local.get()).next_free;
        }
        self.payer.set(Forest::initialize(slot, live));
        Ok(())
    }
    #[inline(always)]
    pub(crate) fn exit(&mut self) {
        // SAFETY: the active context pins its canonical record exclusively.
        let r = unsafe { &mut *self.raw_local() };
        if r.refs != 1 || r.available != r.live.capacity {
            return self.exit_survivors();
        }
        let parent = r.live.parent.unwrap();
        // SAFETY: lexical parent identities are untagged, retained slot pointers.
        if unsafe { (*parent.0.as_ref().local.get()).parent.is_some() } {
            return self.exit_forwarded_parent();
        }
        let unused = self.recycle_scope(parent);
        // SAFETY: the parent edge's existing pin becomes this context's pin.
        unsafe {
            (*self.raw_local()).available = (*self.raw_local()).available.wrapping_add(unused);
        }
    }

    /// The rank-zero one-survivor union has a fixed result: the lexical parent
    /// remains representative, so it can transfer the context pin without
    /// running the general rank/metadata machinery.
    #[cold]
    fn exit_rank_zero_survivor(&mut self, parent: Payer) -> bool {
        let child = self.payer.get();
        let child_id = child.slot().id;
        let parent_id = parent.slot().id;
        let f = child.forest();
        let child_record = f.record(child_id);
        let parent_record = f.record(parent_id);
        let child_parent = child_record.parent;
        let child_rank = child_record.rank;
        let child_refs = child_record.refs;
        let child_outstanding = child_record.outstanding;
        let child_owner = child_record.live.owner;
        let parent_parent = parent_record.parent;
        let parent_owner = parent_record.live.owner;
        let parent_rank = parent_record.rank;
        let parent_available = parent_record.available;
        if child_parent.is_some()
            || child_rank != 0
            || child_refs != 1
            || child_outstanding == 0
            || parent_parent.is_some()
            || child_owner != crate::tasks::current_id()
            || parent_owner != crate::tasks::current_id()
        {
            return false;
        }
        let unused = child_record.available;
        f.record_mut(parent_id).available = parent_available.wrapping_add(unused);
        if parent_rank == 0 {
            f.record_mut(parent_id).rank += 1;
        }
        f.record_mut(child_id).parent = Some(parent_id);
        f.record_mut(child_id).refs = 0;
        f.record_mut(parent_id).refs += 1;
        self.payer.set(parent);
        true
    }
    #[inline(always)]
    fn recycle_scope(&mut self, parent: Payer) -> u64 {
        let old = self.payer.get();
        // SAFETY: the sole context pin and full room prove this record has no survivors.
        unsafe {
            let r = &mut *self.raw_local();
            let unused = r.available;
            let f = &mut *old.0.as_ref().domain.as_ref().forest.get();
            // `Forest::initialize` resets rank before the slot can be observed
            // again. Keeping this free-list path from writing it avoids one
            // redundant store on every empty-scope exit.
            r.live
                .inbox
                .as_ref()
                .release(old.0.as_ref().domain.as_ref());
            r.live = Live::EMPTY;
            #[cfg(any(test, feature = "testing"))]
            {
                r.occupied = false;
            }
            r.next_free = f.free;
            f.free = Some(old.0);
            self.payer.set(parent);
            unused
        }
    }
    #[cold]
    fn exit_forwarded_parent(&mut self) {
        // SAFETY: exit selected a live lexical scope whose retained parent merged.
        let parent = unsafe { (*self.raw_local()).live.parent.unwrap_unchecked() };
        let unused = self.recycle_scope(parent);
        self.normalize_merged();
        // SAFETY: normalization transferred the context's sole pin to the winner.
        unsafe {
            (*self.raw_local()).available = (*self.raw_local()).available.wrapping_add(unused);
        }
    }
    #[cold]
    fn exit_survivors(&mut self) {
        // SAFETY: exit owns the canonical scope and its retained lexical parent.
        let parent = unsafe { (*self.raw_local()).live.parent.unwrap() };
        if self.exit_rank_zero_survivor(parent) {
            return;
        }
        self.exit_general();
    }
    fn exit_general(&mut self) {
        let old = self.payer.get();
        let winner = self.end_id();
        let f = old.forest();
        f.retain(winner);
        f.release(old.slot().id);
        self.payer.set(Payer(NonNull::from(f.slot(winner))));
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn inject(&self, n: usize, later: bool) {
        let f = self.payer.get().forest();
        f.attempts = 0;
        f.fail = Some((n, later));
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn attempts(&self) -> usize {
        self.payer.get().forest().attempts
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn records(&self) -> usize {
        let f = self.payer.get().forest();
        (0..f.length).filter(|&id| f.record(id).occupied).count()
    }

    #[cfg(test)]
    pub(crate) fn scope_debug(&self) -> (usize, usize, usize, usize, u64) {
        let f = self.payer.get().forest();
        let mut occupied = 0;
        let mut forwarding = 0;
        let mut charged_slots = 0;
        let mut charged_bytes = 0u64;
        for id in 0..f.length {
            let record = f.record(id);
            if !record.occupied {
                continue;
            }
            occupied += 1;
            forwarding += usize::from(record.parent.is_some());
            if record.outstanding != 0 {
                charged_slots += 1;
                charged_bytes += record.outstanding;
            }
        }
        (f.length, occupied, forwarding, charged_slots, charged_bytes)
    }
}
impl Clone for Funding {
    fn clone(&self) -> Self {
        let d = self.payer.get().domain();
        d.handles.set(d.handles.get() + 1);
        self.payer.get().forest().retain(self.payer.get().slot().id);
        Self {
            payer: Cell::new(self.payer.get()),
        }
    }
}
impl Drop for Funding {
    fn drop(&mut self) {
        let domain = self.payer.get().slot().domain;
        let d = self.payer.get().domain();
        d.handles.set(d.handles.get() - 1);
        if d.handles.get() != 0 {
            self.payer
                .get()
                .forest()
                .release(self.payer.get().slot().id);
            return;
        }
        CURRENT_DOMAIN.with(|v| {
            if v.get() == domain.as_ptr().addr() {
                v.set(0);
            }
        });
        let mut remote = d.remote.lock().unwrap();
        remote.retired = true;
        let f = self.payer.get().forest();
        f.retired_bytes = (0..f.length).map(|id| f.record(id).outstanding).sum();
        self.drain_locked(&mut remote, true);
        self.payer
            .get()
            .forest()
            .release(self.payer.get().slot().id);
        let empty =
            self.payer.get().forest().retired_bytes == 0 && self.payer.get().forest().zeros == 0;
        drop(remote);
        if empty {
            // SAFETY: the last handle and all charged values have been settled.
            unsafe {
                drop(Box::from_raw(domain.as_ptr()));
            }
        }
    }
}

#[cfg(test)]
#[path = "limit_refund_tests.rs"]
mod refund_tests;

#[cfg(test)]
#[path = "limit_bound_tests.rs"]
mod bound_tests;

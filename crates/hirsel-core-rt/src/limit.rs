//! Task-local limit table. Bytes retain payer slots; only scope handles
//! and union edges use structural pins. The free list retains each unused slot's
//! pin for its next scope. Remote events never touch local fields.
//! Native balance updates wrap because admission/refund invariants already prove
//! their range; they do not add guards against incorrect generated bookkeeping.
//! User-visible size arithmetic, Capacity and count-increment overflow stay checked.
use crate::Capacity;
#[path = "limit_refunds.rs"]
mod refunds;
use refunds::{Inbox, Queue, Remote};
use std::{
    cell::{Cell, RefCell, UnsafeCell},
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
}

// Shared routing and lifetime data is separate from the ABI balances. A
// foreign commit can inspect links or change pins while the owner debits.
struct Links {
    refs: usize,
    // The union edge owns a pin, just like the lexical parent in Live.
    parent: Option<Payer>,
    rank: u32,
    live: Live,
    next_free: Option<NonNull<Slot>>,
    #[cfg(any(test, feature = "testing"))]
    occupied: bool,
}
#[repr(C, align(32))]
pub(crate) struct Slot {
    local: UnsafeCell<LimitRecord>,
    /// Frozen at SLOT_DOMAIN. Set before the slot is published and never
    /// changed, so a live payer word always leads to its domain.
    domain: NonNull<Domain>,
    links: UnsafeCell<Links>,
    pending: AtomicU64,
    pending_zeros: AtomicUsize,
    next_pending: UnsafeCell<Option<NonNull<Slot>>>,
    /// The dirty slots this record leads, while it is a union-find root.
    queue: UnsafeCell<Queue>,
    handover: Cell<Option<u64>>,
    #[cfg(test)]
    id: usize,
}
const _: () = assert!(std::mem::offset_of!(Slot, local) == crate::abi_layout::LIMIT_AVAILABLE);
const _: () = assert!(std::mem::offset_of!(Slot, domain) == crate::abi_layout::SLOT_DOMAIN);
const _: () = assert!(std::mem::offset_of!(Domain, pending) == crate::abi_layout::DOMAIN_PENDING);

impl Slot {
    fn vacant(domain: NonNull<Domain>, _id: usize) -> Self {
        Self {
            local: UnsafeCell::new(LimitRecord {
                available: 0,
                outstanding: 0,
            }),
            links: UnsafeCell::new(Links {
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
            queue: UnsafeCell::new(Queue::EMPTY),
            handover: Cell::new(None),
            domain,
            #[cfg(test)]
            id: _id,
        }
    }
}

const PAGE_LEN: usize = 64;

#[derive(Default)]
struct SmallChunks {
    one: Option<Box<[Slot; 1]>>,
    two: Option<Box<[Slot; 2]>>,
    four: Option<Box<[Slot; 4]>>,
    eight: Option<Box<[Slot; 8]>>,
    sixteen: Option<Box<[Slot; 16]>>,
    thirty_two: Option<Box<[Slot; 32]>>,
}
impl SmallChunks {
    fn get_or_allocate(&mut self, domain: NonNull<Domain>, start: usize) -> &[Slot] {
        match start {
            1 => self
                .one
                .get_or_insert_with(|| Forest::allocate_slots(domain, start))
                .as_slice(),
            2 => self
                .two
                .get_or_insert_with(|| Forest::allocate_slots(domain, start))
                .as_slice(),
            4 => self
                .four
                .get_or_insert_with(|| Forest::allocate_slots(domain, start))
                .as_slice(),
            8 => self
                .eight
                .get_or_insert_with(|| Forest::allocate_slots(domain, start))
                .as_slice(),
            16 => self
                .sixteen
                .get_or_insert_with(|| Forest::allocate_slots(domain, start))
                .as_slice(),
            32 => self
                .thirty_two
                .get_or_insert_with(|| Forest::allocate_slots(domain, start))
                .as_slice(),
            _ => unreachable!(),
        }
    }

    fn slices(&self) -> [&[Slot]; 6] {
        [
            self.one.as_deref().map_or(&[], |chunk| chunk.as_slice()),
            self.two.as_deref().map_or(&[], |chunk| chunk.as_slice()),
            self.four.as_deref().map_or(&[], |chunk| chunk.as_slice()),
            self.eight.as_deref().map_or(&[], |chunk| chunk.as_slice()),
            self.sixteen
                .as_deref()
                .map_or(&[], |chunk| chunk.as_slice()),
            self.thirty_two
                .as_deref()
                .map_or(&[], |chunk| chunk.as_slice()),
        ]
    }
}

struct Forest {
    // Points to the first slot in the same domain allocation.
    first: NonNull<Slot>,
    // Each small chunk starts at the ID equal to its length. Together with
    // the inline first slot they fill the ID range of one page.
    small: SmallChunks,
    pages: Vec<Box<[Slot; PAGE_LEN]>>,
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
    fn first(&self) -> &Slot {
        // SAFETY: the enclosing domain owns this slot and outlives the forest.
        unsafe { self.first.as_ref() }
    }

    fn slots(&self) -> impl Iterator<Item = &Slot> {
        std::iter::once(self.first())
            .chain(self.small.slices().into_iter().flatten())
            .chain(self.pages.iter().flat_map(|page| page.iter()))
            .take(self.length)
    }

    fn outstanding(&self) -> u64 {
        // SAFETY: retirement has exclusive custody of all local balances.
        if self.length == 1 {
            unsafe { (*self.first().local.get()).outstanding }
        } else {
            self.slots()
                .map(|slot| unsafe { (*slot.local.get()).outstanding })
                .sum()
        }
    }

    #[inline(always)]
    fn record(&self, payer: Payer) -> &Links {
        // SAFETY: the payer is pinned and the caller owns routing through the
        // sequencer or retired mailbox.
        unsafe { &*payer.links() }
    }
    #[inline(always)]
    fn record_mut(&mut self, payer: Payer) -> &mut Links {
        // SAFETY: as record, with exclusive access to the links.
        unsafe { &mut *payer.links() }
    }
    #[inline]
    fn retain(&mut self, payer: Payer) {
        self.record_mut(payer).refs += 1;
    }
    fn release(&mut self, mut payer: Payer) {
        loop {
            let r = self.record_mut(payer);
            r.refs -= 1;
            if r.refs != 0 || unsafe { (*payer.local()).outstanding } != 0 {
                return;
            }
            let parent = r.parent.or(r.live.parent);
            // SAFETY: a queued slot pins its root through its charge or edge.
            debug_assert!(unsafe { (*payer.slot().queue.get()).is_empty() });
            // The free list takes over the final slot pin until its next use.
            r.refs = 1;
            r.parent = None;
            r.rank = 0;
            // SAFETY: the initialized record and its domain stay live through release.
            unsafe { r.live.inbox.as_ref().release(payer.domain()) };
            r.live = Live::EMPTY;
            #[cfg(any(test, feature = "testing"))]
            {
                r.occupied = false;
            }
            self.return_vacant(payer.0);
            if let Some(parent) = parent {
                payer = parent;
            } else {
                return;
            }
        }
    }
    #[cold]
    fn fresh(&mut self, domain: NonNull<Domain>) -> NonNull<Slot> {
        let id = self.length;
        let slot = if id >= PAGE_LEN {
            if id.is_multiple_of(PAGE_LEN) {
                self.pages.push(Self::allocate_slots(domain, id));
            }
            &self.pages.last().unwrap()[id % PAGE_LEN]
        } else {
            let start = 1 << id.ilog2();
            &self.small.get_or_allocate(domain, start)[id - start]
        };
        self.length += 1;
        NonNull::from(slot)
    }

    fn allocate_slots<const N: usize>(domain: NonNull<Domain>, start: usize) -> Box<[Slot; N]> {
        let mut slots = Box::<[Slot; N]>::new_uninit();
        let elements = slots.as_mut_ptr().cast::<Slot>();
        for i in 0..N {
            // SAFETY: elements points to the start of the N-slot allocation.
            unsafe { elements.add(i).write(Slot::vacant(domain, start + i)) };
        }
        // SAFETY: the loop initialized every element in the allocation.
        unsafe { slots.assume_init() }
    }

    fn pop_vacant(&mut self) -> Option<NonNull<Slot>> {
        let slot = self.free?;
        // SAFETY: sequencer custody uniquely owns each vacant slot.
        self.free = unsafe { (*slot.as_ref().links.get()).next_free };
        Some(slot)
    }

    fn return_vacant(&mut self, slot: NonNull<Slot>) {
        // SAFETY: the caller transfers this unoccupied slot's lease.
        unsafe { (*slot.as_ref().links.get()).next_free = self.free };
        self.free = Some(slot);
    }

    #[inline(always)]
    fn take(&mut self, domain: NonNull<Domain>, live: Live) -> Payer {
        let slot = VACANT
            .with_borrow_mut(|cached| {
                cached
                    .as_mut()
                    .filter(|cache| cache.domain == domain)
                    .and_then(|cache| {
                        if cache.slots.is_empty() {
                            cache.refill(self);
                        }
                        cache.slots.pop()
                    })
            })
            .or_else(|| self.pop_vacant())
            .unwrap_or_else(|| self.fresh(domain));
        Self::initialize(slot, live)
    }
    #[inline(always)]
    fn initialize(slot: NonNull<Slot>, live: Live) -> Payer {
        // SAFETY: the free list transfers its sole pin; charge and union rank are zero.
        unsafe { (*slot.as_ref().local.get()).available = live.capacity };
        let r = unsafe { &mut *slot.as_ref().links.get() };
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
    fn root(&self, mut payer: Payer) -> Payer {
        while let Some(parent) = self.record(payer).parent {
            payer = parent;
        }
        payer
    }
    #[inline(always)]
    fn find(&mut self, payer: Payer) -> Payer {
        match self.record(payer).parent {
            None => payer,
            Some(parent) if self.record(parent).parent.is_none() => parent,
            Some(_) => self.compress(payer),
        }
    }
    #[cold]
    fn compress(&mut self, payer: Payer) -> Payer {
        let root = self.root(payer);
        let mut node = payer;
        while let Some(p) = self.record(node).parent {
            if p == root {
                break;
            }
            self.retain(root);
            self.record_mut(node).parent = Some(root);
            // Transfer the old edge to this traversal until its next step.
            if node != payer {
                self.release(node);
            }
            node = p;
        }
        if node != payer {
            self.release(node);
        }
        root
    }
    /// Join the records and return (winner, loser). The caller gives the
    /// winner the loser's queued slots (`Remote::unite`).
    fn merge(&mut self, child: Payer, original_parent: Payer, unused: u64) -> (Payer, Payer) {
        let parent = self.find(original_parent);
        // SAFETY: the recipient owns both the original parent and the merged
        // record through this fold.
        let available = unsafe { (*parent.local()).available };
        #[cfg(feature = "prof")]
        parent.observe(available);
        let parent_available = available.wrapping_add(unused);
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
                previous_inbox.as_ref().release(winner.domain());
            }
        }
        self.record_mut(winner).live = parent_live;
        unsafe { (*winner.local()).available = parent_available };
        #[cfg(feature = "prof")]
        crate::mem_prof::forward(parent.local().addr(), winner.local().addr());
        self.retain(winner);
        self.record_mut(loser).parent = Some(winner);
        self.release(original_parent);
        (winner, loser)
    }

    fn refund(&mut self, payer: Payer, bytes: u64) {
        if bytes == 0 {
            self.zeros -= 1;
            self.release(payer);
        } else {
            let root = self.find(payer);
            self.refund_rooted(payer, root, bytes);
        }
    }
    #[inline(always)]
    fn refund_rooted(&mut self, payer: Payer, root: Payer, bytes: u64) {
        if bytes == 0 {
            self.zeros -= 1;
            self.release(payer);
            return;
        }
        // SAFETY: the recipient owns credits along this ordered route.
        let outstanding = unsafe {
            let recipient = root.local();
            #[cfg(feature = "prof")]
            root.observe((*recipient).available);
            (*recipient).available = (*recipient).available.wrapping_add(bytes);
            let original = payer.local();
            (*original).outstanding = (*original).outstanding.wrapping_sub(bytes);
            (*original).outstanding
        };
        if self.record(payer).refs == 0 && outstanding == 0 {
            self.record_mut(payer).refs = 1;
            self.release(payer);
        }
    }
}
#[repr(C)]
struct Domain {
    /// Frozen at DOMAIN_PENDING: set with every posted refund and lowered
    /// only when the domain has none queued. Emitted Arc events read it
    /// through the payer's slot and skip their hook while it is clear.
    pending: AtomicBool,
    forest: UnsafeCell<Forest>,
    handles: Cell<usize>,
    owner: Option<usize>,
    remote: Mutex<Remote>,
    reap_inboxes: Cell<Option<NonNull<Inbox>>>,
    /// Admitted bytes whose backing storage stays until run teardown.
    run_owned: AtomicU64,
    first: UnsafeCell<Slot>,
}
#[derive(Clone, Copy)]
struct Binding {
    domain: NonNull<Domain>,
    inbox: NonNull<Inbox>,
}

const SLOT_BATCH: usize = 16;
const SLOT_CACHE: usize = 2 * SLOT_BATCH;

#[derive(Clone, Copy)]
pub(crate) struct CacheDomain(NonNull<Domain>);
// SAFETY: the parent retains the domain through scoped worker teardown. The
// worker only changes vacant slots under the domain sequencer.
unsafe impl Send for CacheDomain {}
unsafe impl Sync for CacheDomain {}

impl CacheDomain {
    pub(crate) fn lease_vacant(self) {
        VACANT.set(Some(VacantSlots {
            domain: self.0,
            slots: Vec::with_capacity(SLOT_CACHE),
            stats: SlotStats::default(),
        }));
    }
}

struct VacantSlots {
    domain: NonNull<Domain>,
    slots: Vec<NonNull<Slot>>,
    stats: SlotStats,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SlotStats {
    pub transfers: usize,
    pub leased: usize,
    pub returned: usize,
    pub peak: usize,
    pub at_exit: usize,
}

impl VacantSlots {
    fn refill(&mut self, forest: &mut Forest) {
        // Lease only records already returned to the forest. A cache miss
        // with no vacancies creates just the requested record.
        while self.slots.len() < SLOT_BATCH
            && let Some(slot) = forest.pop_vacant()
        {
            self.slots.push(slot);
        }
        let count = self.slots.len();
        if count != 0 {
            self.stats.transfers += 1;
            self.stats.leased += count;
            self.stats.peak = self.stats.peak.max(count);
        }
    }

    fn return_batch(&mut self, forest: &mut Forest, count: usize) {
        if count != 0 {
            self.stats.transfers += 1;
            self.stats.returned += count;
            for slot in self.slots.drain(self.slots.len() - count..) {
                forest.return_vacant(slot);
            }
        }
    }
}

thread_local! {
    static THREAD: u8 = const { 0 };
    static VACANT: RefCell<Option<VacantSlots>> = const { RefCell::new(None) };
}

crate::units::moved_tls! { static BOUND: Option<Binding> = None; }

pub(crate) fn preserve_binding<R>(run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Binding>);
    impl Drop for Restore {
        fn drop(&mut self) {
            Funding::unbind();
            BOUND.set(self.0);
        }
    }
    // The saved binding keeps its existing pin until it is restored.
    let _restore = Restore(BOUND.take());
    run()
}
#[inline(always)]
fn thread() -> usize {
    // mc-units-untied: a unit is the owner, whichever worker runs it.
    if crate::units::untied() {
        return crate::units::identity();
    }
    THREAD.with(|v| v as *const u8 as usize)
}
#[inline(always)]
pub(crate) fn check_unbound() {
    if BOUND.get_here().is_some() || VACANT.with_borrow(Option::is_some) {
        crate::process::stop(b"UntiedParkBound");
    }
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
    #[allow(clippy::mut_from_ref)] // Live access is sequenced; retired access holds the mailbox.
    fn forest(self) -> &'static mut Forest {
        // SAFETY: callers hold the commit, or the retired mailbox mutex.
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
    #[cfg(feature = "prof")]
    fn observe(self, available: u64) {
        crate::mem_prof::slot(self.local().addr(), available);
    }
    fn links(self) -> *mut Links {
        self.slot().links.get()
    }
    #[inline(always)]
    pub(crate) fn canonical(self) -> Self {
        let _commit = crate::sequencer::enter();
        // SAFETY: local task exclusively owns the local record fields.
        if unsafe { (*self.links()).parent.is_none() } {
            self
        } else {
            self.forest().find(self)
        }
    }
    fn root(self) -> Self {
        self.forest().root(self)
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
                #[cfg(feature = "prof")]
                payer.observe(r.available);
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
    pub(crate) fn admit_run_owned(self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.admit(bytes)?;
        self.domain()
            .run_owned
            .fetch_add(bytes.unwrap(), Ordering::Relaxed);
        Ok(())
    }

    /// The helper reads this after all of its tasks have ended.
    pub(crate) fn run_owned(self) -> u64 {
        self.domain().run_owned.load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn injected(self) -> bool {
        let _commit = crate::sequencer::enter();
        let f = self.forest();
        f.attempts += 1;
        f.fail
            .is_some_and(|(n, later)| f.attempts == n || later && f.attempts >= n)
    }
    pub(crate) fn pin_zero(self) {
        let _commit = crate::sequencer::enter();
        let f = self.forest();
        f.retain(self);
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
                #[cfg(feature = "prof")]
                self.observe(r.available);
                r.available = r.available.wrapping_add(bytes);
                r.outstanding = r.outstanding.wrapping_sub(bytes);
            }
        } else {
            self.settle(bytes);
        }
    }
    /// A model unit's refund (units_ledger). A model unit's table is
    /// touched only by the unit running on it (its helpers run one at a time
    /// with it, in the checker), so it takes no commit and credits the
    /// payer's root at once. Never zero bytes: a zero-byte charge holds no pin.
    #[inline]
    pub(crate) fn unit_refund(self, bytes: u64) {
        debug_assert!(bytes != 0, "a zero-byte unit refund");
        // SAFETY: as above; a root that keeps a pin or other bytes is
        // credited in place, as `refund_rooted` does with `root == id`.
        unsafe {
            let links = &*self.links();
            let r = &mut *self.local();
            if links.parent.is_none() && (links.refs != 0 || r.outstanding != bytes) {
                #[cfg(feature = "prof")]
                self.observe(r.available);
                r.available = r.available.wrapping_add(bytes);
                r.outstanding = r.outstanding.wrapping_sub(bytes);
                return;
            }
        }
        let f = self.forest();
        let root = f.find(self);
        f.refund_rooted(self, root, bytes);
    }
    /// A record's (used, capacity): what its live limit has handed out.
    pub(crate) fn usage(self) -> (u64, u64) {
        let p = self.canonical();
        let _commit = crate::sequencer::enter();
        // SAFETY: the caller's handle or outstanding bytes pin this record.
        unsafe {
            let capacity = (*p.links()).live.capacity;
            (capacity - (*p.local()).available, capacity)
        }
    }
    /// Connection charges settle in their owning unit at the release or take.
    pub(crate) fn settle_at_unit(self, bytes: u64) {
        let _commit = crate::sequencer::enter();
        if !crate::tasks::parallel() && self.domain().owner == Some(thread()) {
            self.forest().refund(self, bytes);
        } else {
            self.settle(bytes);
        }
    }

    #[inline]
    pub(crate) fn settle(self, bytes: u64) {
        let _commit = crate::sequencer::enter();
        let d = self.domain();
        let local = BOUND
            .get()
            .is_some_and(|binding| binding.domain == self.slot().domain)
            || (!crate::tasks::parallel() && d.owner == Some(thread()));
        if local && d.handles.get() != 0 {
            let mut root = self.root();
            // SAFETY: scheduler execution serializes reads of live routing.
            let mut live = unsafe { (*root.links()).live };
            if bytes != 0 && live.owner != crate::tasks::current_id() {
                let _commit = crate::tasks::before_commit();
                root = self.root();
                // SAFETY: an outstanding charge pins the current route.
                live = unsafe { (*root.links()).live };
            }
            if live.owner != crate::tasks::current_id() {
                let publish = || d.remote.lock().unwrap().enqueue(self, bytes, Some(root));
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
            let link = unsafe { &*self.links() };
            if bytes != 0 && link.parent.is_none() && link.refs != 0 {
                let r = unsafe { &mut *self.local() };
                #[cfg(feature = "prof")]
                self.observe(r.available);
                r.available = r.available.wrapping_add(bytes);
                r.outstanding = r.outstanding.wrapping_sub(bytes);
            } else {
                self.forest().refund_rooted(self, root, bytes);
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
            f.refund(self, bytes);
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
        let _commit = crate::sequencer::enter();
        let parent = self.canonical();
        let f = self.forest();
        // SAFETY: this reservation draws from the payer's current owner.
        unsafe { (*parent.local()).outstanding -= bytes };
        f.retain(parent);
        let live = f.record(parent).live;
        let payer = f.take(
            self.slot().domain,
            Live {
                capacity: bytes,
                parent: Some(parent),
                ..live
            },
        );
        self.domain().handles.set(self.domain().handles.get() + 1);
        Funding {
            payer: Cell::new(payer),
        }
    }

    /// Hand a reservation's record over to the scope it leads to, returning the
    /// cell's charge with it. The record is its own root until this handover
    /// and its cell is its only charge, so the charge goes back to the record's
    /// balance and the merge hands it to the live limit together with the unused
    /// bytes (MEM-20), whichever task drops the cell: never to the inbox of the
    /// task that reserved it, which may have ended.
    pub(crate) fn finish_reserved(self, charge: u64) {
        let _commit = crate::sequencer::enter();
        // SAFETY: the cell owns this unmerged record; no other handle charges it.
        unsafe {
            let r = &mut *self.local();
            debug_assert!((*self.links()).parent.is_none() && r.outstanding == charge);
            r.available += charge;
            r.outstanding = 0;
        }
        let funding = Funding {
            payer: Cell::new(self),
        };
        // The hand-over merges the record or leaves a prepaid queue pin for
        // the recipient. The reservation cell releases its own pin in either case.
        funding.end();
        funding.payer.get().forest().release(self);
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
    /// Context drops), and the merge releases that edge when the cell finishes.
    pub(crate) fn isolate_reserve(&mut self, bytes: u64) -> Result<Payer, Capacity> {
        let _commit = crate::sequencer::enter();
        let live = self.reserve_scope(bytes)?;
        let domain = unsafe { self.payer.get().0.as_ref().domain };
        let forest = unsafe { &mut *domain.as_ref().forest.get() };
        forest.retain(self.payer.get());
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
        let domain = Box::new(Domain {
            forest: UnsafeCell::new(Forest {
                first: NonNull::dangling(),
                small: Default::default(),
                pages: Vec::new(),
                length: 1,
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
            reap_inboxes: Cell::new(None),
            run_owned: AtomicU64::new(0),
            first: UnsafeCell::new(Slot::vacant(NonNull::dangling(), 0)),
        });
        let domain = NonNull::from(Box::leak(domain));
        // SAFETY: nothing refers to this fresh domain yet. The slot and its
        // back pointer are initialized in their final locations before use.
        let first = unsafe {
            let domain_ref = domain.as_ref();
            let first = NonNull::new_unchecked(domain_ref.first.get());
            (*first.as_ptr()).domain = domain;
            (*domain_ref.forest.get()).first = first;
            first
        };
        let payer = Forest::initialize(
            first,
            Live {
                owner,
                inbox,
                capacity,
                parent: None,
            },
        );
        Self {
            payer: Cell::new(payer),
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
        let _commit = crate::sequencer::enter();
        let payer = self.payer.get().canonical();
        let domain = payer.slot().domain;
        // SAFETY: the live record pins its inbox through this dispatch binding.
        let inbox = unsafe { (*payer.links()).live.inbox };
        let previous = BOUND.get();
        if previous.is_some_and(|old| old.inbox == inbox) {
            return;
        }
        // Pin until unbind, including while a hand-over moves the last record.
        unsafe { inbox.as_ref().retain() };
        BOUND.set(Some(Binding { domain, inbox }));
        if let Some(old) = previous {
            // SAFETY: the old binding held the inbox pin until this replacement.
            unsafe { old.inbox.as_ref().release(old.domain.as_ref()) };
        }
    }
    pub(crate) fn unbind() {
        let _commit = crate::sequencer::enter();
        if let Some(binding) = BOUND.take() {
            // SAFETY: the task that owns this binding remains pinned until unbind.
            unsafe { binding.inbox.as_ref().release(binding.domain.as_ref()) };
        }
    }
    /// Pin a bounded vacant-slot cache to the root funding domain for this
    /// worker's lifetime. The run joins all workers before dropping the root.
    pub(crate) fn cache_domain(&self) -> CacheDomain {
        CacheDomain(self.payer.get().slot().domain)
    }

    pub(crate) fn return_vacant() -> SlotStats {
        let _commit = crate::sequencer::enter();
        let Some(mut cache) = VACANT.take() else {
            return SlotStats::default();
        };
        // SAFETY: run startup pins the domain through every worker's return.
        let forest = unsafe { &mut *cache.domain.as_ref().forest.get() };
        let count = cache.slots.len();
        cache.stats.at_exit = count;
        cache.return_batch(forest, count);
        cache.stats
    }

    /// The word `drain` tests on this thread, for a commit served by another
    /// worker on this owner's behalf.
    #[inline(always)]
    pub(crate) fn pending_word(&self) -> *const AtomicBool {
        let domain = self.payer.get().slot().domain;
        // SAFETY: the current Funding and dispatch binding pin their state.
        unsafe {
            match BOUND.get().filter(|binding| binding.domain == domain) {
                Some(binding) => binding.inbox.as_ref().pending_word(),
                None => &domain.as_ref().pending,
            }
        }
    }

    /// Apply only the running owner's ordered refunds. No admission or allocation.
    #[inline(always)]
    pub fn drain(&self) {
        let domain = self.payer.get().slot().domain;
        // SAFETY: the current Funding and dispatch binding pin their state.
        let pending = unsafe {
            match BOUND.get().filter(|binding| binding.domain == domain) {
                Some(binding) => binding.inbox.as_ref().pending(),
                None => domain.as_ref().pending.load(Ordering::Acquire),
            }
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
        let _commit = crate::sequencer::enter();
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
            let payer = Payer(slot);
            let slot = payer.slot();
            if let Some(unused) = slot.handover.take() {
                let original_parent = f.record_mut(payer).parent.take().unwrap();
                let (winner, loser) = f.merge(payer, original_parent, unused);
                remote.unite(winner.0, loser.0);
                f.release(payer);
            }
            let bytes = slot.pending.swap(0, Ordering::Relaxed);
            let zeros = slot.pending_zeros.swap(0, Ordering::Relaxed);
            if bytes != 0 {
                f.refund(payer, bytes);
                if retired {
                    f.retired_bytes -= bytes;
                }
            }
            for _ in 0..zeros {
                f.refund(payer, 0);
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
        let _commit = crate::sequencer::enter();
        // SAFETY: the run's root handle pins its canonical live limit record.
        unsafe { (*self.payer.get().canonical().links()).live.capacity }
    }
    #[cfg(feature = "testing")]
    pub(crate) fn set_test_available(&self, bytes: u64) {
        let _commit = crate::sequencer::enter();
        // SAFETY: the process fixture exclusively owns its live root record.
        let payer = self.payer.get().canonical();
        let record = unsafe { &mut *payer.local() };
        let live = unsafe { &mut (*payer.links()).live };
        let reserved = live.capacity.wrapping_sub(record.available);
        // A sample may choose u64::MAX after startup. Its synthetic allowance
        // and prepaid startup use the same modular balances as the fixture.
        live.capacity = bytes.wrapping_add(reserved);
        record.available = bytes;
    }
    pub(crate) fn admit(&self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.payer.get().canonical().admit(bytes)
    }
    /// A model unit's admission (units_ledger): no injection and no capacity
    /// event; a refusal returns what is available. The bytes stay
    /// outstanding on the returned canonical payer until `unit_refund`.
    #[inline]
    pub(crate) fn unit_charge(&self, bytes: u64) -> Result<Payer, u64> {
        let payer = self.payer.get();
        // SAFETY: as in `unit_refund`, only this unit reads its links.
        let payer = if unsafe { (*payer.links()).parent.is_none() } {
            payer
        } else {
            payer.canonical()
        };
        // SAFETY: the unit's handle pins its canonical local record.
        let r = unsafe { &mut *payer.local() };
        if bytes > r.available {
            return Err(r.available);
        }
        r.available -= bytes;
        r.outstanding += bytes;
        Ok(payer)
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
        let _commit = crate::sequencer::enter();
        let payer = self.payer.get();
        payer.forest().record(payer).live.owner
    }
    pub(crate) fn task_owner(&self, owner: u64) {
        let _commit = crate::sequencer::enter();
        let payer = self.payer.get().canonical();
        let old = payer.forest().record(payer).live;
        if old.owner != owner {
            let domain = payer.domain();
            let mut remote = domain.remote.lock().unwrap();
            remote.reap(domain);
            let inbox = remote.inbox(owner);
            // SAFETY: the replacing live record takes over this inbox pin.
            unsafe { inbox.as_ref().retain() };
            let live = &mut payer.forest().record_mut(payer).live;
            live.owner = owner;
            live.inbox = inbox;
            // SAFETY: the map retains the previous inbox until the next reap.
            unsafe { old.inbox.as_ref().release(domain) };
            remote.relink(payer.0, old.inbox);
        }
    }

    /// The receiving limit and its task at the current hand-over position.
    fn handover_destination(&self) -> (Payer, u64) {
        let child = self.payer.get();
        let parent = child.forest().record(child).live.parent.unwrap().root();
        let owner = parent.forest().record(parent).live.owner;
        (parent, owner)
    }

    /// Close an unused grant or a cause cell's private reservation.
    pub fn end(&self) {
        let _commit = crate::sequencer::enter();
        let (_, owner) = self.handover_destination();
        let _turn = (owner != crate::tasks::current_id()).then(crate::tasks::before_commit);
        let (_, owner) = self.handover_destination();
        if owner == crate::tasks::current_id() {
            self.merge_with_parent();
        } else {
            self.drain_notified();
            self.transfer();
        }
    }

    /// The old owner folds before publishing the hand-over. The link routes
    /// later refunds immediately. Only the recipient completes the ranked union
    /// and applies the unused reservation, at its next event.
    pub(crate) fn handover(&mut self, creator: u64) {
        let _commit = crate::sequencer::enter();
        let active = self.owner();
        let (_, owner) = self.handover_destination();
        let _turn = (owner != active || creator != active).then(crate::tasks::before_commit);
        self.drain();
        let child = self.payer.get();
        let live = child.forest().record(child).live;
        let (_, owner) = self.handover_destination();
        if owner != live.owner {
            self.transfer();
        } else if creator != live.owner {
            let unused = unsafe { (*child.local()).available };
            crate::tasks::commit_refund(owner, unused, || self.exit());
        } else {
            self.exit();
        }
    }

    fn transfer(&self) {
        let child = self.payer.get();
        let d = child.domain();
        let (parent, owner) = self.handover_destination();
        let unused = unsafe { (*child.local()).available };
        crate::tasks::commit_refund(owner, unused, || {
            let mut remote = d.remote.lock().unwrap();
            let f = child.forest();
            let lexical_parent = f.record(child).live.parent.unwrap();
            debug_assert!(f.record(child).parent.is_none());
            f.retain(child);
            f.record_mut(child).parent = Some(lexical_parent);
            // The child's queued slots now lead to the destination root.
            remote.unite(parent.0, child.0);
            remote.enqueue_handover(child, unused, parent);
        });
    }

    fn merge_with_parent(&self) -> Payer {
        let payer = self.payer.get();
        let f = payer.forest();
        let child = f.find(payer);
        let original_parent = f.record(child).live.parent.unwrap();
        let unused = unsafe { (*child.local()).available };
        let (winner, loser) = f.merge(child, original_parent, unused);
        let mut remote = payer.domain().remote.lock().unwrap();
        remote.unite(winner.0, loser.0);
        winner
    }

    /// Move a retained forwarding handle to its canonical record at a context switch.
    #[inline(always)]
    pub(crate) fn normalize(&self) {
        let _commit = crate::sequencer::enter();
        // SAFETY: Funding handles contain untagged slot pointers, unlike block words.
        if unsafe { (*self.payer.get().links()).parent.is_some() } {
            self.normalize_merged();
        }
    }
    #[cold]
    fn normalize_merged(&self) {
        let canonical = self.payer.get().canonical();
        let f = self.payer.get().forest();
        f.retain(canonical);
        f.release(self.payer.get());
        self.payer.set(canonical);
    }
    #[inline(always)]
    pub(crate) fn raw_local(&self) -> *mut LimitRecord {
        self.payer.get().0.as_ptr().cast()
    }
    #[inline(always)]
    fn reserve_scope(&self, bytes: u64) -> Result<Live, Capacity> {
        let _commit = crate::sequencer::enter();
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
        #[cfg(feature = "prof")]
        parent.observe(r.available);
        let source = unsafe { (*parent.links()).live };
        let live = Live {
            owner: source.owner,
            inbox: source.inbox,
            capacity: bytes,
            parent: Some(parent),
        };
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::sample();
        Ok(live)
    }
    #[inline(always)]
    pub(crate) fn enter(&mut self, bytes: u64) -> Result<(), Capacity> {
        let _commit = crate::sequencer::enter();
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
        let domain = self.payer.get().slot().domain;
        VACANT
            .with_borrow(|cached| {
                cached
                    .as_ref()
                    .filter(|cache| cache.domain == domain)
                    .map(|cache| cache.slots.last().copied())
            })
            .unwrap_or_else(|| {
                // SAFETY: the caller retains the sequencer through selection and use.
                unsafe { (*domain.as_ref().forest.get()).free }
            })
    }
    #[inline(always)]
    pub(crate) fn enter_reused(&mut self, bytes: u64, slot: NonNull<Slot>) -> Result<(), Capacity> {
        let _commit = crate::sequencer::enter();
        let live = self.reserve_scope(bytes)?;
        let domain = self.payer.get().slot().domain;
        let cached = VACANT.with_borrow_mut(|cached| {
            cached
                .as_mut()
                .filter(|cache| cache.domain == domain)
                .and_then(|cache| cache.slots.pop())
        });
        if cached.is_none() {
            // SAFETY: the caller retains the sequencer through selection and use.
            unsafe {
                (*domain.as_ref().forest.get()).free = (*slot.as_ref().links.get()).next_free
            };
        }
        self.payer.set(Forest::initialize(slot, live));
        Ok(())
    }
    #[inline(always)]
    pub(crate) fn exit(&mut self) {
        let _commit = crate::sequencer::enter();
        // SAFETY: the active context pins its canonical record exclusively.
        let payer = self.payer.get();
        let links = unsafe { &*payer.links() };
        let available = unsafe { (*payer.local()).available };
        if links.refs != 1 || available != links.live.capacity {
            return self.exit_survivors();
        }
        let parent = links.live.parent.unwrap();
        // SAFETY: lexical parent identities are untagged, retained slot pointers.
        if unsafe { (*parent.links()).parent.is_some() } {
            return self.exit_forwarded_parent();
        }
        let unused = self.recycle_scope(parent);
        // SAFETY: the parent edge's existing pin becomes this context's pin.
        unsafe {
            #[cfg(feature = "prof")]
            self.payer.get().observe((*self.raw_local()).available);
            (*self.raw_local()).available = (*self.raw_local()).available.wrapping_add(unused);
        }
    }

    /// The rank-zero one-survivor union has a fixed result: the lexical parent
    /// remains representative, so it can transfer the context pin without
    /// running the general rank/metadata machinery.
    #[cold]
    fn exit_rank_zero_survivor(&mut self, parent: Payer) -> bool {
        let child = self.payer.get();
        let f = child.forest();
        let child_record = f.record(child);
        let parent_record = f.record(parent);
        let child_parent = child_record.parent;
        let child_rank = child_record.rank;
        let child_refs = child_record.refs;
        let child_outstanding = unsafe { (*child.local()).outstanding };
        let child_owner = child_record.live.owner;
        let parent_parent = parent_record.parent;
        let parent_owner = parent_record.live.owner;
        let parent_rank = parent_record.rank;
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
        let unused = unsafe { (*child.local()).available };
        unsafe {
            #[cfg(feature = "prof")]
            parent.observe((*parent.local()).available);
            (*parent.local()).available = (*parent.local()).available.wrapping_add(unused);
        }
        if parent_rank == 0 {
            f.record_mut(parent).rank += 1;
        }
        f.record_mut(child).parent = Some(parent);
        f.record_mut(child).refs = 0;
        f.record_mut(parent).refs += 1;
        child
            .domain()
            .remote
            .lock()
            .unwrap()
            .unite(parent.0, child.0);
        self.payer.set(parent);
        true
    }
    #[inline(always)]
    fn recycle_scope(&mut self, parent: Payer) -> u64 {
        let old = self.payer.get();
        // SAFETY: the sole context pin and full room prove this record has no survivors.
        unsafe {
            let r = &mut *old.links();
            let unused = (*old.local()).available;
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
            let cached = VACANT.with_borrow_mut(|cached| {
                let domain = old.slot().domain;
                cached
                    .as_mut()
                    .filter(|cache| cache.domain == domain)
                    .map(|cache| {
                        if cache.slots.len() == SLOT_CACHE {
                            cache.return_batch(f, SLOT_BATCH);
                        }
                        cache.slots.push(old.0);
                        cache.stats.peak = cache.stats.peak.max(cache.slots.len());
                    })
                    .is_some()
            });
            if !cached {
                f.return_vacant(old.0);
            }
            self.payer.set(parent);
            unused
        }
    }
    #[cold]
    fn exit_forwarded_parent(&mut self) {
        // SAFETY: exit selected a live lexical scope whose retained parent merged.
        let parent = unsafe { (*self.payer.get().links()).live.parent.unwrap_unchecked() };
        let unused = self.recycle_scope(parent);
        self.normalize_merged();
        // SAFETY: normalization transferred the context's sole pin to the winner.
        unsafe {
            #[cfg(feature = "prof")]
            self.payer.get().observe((*self.raw_local()).available);
            (*self.raw_local()).available = (*self.raw_local()).available.wrapping_add(unused);
        }
    }
    #[cold]
    fn exit_survivors(&mut self) {
        // SAFETY: exit owns the canonical scope and its retained lexical parent.
        let parent = unsafe { (*self.payer.get().links()).live.parent.unwrap() };
        if self.exit_rank_zero_survivor(parent) {
            return;
        }
        self.exit_general();
    }
    fn exit_general(&mut self) {
        let old = self.payer.get();
        let winner = self.merge_with_parent();
        let f = old.forest();
        f.retain(winner);
        f.release(old);
        self.payer.set(winner);
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn inject(&self, n: usize, later: bool) {
        let _commit = crate::sequencer::enter();
        let f = self.payer.get().forest();
        f.attempts = 0;
        f.fail = Some((n, later));
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn attempts(&self) -> usize {
        let _commit = crate::sequencer::enter();
        self.payer.get().forest().attempts
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn records(&self) -> usize {
        let _commit = crate::sequencer::enter();
        let f = self.payer.get().forest();
        f.slots()
            .filter(|slot| unsafe { (*slot.links.get()).occupied })
            .count()
    }

    #[cfg(test)]
    pub(crate) fn scope_debug(&self) -> (usize, usize, usize, usize, u64) {
        let _commit = crate::sequencer::enter();
        let f = self.payer.get().forest();
        let mut occupied = 0;
        let mut forwarding = 0;
        let mut charged_slots = 0;
        let mut charged_bytes = 0u64;
        for slot in f.slots() {
            let payer = Payer(NonNull::from(slot));
            let record = f.record(payer);
            if !record.occupied {
                continue;
            }
            occupied += 1;
            forwarding += usize::from(record.parent.is_some());
            let outstanding = unsafe { (*payer.local()).outstanding };
            if outstanding != 0 {
                charged_slots += 1;
                charged_bytes += outstanding;
            }
        }
        (f.length, occupied, forwarding, charged_slots, charged_bytes)
    }
}
impl Clone for Funding {
    fn clone(&self) -> Self {
        let _commit = crate::sequencer::enter();
        let d = self.payer.get().domain();
        d.handles.set(d.handles.get() + 1);
        self.payer.get().forest().retain(self.payer.get());
        Self {
            payer: Cell::new(self.payer.get()),
        }
    }
}
impl Drop for Funding {
    fn drop(&mut self) {
        let _commit = crate::sequencer::enter();
        let domain = self.payer.get().slot().domain;
        let d = self.payer.get().domain();
        d.handles.set(d.handles.get() - 1);
        if d.handles.get() != 0 {
            self.payer.get().forest().release(self.payer.get());
            return;
        }
        if BOUND.get().is_some_and(|binding| binding.domain == domain) {
            Self::unbind();
        }
        let mut remote = d.remote.lock().unwrap();
        remote.retired = true;
        let f = self.payer.get().forest();
        f.retired_bytes = f.outstanding();
        self.drain_locked(&mut remote, true);
        self.payer.get().forest().release(self.payer.get());
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

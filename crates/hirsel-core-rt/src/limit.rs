//! Task-local paged limit table. Bytes retain payer slots; only scope handles
//! and union edges use structural pins. Remote events never touch local fields.
//! Native balance updates wrap because admission/refund invariants already prove
//! their range; they do not add guards against incorrect generated bookkeeping.
//! User-visible size arithmetic, Capacity and count-increment overflow stay checked.
use crate::Capacity;
use std::{
    cell::{Cell, UnsafeCell},
    ptr::NonNull,
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy)]
struct Live {
    owner: u64,
    capacity: u64,
    parent: Option<usize>,
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
    live: Option<Live>,
    next_free: Option<usize>,
    occupied: bool,
}
#[repr(C, align(32))]
struct Slot {
    local: UnsafeCell<LimitRecord>,
    pending: AtomicU64,
    pending_zeros: AtomicUsize,
    domain: NonNull<Domain>,
    id: usize,
}
struct Forest {
    pages: Vec<Box<[Slot; 64]>>,
    length: usize,
    free: Option<usize>,
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
            let free = self.free;
            let r = self.record_mut(id);
            r.refs -= 1;
            if r.refs != 0 || r.outstanding != 0 {
                return;
            }
            let parent = r.parent.or_else(|| r.live.and_then(|l| l.parent));
            r.occupied = false;
            r.next_free = free;
            self.free = Some(id);
            if let Some(p) = parent {
                id = p;
            } else {
                return;
            }
        }
    }
    fn insert(&mut self, domain: NonNull<Domain>, live: Live) -> usize {
        let id = if let Some(id) = self.free {
            self.free = self.record(id).next_free;
            id
        } else {
            let id = self.length;
            if id.is_multiple_of(64) {
                self.pages.push(Box::new(std::array::from_fn(|i| Slot {
                    local: UnsafeCell::new(LimitRecord {
                        available: 0,
                        outstanding: 0,
                        refs: 0,
                        parent: None,
                        rank: 0,
                        live: None,
                        next_free: None,
                        occupied: false,
                    }),
                    pending: AtomicU64::new(0),
                    pending_zeros: AtomicUsize::new(0),
                    domain,
                    id: id + i,
                })));
            }
            self.length += 1;
            id
        };
        *self.record_mut(id) = LimitRecord {
            available: live.capacity,
            outstanding: 0,
            refs: 1,
            parent: None,
            rank: 0,
            live: Some(live),
            next_free: None,
            occupied: true,
        };
        id
    }
    #[inline(always)]
    fn find(&mut self, id: usize) -> usize {
        if self.record(id).parent.is_none() {
            id
        } else {
            self.compress(id)
        }
    }
    #[cold]
    fn compress(&mut self, id: usize) -> usize {
        let mut root = id;
        while let Some(p) = self.record(root).parent {
            root = p;
        }
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
    fn refund(&mut self, id: usize, bytes: u64) {
        if bytes == 0 {
            self.zeros -= 1;
            self.release(id);
            return;
        }
        let root = self.find(id);
        self.record_mut(root).available = self.record(root).available.wrapping_add(bytes);
        let r = self.record_mut(id);
        r.outstanding = r.outstanding.wrapping_sub(bytes);
        if r.refs == 0 && r.outstanding == 0 {
            r.refs = 1;
            self.release(id);
        }
    }
}
struct Remote {
    retired: bool,
}
struct Domain {
    forest: UnsafeCell<Forest>,
    handles: Cell<usize>,
    owner: Option<usize>,
    remote: Mutex<Remote>,
}
thread_local! { static THREAD:u8=const{0}; static CURRENT_DOMAIN: Cell<usize> = const { Cell::new(0) }; }
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
        unsafe { &*self.0.as_ptr().map_addr(|v| v & !31) }
    }
    pub(crate) fn native_tag(self) -> usize {
        self.0.as_ptr().addr() & 31
    }
    pub(crate) fn with_tag(self, tag: usize) -> Self {
        // SAFETY: tagging preserves slot provenance; slot() strips these five bits.
        Self(unsafe { NonNull::new_unchecked(self.0.as_ptr().map_addr(|v| (v & !31) | tag)) })
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
            self
        } else {
            let f = self.forest();
            let id = f.find(self.slot().id);
            Self(NonNull::from(f.slot(id)))
        }
    }
    #[inline(always)]
    pub(crate) fn admit(self, bytes: Option<u64>) -> Result<(), Capacity> {
        let payer = self;
        #[cfg(any(test, feature = "testing"))]
        let injected = payer.injected();
        #[cfg(not(any(test, feature = "testing")))]
        let injected = false;
        // SAFETY: canonical live local record, accessed exclusively by its task.
        let r = unsafe { &mut *payer.local() };
        match bytes {
            Some(n) if !injected && n <= r.available => {
                r.available = r.available.wrapping_sub(n);
                r.outstanding = r.outstanding.wrapping_add(n);
                Ok(())
            }
            n => Err(Capacity {
                requested: n.unwrap_or(u64::MAX),
                available: r.available,
            }),
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
    #[inline]
    pub(crate) fn settle(self, bytes: u64) {
        let d = self.domain();
        if d.owner == Some(thread()) && d.handles.get() != 0 {
            let root = self.canonical();
            let owner = root.forest().record(root.slot().id).live.unwrap().owner;
            if owner != crate::tasks::current_id() && bytes != 0 {
                crate::abi_tasks::cross_task_refund(owner, bytes);
                self.slot().pending.fetch_add(bytes, Ordering::Relaxed);
                return;
            }
        }
        if (d.owner == Some(thread())
            || CURRENT_DOMAIN.with(|v| v.get() == self.slot().domain.as_ptr().addr()))
            && d.handles.get() != 0
        {
            // SAFETY: this task owns all local fields; remote events use only mailbox atoms.
            let r = unsafe { &mut *self.local() };
            if bytes != 0 && r.parent.is_none() && r.refs != 0 {
                r.available = r.available.wrapping_add(bytes);
                r.outstanding = r.outstanding.wrapping_sub(bytes);
            } else {
                self.forest().refund(self.slot().id, bytes);
            }
        } else {
            self.remote_refund(bytes);
        }
    }
    #[cold]
    fn remote_refund(self, bytes: u64) {
        let d = self.domain();
        let remote = d.remote.lock().unwrap();
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
        } else if bytes != 0 {
            self.slot().pending.fetch_add(bytes, Ordering::Relaxed);
        } else {
            self.slot().pending_zeros.fetch_add(1, Ordering::Relaxed);
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
                owner: f.record(parent.slot().id).live.unwrap().owner,
                capacity: bytes,
                parent: Some(parent.slot().id),
            },
        );
        self.domain().handles.set(self.domain().handles.get() + 1);
        Funding {
            payer: Self(NonNull::from(f.slot(id))),
        }
    }
}
/// Safe task-local scope handle. Allocations retain bytes, never these handles.
#[repr(C)]
pub struct Funding {
    pub(crate) payer: Payer,
}
impl Funding {
    pub fn root(capacity: u64) -> Self {
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
            remote: Mutex::new(Remote { retired: false }),
        });
        let domain = NonNull::from(Box::leak(domain));
        // SAFETY: the fresh domain is exclusively owned until its first handle returns.
        let f = unsafe { &mut *domain.as_ref().forest.get() };
        let id = f.insert(
            domain,
            Live {
                owner: crate::tasks::current_id(),
                capacity,
                parent: None,
            },
        );
        Self {
            payer: Payer(NonNull::from(f.slot(id))),
        }
    }
    pub(crate) fn native(&self) {
        // SAFETY: context_new calls this before publishing the fresh native context.
        unsafe {
            (*self.payer.slot().domain.as_ptr()).owner = None;
        }
        self.bind();
    }
    pub(crate) fn bind(&self) {
        CURRENT_DOMAIN.with(|v| v.set(self.payer.slot().domain.as_ptr().addr()));
    }
    pub(crate) fn unbind() {
        CURRENT_DOMAIN.with(|v| v.set(0));
    }
    /// Provider drains at event handoff, before this task resumes. No allocation.
    pub fn drain(&self) {
        let _remote = self.payer.domain().remote.lock().unwrap();
        self.drain_locked(false);
    }
    fn drain_locked(&self, retired: bool) {
        let f = self.payer.forest();
        for id in 0..f.length {
            let bytes = f.slot(id).pending.swap(0, Ordering::Relaxed);
            if bytes != 0 {
                f.refund(id, bytes);
                if retired {
                    f.retired_bytes -= bytes;
                }
            }
            let zeros = f.slot(id).pending_zeros.swap(0, Ordering::Relaxed);
            for _ in 0..zeros {
                f.refund(id, 0);
            }
        }
    }
    pub(crate) fn fast(&self) -> *mut LimitRecord {
        self.payer.canonical().local()
    }
    #[inline(always)]
    pub fn available(&self) -> u64 {
        // SAFETY: this handle pins its canonical local record.
        unsafe { (*self.fast()).available }
    }
    pub(crate) fn admit(&self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.payer.canonical().admit(bytes)
    }
    #[cfg(test)]
    pub(crate) fn refund(&self, bytes: u64) {
        self.payer.refund(bytes);
    }
    pub fn reserve(&self, bytes: u64) -> Result<Self, Capacity> {
        self.admit(Some(bytes))?;
        Ok(self.payer.prepaid(bytes))
    }
    pub(crate) fn task_owner(&self, owner: u64) {
        let payer = self.payer.canonical();
        payer
            .forest()
            .record_mut(payer.slot().id)
            .live
            .as_mut()
            .unwrap()
            .owner = owner;
    }
    pub fn end(&self) {
        self.end_id();
    }
    fn end_id(&self) -> usize {
        let f = self.payer.forest();
        let child = f.find(self.payer.slot().id);
        let live = f.record_mut(child).live.take().unwrap();
        let original_parent = live.parent.unwrap();
        let parent = f.find(original_parent);
        let unused = f.record(child).available;
        crate::abi_tasks::cross_task_refund(f.record(parent).live.unwrap().owner, unused);
        f.record_mut(parent).available += unused;
        let parent_available = f.record(parent).available;
        let parent_live = f.record_mut(parent).live.take();
        let (winner, loser) = if f.record(child).rank > f.record(parent).rank {
            (child, parent)
        } else {
            if f.record(child).rank == f.record(parent).rank {
                f.record_mut(parent).rank += 1;
            }
            (parent, child)
        };
        f.record_mut(winner).live = parent_live;
        f.record_mut(winner).available = parent_available;
        f.retain(winner);
        f.record_mut(loser).parent = Some(winner);
        f.release(original_parent);
        winner
    }
    #[inline]
    pub(crate) fn enter(&mut self, bytes: u64) -> Result<(), Capacity> {
        self.admit(Some(bytes))?;
        let parent = self.payer.canonical();
        let f = self.payer.forest();
        f.record_mut(parent.slot().id).outstanding -= bytes;
        f.retain(parent.slot().id);
        let id = f.insert(
            self.payer.slot().domain,
            Live {
                owner: f.record(parent.slot().id).live.unwrap().owner,
                capacity: bytes,
                parent: Some(parent.slot().id),
            },
        );
        f.release(self.payer.slot().id);
        self.payer = Payer(NonNull::from(f.slot(id)));
        Ok(())
    }
    #[inline]
    pub(crate) fn exit(&mut self) {
        let old = self.payer.slot().id;
        let f = self.payer.forest();
        let r = f.record(old);
        // Empty lexical scopes leave no funding record or union edge behind.
        if r.refs == 1 && r.available == r.live.unwrap().capacity {
            let live = r.live.unwrap();
            let parent = f.find(live.parent.unwrap());
            f.record_mut(parent).available += live.capacity;
            f.retain(parent);
            f.release(old);
            self.payer = Payer(NonNull::from(f.slot(parent)));
        } else {
            let winner = self.end_id();
            let f = self.payer.forest();
            f.retain(winner);
            f.release(old);
            self.payer = Payer(NonNull::from(f.slot(winner)));
        }
    }
    pub(crate) fn has_parent(&self) -> bool {
        let p = self.payer.canonical();
        p.forest()
            .record(p.slot().id)
            .live
            .unwrap()
            .parent
            .is_some()
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn inject(&self, n: usize, later: bool) {
        let f = self.payer.forest();
        f.attempts = 0;
        f.fail = Some((n, later));
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn attempts(&self) -> usize {
        self.payer.forest().attempts
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn records(&self) -> usize {
        let f = self.payer.forest();
        (0..f.length).filter(|&id| f.record(id).occupied).count()
    }
}
impl Clone for Funding {
    fn clone(&self) -> Self {
        let d = self.payer.domain();
        d.handles.set(d.handles.get() + 1);
        self.payer.forest().retain(self.payer.slot().id);
        Self { payer: self.payer }
    }
}
impl Drop for Funding {
    fn drop(&mut self) {
        let domain = self.payer.slot().domain;
        let d = self.payer.domain();
        d.handles.set(d.handles.get() - 1);
        if d.handles.get() != 0 {
            self.payer.forest().release(self.payer.slot().id);
            return;
        }
        CURRENT_DOMAIN.with(|v| {
            if v.get() == domain.as_ptr().addr() {
                v.set(0);
            }
        });
        let mut remote = d.remote.lock().unwrap();
        remote.retired = true;
        let f = self.payer.forest();
        f.retired_bytes = (0..f.length).map(|id| f.record(id).outstanding).sum();
        self.drain_locked(true);
        self.payer.forest().release(self.payer.slot().id);
        let empty = self.payer.forest().retired_bytes == 0 && self.payer.forest().zeros == 0;
        drop(remote);
        if empty {
            // SAFETY: the last handle and all charged values have been settled.
            unsafe {
                drop(Box::from_raw(domain.as_ptr()));
            }
        }
    }
}

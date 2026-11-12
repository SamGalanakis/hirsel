//! A process allowance shared by all simultaneously live immutable snapshots.
//!
//! Evictable charges (optional header facts) are admitted only up to the limit
//! minus a headroom; every other charge, the allocator's named owners
//! included, may use the whole limit. The engine restores the headroom at its
//! eviction points, so the unevictable owners that grow between two of them
//! (inside the allocator, where nothing can be evicted) find room without an
//! eviction. The headroom is the declared bound on what one stretch of work
//! between eviction points may add to the unevictable owners.
use std::panic::Location;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use super::OWNERS;

pub struct ProcessBudget {
    limit: u64,
    headroom: u64,
    ledger: Mutex<Ledger>,
    released: Condvar,
    /// Threads inside `wait_for_release`, counted under `ledger`'s lock, so a
    /// release made under that lock wakes exactly when someone can be waiting
    /// and every other release, nearly all of them, makes no futex call.
    waiters: AtomicUsize,
    owners: [AtomicU64; OWNERS],
    idle_reclaimer: Mutex<Option<Weak<dyn Fn() + Send + Sync>>>,
    /// Holders of evictable charges other than the engine's header facts: each
    /// call drops one charge no reader holds and says whether it dropped one.
    evictors: Mutex<Vec<Weak<dyn Fn() -> bool + Send + Sync>>>,
    /// The host budget also records which code sites hold its peak.
    sites: bool,
}

#[derive(Default)]
struct Ledger {
    reserved: u64,
    /// The part of `reserved` held by evictable charges.
    evictable: u64,
    /// Whether an eviction point has passed; growth before the first one is
    /// setup, not a stretch.
    marked: bool,
    /// Unevictable bytes and named-owner bytes at the last eviction point.
    mark: u64,
    mark_owners: [u64; OWNERS],
    /// The high water of unevictable growth past an eviction point's mark.
    stretch: u64,
    /// The high water of the named owners' growth past that mark: the
    /// growth inside the allocator, where nothing can be evicted.
    owner_stretch: u64,
    /// The largest single charge admitted, and the largest a named owner made.
    largest: u64,
    owner_largest: u64,
    peak: u64,
    peak_evictable: u64,
    peak_owners: [u64; OWNERS],
    /// The reserved total at the last site snapshot.
    snapped: u64,
}

/// What the budget reached, for `--timings`.
pub struct BudgetReport {
    pub limit: u64,
    pub headroom: u64,
    pub peak: u64,
    pub peak_evictable: u64,
    pub peak_owners: [u64; OWNERS],
    /// The high water of unevictable growth between two eviction points.
    pub stretch: u64,
    /// The same for the named owners alone.
    pub owner_stretch: u64,
    /// The largest single charge admitted.
    pub largest: u64,
    /// The largest single allocation a named owner made.
    pub owner_largest: u64,
}

/// A named owner's refusal, read by the allocator without allocating.
pub struct OwnerRefusal {
    pub reserved: u64,
    pub limit: u64,
    pub headroom: u64,
    pub evictable: u64,
    /// The refused owner's growth since the last eviction point.
    pub growth: u64,
}

impl ProcessBudget {
    /// A budget with no headroom: evictable charges may fill it.
    pub fn new(limit: u64) -> Arc<Self> {
        Self::build(limit, 0, false)
    }
    /// A budget whose evictable charges leave `headroom` bytes for the
    /// unevictable ones, recording its peak by code site.
    pub fn with_headroom(limit: u64, headroom: u64) -> Arc<Self> {
        Self::build(limit, headroom.min(limit), true)
    }
    fn build(limit: u64, headroom: u64, sites: bool) -> Arc<Self> {
        Arc::new(Self {
            limit,
            headroom,
            ledger: Mutex::new(Ledger::default()),
            released: Condvar::new(),
            waiters: AtomicUsize::new(0),
            owners: std::array::from_fn(|_| AtomicU64::new(0)),
            idle_reclaimer: Mutex::new(None),
            evictors: Mutex::default(),
            sites,
        })
    }
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn owner_bytes(&self) -> [u64; OWNERS] {
        self.owners
            .each_ref()
            .map(|value| value.load(Ordering::Relaxed))
    }
    pub(super) fn retain_owner(&self, owner: super::OwnerKind, bytes: u64) -> bool {
        let mut ledger = self.ledger();
        if bytes > self.limit.saturating_sub(ledger.reserved) {
            return false;
        }
        ledger.reserved += bytes;
        ledger.largest = ledger.largest.max(bytes);
        ledger.owner_largest = ledger.owner_largest.max(bytes);
        self.owners[owner as usize].fetch_add(bytes, Ordering::Relaxed);
        self.grew(&mut ledger);
        true
    }
    pub(super) fn release_owner(&self, owner: super::OwnerKind, bytes: u64) {
        let mut ledger = self.ledger();
        ledger.reserved = ledger
            .reserved
            .checked_sub(bytes)
            .expect("admitted owner releases its own bytes");
        let previous = self.owners[owner as usize].fetch_sub(bytes, Ordering::Relaxed);
        assert!(previous >= bytes, "named owner charge underflow");
        self.wake(&ledger);
    }
    /// What a named owner's refusal reports.
    pub(crate) fn owner_refusal(&self, owner: super::OwnerKind) -> OwnerRefusal {
        let ledger = self.ledger();
        OwnerRefusal {
            reserved: ledger.reserved,
            limit: self.limit,
            headroom: self.headroom,
            evictable: ledger.evictable,
            growth: self.owners[owner as usize]
                .load(Ordering::Relaxed)
                .saturating_sub(ledger.mark_owners[owner as usize]),
        }
    }
    /// Record a growth of the reserved total, under the ledger lock.
    fn grew(&self, ledger: &mut Ledger) {
        if ledger.marked {
            let unevictable = ledger.reserved - ledger.evictable;
            ledger.stretch = ledger.stretch.max(unevictable.saturating_sub(ledger.mark));
            let owners: u64 = self.owner_bytes().iter().sum();
            let marked: u64 = ledger.mark_owners.iter().sum();
            ledger.owner_stretch = ledger.owner_stretch.max(owners.saturating_sub(marked));
        }
        if ledger.reserved <= ledger.peak {
            return;
        }
        ledger.peak = ledger.reserved;
        ledger.peak_evictable = ledger.evictable;
        ledger.peak_owners = self.owner_bytes();
        if self.sites && ledger.reserved >= ledger.snapped.saturating_add(sites::STEP) {
            ledger.snapped = ledger.reserved;
            sites::snapshot();
        }
    }
    /// Wake the waiters after a release made while holding the ledger.
    fn wake(&self, _held: &MutexGuard<'_, Ledger>) {
        if self.waiters.load(Ordering::Relaxed) != 0 {
            self.released.notify_all();
        }
    }
    /// Register the process's idle retention authority without extending its lifetime.
    /// Reclamation runs outside the budget lock and must preserve live owners.
    pub fn set_idle_reclaimer(&self, reclaimer: Weak<dyn Fn() + Send + Sync>) {
        *self
            .idle_reclaimer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(reclaimer);
    }
    fn reclaim_idle(&self) -> bool {
        let reclaimer = self
            .idle_reclaimer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade);
        let Some(reclaimer) = reclaimer else {
            return false;
        };
        reclaimer();
        true
    }
    /// Register a holder of evictable charges for every eviction point and
    /// every refused reservation, without extending its lifetime. It runs
    /// outside the budget lock and must not reserve from this budget.
    pub fn register_evictor(&self, evictor: Weak<dyn Fn() -> bool + Send + Sync>) {
        let mut evictors = self.evictors.lock().unwrap_or_else(|e| e.into_inner());
        evictors.retain(|evictor| evictor.strong_count() != 0);
        evictors.push(evictor);
    }
    /// Evict one registered evictable charge; false once none evicts.
    pub fn evict_one(&self) -> bool {
        let live = {
            let mut evictors = self.evictors.lock().unwrap_or_else(|e| e.into_inner());
            evictors.retain(|evictor| evictor.strong_count() != 0);
            evictors
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        live.iter().any(|evict| evict())
    }
    /// Nonblocking: reject a new source snapshot instead of multiplying caps
    /// or waiting while holding another snapshot's allowance.
    #[track_caller]
    pub fn reserve(self: &Arc<Self>, bytes: u64) -> std::io::Result<MemoryReservation> {
        let mut error = match self.reserve_once(bytes) {
            Ok(reservation) => return Ok(reservation),
            Err(error) => error,
        };
        while bytes <= self.limit && self.evict_one() {
            match self.reserve_once(bytes) {
                Ok(reservation) => return Ok(reservation),
                Err(refused) => error = refused,
            }
        }
        if bytes > self.limit || !self.reclaim_idle() {
            return Err(error);
        }
        self.reserve_once(bytes)
    }
    /// Reserve without reclaiming retained owners. A host that owns a multipart
    /// admission releases idle state only after its failed attempt has rolled back.
    #[track_caller]
    pub fn reserve_once(self: &Arc<Self>, bytes: u64) -> std::io::Result<MemoryReservation> {
        self.admit(bytes, false, false, Location::caller())
    }
    /// Reserve an evictable charge: one its holder releases when an eviction
    /// point asks. It is admitted only while the headroom stays free.
    #[track_caller]
    pub fn reserve_evictable(self: &Arc<Self>, bytes: u64) -> std::io::Result<MemoryReservation> {
        let site = Location::caller();
        let mut error = match self.admit(bytes, true, false, site) {
            Ok(reservation) => return Ok(reservation),
            Err(error) => error,
        };
        while bytes <= self.limit - self.headroom && self.evict_one() {
            match self.admit(bytes, true, false, site) {
                Ok(reservation) => return Ok(reservation),
                Err(refused) => error = refused,
            }
        }
        if bytes > self.limit - self.headroom || !self.reclaim_idle() {
            return Err(error);
        }
        self.admit(bytes, true, false, site)
    }
    /// Reserve an evictable charge its holder requires after evicting what
    /// it could: it may take the headroom, as an unevictable charge would.
    #[track_caller]
    pub fn reserve_evictable_in_headroom(
        self: &Arc<Self>,
        bytes: u64,
    ) -> std::io::Result<MemoryReservation> {
        self.admit(bytes, true, true, Location::caller())
    }
    fn admit(
        self: &Arc<Self>,
        bytes: u64,
        evictable: bool,
        headroom: bool,
        site: &'static Location<'static>,
    ) -> std::io::Result<MemoryReservation> {
        let ceiling = if evictable && !headroom {
            self.limit - self.headroom
        } else {
            self.limit
        };
        let mut ledger = self.ledger();
        if bytes > ceiling.saturating_sub(ledger.reserved) {
            let used = ledger.reserved;
            // Formatting can allocate/free. Keep the allocator's ownership
            // table out of this budget lock's lifetime.
            drop(ledger);
            return Err(std::io::Error::other(format!(
                "process query allowance exhausted: {bytes} bytes requested with {used} of {ceiling} reserved"
            )));
        }
        ledger.reserved += bytes;
        ledger.largest = ledger.largest.max(bytes);
        if evictable {
            ledger.evictable += bytes;
        }
        if self.sites {
            sites::add(site, bytes);
        }
        self.grew(&mut ledger);
        Ok(MemoryReservation {
            budget: self.clone(),
            bytes,
            evictable,
            site,
        })
    }
    /// A charge of no bytes, grown later or held only for its budget.
    #[track_caller]
    pub fn empty(self: &Arc<Self>) -> MemoryReservation {
        MemoryReservation {
            budget: self.clone(),
            bytes: 0,
            evictable: false,
            site: Location::caller(),
        }
    }
    pub fn reserved(&self) -> u64 {
        self.ledger().reserved
    }
    pub fn limit(&self) -> u64 {
        self.limit
    }
    pub fn headroom(&self) -> u64 {
        self.headroom
    }
    /// Bytes not yet reserved by any owner.
    pub fn free(&self) -> u64 {
        self.limit.saturating_sub(self.reserved())
    }
    /// Bytes an evictable charge may still take.
    pub fn evictable_free(&self) -> u64 {
        (self.limit - self.headroom).saturating_sub(self.reserved())
    }
    /// Whether an eviction point should evict: evictable charges are held
    /// and the headroom would not stay free after a further `charge`.
    pub fn short_of_headroom(&self, charge: u64) -> bool {
        let ledger = self.ledger();
        ledger.evictable != 0 && self.limit - ledger.reserved < self.headroom.saturating_add(charge)
    }
    /// An eviction point has evicted what it could; growth is measured from here.
    pub fn eviction_point(&self) {
        let mut ledger = self.ledger();
        ledger.marked = true;
        ledger.mark = ledger.reserved - ledger.evictable;
        ledger.mark_owners = self.owner_bytes();
    }
    pub fn report(&self) -> BudgetReport {
        let ledger = self.ledger();
        BudgetReport {
            limit: self.limit,
            headroom: self.headroom,
            peak: ledger.peak,
            peak_evictable: ledger.peak_evictable,
            peak_owners: ledger.peak_owners,
            stretch: ledger.stretch,
            owner_stretch: ledger.owner_stretch,
            largest: ledger.largest,
            owner_largest: ledger.owner_largest,
        }
    }
    /// Block until an owner releases part of this allowance, a host reports
    /// idle work, or the timeout passes. Only a caller holding none of this
    /// budget may wait; one holding a charge could wait on itself.
    pub fn wait_for_release(&self, timeout: Duration) {
        let ledger = self.ledger();
        self.waiters.fetch_add(1, Ordering::Relaxed);
        let (ledger, _) = self
            .released
            .wait_timeout(ledger, timeout)
            .unwrap_or_else(|e| e.into_inner());
        self.waiters.fetch_sub(1, Ordering::Relaxed);
        drop(ledger);
    }
    /// Wake waiters after work became idle without releasing its charge yet.
    pub fn notify_idle(&self) {
        self.released.notify_all();
    }
}

/// Held through worker shutdown or the last externally held input/map owner.
/// This type is not Clone: a retained shared mapping shares an Arc around its
/// one reservation, rather than duplicating or silently dropping its charge.
pub struct MemoryReservation {
    budget: Arc<ProcessBudget>,
    bytes: u64,
    evictable: bool,
    site: &'static Location<'static>,
}

impl MemoryReservation {
    /// Transfer part of this already charged allowance to a distinct owner of
    /// the same class. The process total never changes, including on failure;
    /// each resulting owner releases exactly its part on Drop.
    #[track_caller]
    pub fn split(&mut self, bytes: u64) -> Option<Self> {
        self.bytes = self.bytes.checked_sub(bytes)?;
        let site = Location::caller();
        if self.budget.sites {
            sites::moved(self.site, site, bytes);
        }
        Some(Self {
            budget: self.budget.clone(),
            bytes,
            evictable: self.evictable,
            site,
        })
    }
    /// Take over a charge on the same process budget, in this charge's class.
    /// A charge from another budget is handed back unchanged.
    pub fn join(&mut self, mut other: Self) -> Result<(), Self> {
        if !Arc::ptr_eq(&self.budget, &other.budget) {
            return Err(other);
        }
        let bytes = std::mem::take(&mut other.bytes);
        if other.evictable != self.evictable && bytes != 0 {
            let mut ledger = self.budget.ledger();
            if self.evictable {
                ledger.evictable += bytes;
            } else {
                ledger.evictable -= bytes;
                self.budget.grew(&mut ledger);
            }
        }
        if self.budget.sites {
            sites::moved(other.site, self.site, bytes);
        }
        self.bytes += bytes;
        Ok(())
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// Extend this charge by `bytes` more of its own budget, in its class.
    #[track_caller]
    pub fn grow(&mut self, bytes: u64) -> std::io::Result<()> {
        let extra = if self.evictable {
            self.budget.reserve_evictable(bytes)?
        } else {
            self.budget.reserve(bytes)?
        };
        self.join(extra).ok().expect("grown from its own budget");
        Ok(())
    }
    /// The request footprint this charge was reserved from.
    pub fn budget(&self) -> &Arc<ProcessBudget> {
        &self.budget
    }
}
impl Drop for MemoryReservation {
    fn drop(&mut self) {
        let bytes = std::mem::take(&mut self.bytes);
        if bytes == 0 {
            return;
        }
        let mut ledger = self.budget.ledger();
        ledger.reserved -= bytes;
        if self.evictable {
            ledger.evictable -= bytes;
        }
        if self.budget.sites {
            sites::sub(self.site, bytes);
        }
        self.budget.wake(&ledger);
    }
}

/// The host budget's live bytes by reserving code site, its own peak per
/// site, and every site's bytes when the total last rose by `STEP` toward its
/// peak. Updated under the budget's ledger lock.
pub mod sites {
    use std::panic::Location;
    use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering::Relaxed};
    pub(super) const STEP: u64 = 1 << 20;
    const SLOTS: usize = 1024;
    static KEYS: [AtomicPtr<Location<'static>>; SLOTS] =
        [const { AtomicPtr::new(std::ptr::null_mut()) }; SLOTS];
    static LIVE: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
    static OWN_PEAK: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
    static AT_PEAK: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
    /// The slot of `site`; `None` once every slot holds another site.
    fn slot(site: &'static Location<'static>) -> Option<usize> {
        let key = site as *const Location<'static> as *mut Location<'static>;
        let start = (key as usize >> 3) % SLOTS;
        (0..SLOTS).map(|n| (start + n) % SLOTS).find(|&at| {
            match KEYS[at].compare_exchange(std::ptr::null_mut(), key, Relaxed, Relaxed) {
                Ok(_) => true,
                Err(held) => held == key,
            }
        })
    }
    pub(super) fn add(site: &'static Location<'static>, bytes: u64) {
        if let Some(at) = slot(site).filter(|_| bytes != 0) {
            let live = LIVE[at].fetch_add(bytes, Relaxed) + bytes;
            OWN_PEAK[at].fetch_max(live, Relaxed);
        }
    }
    pub(super) fn sub(site: &'static Location<'static>, bytes: u64) {
        if let Some(at) = slot(site).filter(|_| bytes != 0) {
            LIVE[at].fetch_sub(bytes, Relaxed);
        }
    }
    pub(super) fn moved(
        from: &'static Location<'static>,
        to: &'static Location<'static>,
        bytes: u64,
    ) {
        if from != to {
            sub(from, bytes);
            add(to, bytes);
        }
    }
    pub(super) fn snapshot() {
        for at in 0..SLOTS {
            AT_PEAK[at].store(LIVE[at].load(Relaxed), Relaxed);
        }
    }
    /// Each site that held at least `floor` bytes: its bytes at the budget's
    /// peak and its own peak. Allocates nothing.
    pub fn each(floor: u64, mut visit: impl FnMut(&'static Location<'static>, u64, u64)) {
        for at in 0..SLOTS {
            let key = KEYS[at].load(Relaxed);
            let own = OWN_PEAK[at].load(Relaxed);
            if !key.is_null() && own >= floor {
                // SAFETY: keys are only ever `&'static Location` pointers.
                visit(unsafe { &*key }, AT_PEAK[at].load(Relaxed), own);
            }
        }
    }
}

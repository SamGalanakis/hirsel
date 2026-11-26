//! A process allowance shared by all simultaneously live immutable snapshots.
//! Every charge, the allocator's named owners included, may use the whole limit.
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::OWNERS;

pub struct ProcessBudget {
    limit: u64,
    ledger: Mutex<Ledger>,
    owners: [AtomicU64; OWNERS],
    /// The host budget also records which code sites hold its peak.
    sites: bool,
}

#[derive(Default)]
struct Ledger {
    reserved: u64,
    /// The largest single charge admitted, and the largest a named owner made.
    largest: u64,
    owner_largest: u64,
    peak: u64,
    peak_owners: [u64; OWNERS],
    /// The reserved total at the last site snapshot.
    snapped: u64,
}

/// What the budget reached, for `--timings`.
pub struct BudgetReport {
    pub limit: u64,
    pub peak: u64,
    pub peak_owners: [u64; OWNERS],
    /// The largest single charge admitted.
    pub largest: u64,
    /// The largest single allocation a named owner made.
    pub owner_largest: u64,
}

/// A named owner's refusal, read by the allocator without allocating.
pub struct OwnerRefusal {
    pub reserved: u64,
    pub limit: u64,
}

impl ProcessBudget {
    /// A budget whose reservations do not report their code sites.
    pub fn new(limit: u64) -> Arc<Self> {
        Self::build(limit, false)
    }
    /// The host budget, recording its peak by code site.
    pub(crate) fn host(limit: u64) -> Arc<Self> {
        Self::build(limit, true)
    }
    fn build(limit: u64, sites: bool) -> Arc<Self> {
        Arc::new(Self {
            limit,
            ledger: Mutex::new(Ledger::default()),
            owners: std::array::from_fn(|_| AtomicU64::new(0)),
            sites,
        })
    }
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
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
    }
    /// What a named owner's refusal reports.
    pub(crate) fn owner_refusal(&self) -> OwnerRefusal {
        let ledger = self.ledger();
        OwnerRefusal {
            reserved: ledger.reserved,
            limit: self.limit,
        }
    }
    /// Record a growth of the reserved total, under the ledger lock.
    fn grew(&self, ledger: &mut Ledger) {
        if ledger.reserved <= ledger.peak {
            return;
        }
        ledger.peak = ledger.reserved;
        ledger.peak_owners = self
            .owners
            .each_ref()
            .map(|value| value.load(Ordering::Relaxed));
        if self.sites && ledger.reserved >= ledger.snapped.saturating_add(sites::STEP) {
            ledger.snapped = ledger.reserved;
            sites::snapshot();
        }
    }
    /// Nonblocking: reject a new source snapshot instead of multiplying caps
    /// or waiting while holding another snapshot's allowance.
    #[track_caller]
    pub fn reserve(self: &Arc<Self>, bytes: u64) -> std::io::Result<MemoryReservation> {
        let site = Location::caller();
        let mut ledger = self.ledger();
        if bytes > self.limit.saturating_sub(ledger.reserved) {
            let used = ledger.reserved;
            // Formatting can allocate/free. Keep the allocator's ownership
            // table out of this budget lock's lifetime.
            drop(ledger);
            return Err(std::io::Error::other(format!(
                "process query allowance exhausted: {bytes} bytes requested with {used} of {} reserved",
                self.limit
            )));
        }
        ledger.reserved += bytes;
        ledger.largest = ledger.largest.max(bytes);
        if self.sites {
            sites::add(site, bytes);
        }
        self.grew(&mut ledger);
        Ok(MemoryReservation {
            budget: self.clone(),
            bytes,
            site,
        })
    }
    /// A charge of no bytes, grown later or held only for its budget.
    #[track_caller]
    pub fn empty(self: &Arc<Self>) -> MemoryReservation {
        MemoryReservation {
            budget: self.clone(),
            bytes: 0,
            site: Location::caller(),
        }
    }
    pub fn reserved(&self) -> u64 {
        self.ledger().reserved
    }
    pub fn limit(&self) -> u64 {
        self.limit
    }
    /// Bytes not yet reserved by any owner.
    pub fn free(&self) -> u64 {
        self.limit.saturating_sub(self.reserved())
    }
    pub fn report(&self) -> BudgetReport {
        let ledger = self.ledger();
        BudgetReport {
            limit: self.limit,
            peak: ledger.peak,
            peak_owners: ledger.peak_owners,
            largest: ledger.largest,
            owner_largest: ledger.owner_largest,
        }
    }
}

/// Held through worker shutdown or the last externally held input/map owner.
/// This type is not Clone: a retained shared mapping shares an Arc around its
/// one reservation, rather than duplicating or silently dropping its charge.
pub struct MemoryReservation {
    budget: Arc<ProcessBudget>,
    bytes: u64,
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
            site,
        })
    }
    /// Take over a charge on the same process budget. A charge from another
    /// budget is handed back unchanged.
    pub fn join(&mut self, mut other: Self) -> Result<(), Self> {
        if !Arc::ptr_eq(&self.budget, &other.budget) {
            return Err(other);
        }
        let bytes = std::mem::take(&mut other.bytes);
        if self.budget.sites {
            sites::moved(other.site, self.site, bytes);
        }
        self.bytes += bytes;
        Ok(())
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// Extend this charge by `bytes` more of its own budget.
    #[track_caller]
    pub fn grow(&mut self, bytes: u64) -> std::io::Result<()> {
        let extra = self.budget.reserve(bytes)?;
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
        if self.budget.sites {
            sites::sub(self.site, bytes);
        }
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

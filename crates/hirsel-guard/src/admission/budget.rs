//! A process allowance shared by all simultaneously live immutable snapshots.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

pub struct ProcessBudget {
    limit: u64,
    reserved: Mutex<u64>,
    released: Condvar,
    /// Threads inside `wait_for_release`, counted under `reserved`'s lock, so a
    /// release made under that lock wakes exactly when someone can be waiting
    /// and every other release, nearly all of them, makes no futex call.
    waiters: AtomicUsize,
    owners: [std::sync::atomic::AtomicU64; super::OWNERS],
    idle_reclaimer: Mutex<Option<Weak<dyn Fn() + Send + Sync>>>,
}
impl ProcessBudget {
    /// The engine subtracts its fixed overhead from the host limit first.
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            reserved: Mutex::new(0),
            released: Condvar::new(),
            waiters: AtomicUsize::new(0),
            owners: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)),
            idle_reclaimer: Mutex::new(None),
        })
    }
    pub fn owner_bytes(&self) -> [u64; super::OWNERS] {
        self.owners
            .each_ref()
            .map(|value| value.load(std::sync::atomic::Ordering::Relaxed))
    }
    pub(super) fn retain_owner(&self, owner: super::OwnerKind, bytes: u64) -> bool {
        let mut reserved = self.reserved.lock().unwrap_or_else(|e| e.into_inner());
        if bytes > self.limit.saturating_sub(*reserved) {
            return false;
        }
        *reserved += bytes;
        self.owners[owner as usize].fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        true
    }
    pub(super) fn release_owner(&self, owner: super::OwnerKind, bytes: u64) {
        let mut reserved = self.reserved.lock().unwrap_or_else(|e| e.into_inner());
        *reserved = reserved
            .checked_sub(bytes)
            .expect("admitted owner releases its own bytes");
        let previous =
            self.owners[owner as usize].fetch_sub(bytes, std::sync::atomic::Ordering::Relaxed);
        assert!(previous >= bytes, "named owner charge underflow");
        self.wake(&reserved);
    }
    /// Wake the waiters after a release made while holding `reserved`.
    fn wake(&self, _held: &MutexGuard<'_, u64>) {
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
    /// Nonblocking: reject a new source snapshot instead of multiplying caps
    /// or waiting while holding another snapshot's allowance.
    pub fn reserve(self: &Arc<Self>, bytes: u64) -> std::io::Result<MemoryReservation> {
        let error = match self.reserve_once(bytes) {
            Ok(reservation) => return Ok(reservation),
            Err(error) => error,
        };
        if bytes > self.limit {
            return Err(error);
        }
        let reclaimer = self
            .idle_reclaimer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade);
        let Some(reclaimer) = reclaimer else {
            return Err(error);
        };
        reclaimer();
        drop(reclaimer);
        self.reserve_once(bytes)
    }
    /// Reserve without reclaiming retained owners. A host that owns a multipart
    /// admission releases idle state only after its failed attempt has rolled back.
    pub fn reserve_once(self: &Arc<Self>, bytes: u64) -> std::io::Result<MemoryReservation> {
        let mut reserved = self.reserved.lock().unwrap_or_else(|e| e.into_inner());
        if bytes > self.limit.saturating_sub(*reserved) {
            let used = *reserved;
            // Formatting can allocate/free. Keep the allocator's ownership
            // table out of this budget lock's lifetime.
            drop(reserved);
            return Err(std::io::Error::other(format!(
                "process query allowance exhausted: {bytes} bytes requested with {} of {} reserved",
                used, self.limit
            )));
        }
        *reserved += bytes;
        Ok(MemoryReservation {
            budget: self.clone(),
            bytes,
        })
    }
    /// A charge of no bytes, grown later or held only for its budget.
    pub fn empty(self: &Arc<Self>) -> MemoryReservation {
        MemoryReservation {
            budget: self.clone(),
            bytes: 0,
        }
    }
    pub fn reserved(&self) -> u64 {
        *self.reserved.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn limit(&self) -> u64 {
        self.limit
    }
    /// Bytes not yet reserved by any owner.
    pub fn free(&self) -> u64 {
        self.limit.saturating_sub(self.reserved())
    }
    /// Block until an owner releases part of this allowance, a host reports
    /// idle work, or the timeout passes. Only a caller holding none of this
    /// budget may wait; one holding a charge could wait on itself.
    pub fn wait_for_release(&self, timeout: Duration) {
        let reserved = self.reserved.lock().unwrap_or_else(|e| e.into_inner());
        self.waiters.fetch_add(1, Ordering::Relaxed);
        let (reserved, _) = self
            .released
            .wait_timeout(reserved, timeout)
            .unwrap_or_else(|e| e.into_inner());
        self.waiters.fetch_sub(1, Ordering::Relaxed);
        drop(reserved);
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
}

impl MemoryReservation {
    /// Transfer part of this already charged allowance to a distinct owner.
    /// The process total never changes, including on failure; each resulting
    /// owner releases exactly its part on Drop.
    pub fn split(&mut self, bytes: u64) -> Option<Self> {
        self.bytes = self.bytes.checked_sub(bytes)?;
        Some(Self {
            budget: self.budget.clone(),
            bytes,
        })
    }
    /// Take over a charge on the same process budget.
    /// A charge from another budget is handed back unchanged.
    pub fn join(&mut self, mut other: Self) -> Result<(), Self> {
        if !Arc::ptr_eq(&self.budget, &other.budget) {
            return Err(other);
        }
        self.bytes += std::mem::take(&mut other.bytes);
        Ok(())
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// Extend this charge by `bytes` more of its own budget.
    pub fn grow(&mut self, bytes: u64) -> std::io::Result<()> {
        let mut extra = self.budget.reserve(bytes)?;
        self.bytes += std::mem::take(&mut extra.bytes);
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
        let mut reserved = self
            .budget
            .reserved
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *reserved -= bytes;
        self.budget.wake(&reserved);
    }
}

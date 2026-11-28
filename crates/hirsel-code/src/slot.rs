use super::{Candidate, Code, EntryIndex, Image, Refusal, Shared, Signature, Stop};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Entry target stored in a slot or worker row: optional slab image pin (`None`
/// for process-lifetime statically linked entries) + entry code address.
type Raw = (Option<Arc<Image>>, usize);

struct Cell {
    shared: Arc<Shared>,
    version: AtomicU64,
    current: Mutex<Option<Raw>>,
}

/// The version of a slot whose last handle is gone. No activation reaches it.
const CLOSED: u64 = u64::MAX;

/// What the handles of one slot share. Workers hold the cell and not this, so the
/// last handle to go closes the cell while their rows still name it.
struct Lease(Arc<Cell>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Lease {
    /// An empty version-0 slot of `shared`'s pool.
    fn new(shared: Arc<Shared>) -> Arc<Self> {
        Arc::new(Self(Arc::new(Cell {
            shared,
            version: AtomicU64::new(0),
            current: Mutex::new(None),
        })))
    }
}

impl Cell {
    /// Give up the slot's code and move its version to `CLOSED`, so every worker row
    /// for it is stale and goes at that worker's next park (PACK-6).
    fn close(&self) {
        let old = {
            let mut current = self.current.lock().unwrap();
            self.version.store(CLOSED, Ordering::Release);
            self.shared.updates.fetch_add(1, Ordering::Release);
            current.take()
        };
        let Some((Some(image), _)) = old else { return };
        {
            let mut retired = self.shared.retired.lock().unwrap();
            if image.slots.fetch_sub(1, Ordering::Relaxed) == 1 {
                image.retired.store(true, Ordering::Relaxed);
                *retired += image.bytes;
            }
        }
        drop(image);
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Install `(image, entry)` at `expected` version. Checks pool identity
    /// before taking the slot lock or mutating state (PACK-14).
    fn activate(
        &self,
        expected: u64,
        image: Option<Arc<Image>>,
        entry: usize,
    ) -> Result<u64, Stop> {
        if let Some(ref img) = image
            && !Arc::ptr_eq(&img.shared, &self.shared)
        {
            return Err(Stop::WrongPool);
        }
        let old;
        let next_version;
        {
            let mut current = self.current.lock().unwrap();
            let version = self.version.load(Ordering::Relaxed);
            if version != expected {
                return Err(Stop::Stale(version));
            }
            let Some(next) = version.checked_add(1).filter(|next| *next != CLOSED) else {
                return Err(Stop::Capacity);
            };
            next_version = next;
            let same = match (
                current.as_ref().and_then(|(h, _)| h.as_ref()),
                image.as_ref(),
            ) {
                (Some(held), Some(new)) => Arc::ptr_eq(held, new),
                (None, None) => true,
                _ => false,
            };
            if !same {
                let mut retired = self.shared.retired.lock().unwrap();
                let returns = image
                    .as_ref()
                    .is_some_and(|img| img.retired.load(Ordering::Relaxed));
                let new_bytes = image.as_ref().map_or(0, |img| img.bytes);
                let leaves = current
                    .as_ref()
                    .and_then(|(held, _)| held.as_ref())
                    .filter(|held| held.slots.load(Ordering::Relaxed) == 1);
                let after = *retired - if returns { new_bytes } else { 0 }
                    + leaves.map_or(0, |held| held.bytes);
                if leaves.is_some() && after > self.shared.config.retired_bytes {
                    return Err(Stop::Capacity);
                }
                if let Some(ref new) = image {
                    new.slots.fetch_add(1, Ordering::Relaxed);
                    new.retired.store(false, Ordering::Relaxed);
                }
                if let Some((Some(held), _)) = &*current {
                    held.slots.fetch_sub(1, Ordering::Relaxed);
                }
                if let Some(held) = leaves {
                    held.retired.store(true, Ordering::Relaxed);
                }
                *retired = after;
            }
            old = current.replace((image, entry));
            self.version.store(next_version, Ordering::Release);
            self.shared.updates.fetch_add(1, Ordering::Release);
        }
        // The old code's count is given up outside every lock.
        drop(old);
        Ok(next_version)
    }
}

/// A shared `(version, Code)` that workers dispatch through (PACK-6).
pub struct Slot<F> {
    lease: Arc<Lease>,
    callable: PhantomData<fn() -> F>,
}

impl<F> Clone for Slot<F> {
    fn clone(&self) -> Self {
        Self {
            lease: self.lease.clone(),
            callable: PhantomData,
        }
    }
}

/// A launch's selected native adapters in Runner registration order. A release
/// with a canonical order can activate any number of slots from this one value.
#[derive(Clone)]
pub struct Registered<F> {
    code: Code<F>,
    order: Arc<[u32]>,
}

impl<F> Code<F> {
    /// Bind the installer-selected rows to this realization before activation.
    /// Several distinct routes can use one executable with different bindings.
    pub fn register<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a Candidate>,
    ) -> Result<Registered<F>, Refusal> {
        let order = candidates
            .into_iter()
            .map(|candidate| {
                if !std::ptr::eq(candidate.image.as_ptr(), Arc::as_ptr(&self.image)) {
                    return Err(Refusal::NoEntry);
                }
                u32::try_from(candidate.index).map_err(|_| Refusal::Capacity)
            })
            .collect::<Result<Arc<[_]>, _>>()?;
        Ok(Registered {
            code: self.clone(),
            order,
        })
    }
}

impl<F: Signature> Slot<F> {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            lease: Lease::new(shared),
            callable: PhantomData,
        }
    }

    pub fn version(&self) -> u64 {
        self.lease.0.version()
    }

    /// Publish a complete installation and issue dispatch permission for exactly
    /// the version selected under the activation lock.
    pub fn activate_checkout(
        &self,
        expected: u64,
        registration: &Registered<F>,
    ) -> Result<Checkout, Stop> {
        let version = self.activate(expected, registration.code.clone())?;
        Ok(Checkout {
            lease: Arc::clone(&self.lease),
            version,
            order: Arc::clone(&registration.order),
        })
    }

    /// Store `code` when the slot is at version `expected`, and return the new version.
    /// It waits for no call, worker or unit that still uses the old code.
    pub fn activate(&self, expected: u64, code: Code<F>) -> Result<u64, Stop> {
        self.lease
            .0
            .activate(expected, Some(code.image), code.entry)
    }
}

/// Dispatch permission for one activation. Its holder can neither replace the
/// slot nor use bindings from a previous activation of the same image.
#[derive(Clone)]
pub struct Checkout {
    lease: Arc<Lease>,
    version: u64,
    order: Arc<[u32]>,
}

impl Checkout {
    pub fn version(&self) -> u64 {
        self.version
    }
}

/// Untyped slot handle for the runtime bridge (`CodeSlot[F]`, PACK-14).
#[derive(Clone)]
pub struct RawSlot {
    lease: Arc<Lease>,
}

impl RawSlot {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            lease: Lease::new(shared),
        }
    }

    pub fn version(&self) -> u64 {
        self.lease.0.version()
    }

    pub fn activate(&self, expected: u64, code: &super::RawCode) -> Result<u64, Stop> {
        self.lease
            .0
            .activate(expected, code.image.clone(), code.entry)
    }

    /// Check whether this slot belongs to `loader`'s pool.
    pub fn same_pool(&self, loader: &super::Loader) -> bool {
        Arc::ptr_eq(&self.lease.0.shared, &loader.0)
    }
}

/// Owning pin on a loaded image generation, used when a parked continuation
/// leaves worker custody or a Unit owns its generation through end publication.
#[derive(Clone)]
pub struct CodePin(pub(super) Option<Arc<Image>>);

impl CodePin {
    pub const fn static_pin() -> Self {
        Self(None)
    }
}

/// Token tracking one in-flight worker borrow from `begin_dispatch` to `end_dispatch`.
pub enum BorrowToken {
    /// Borrowed from worker-private row `index` (zero shared-line writes on hit).
    Row(usize),
    /// All worker rows were pinned by nested calls; this call holds its own pin.
    Owned(Option<Arc<Image>>),
    /// Promoted to a continuation's `CodePin` before leaving worker custody.
    Promoted,
}

struct Row {
    cell: Arc<Cell>,
    version: u64,
    code: Raw,
    used: u64,
    /// Worker-private count of active calls borrowing this row. Never written by
    /// another thread. Blocks eviction, in-place refresh, and `park` while > 0.
    borrows: u32,
}

/// One worker's private table of the code it dispatches to (PACK-6, PACK-14).
pub struct Worker {
    shared: Arc<Shared>,
    rows: Vec<Option<Row>>,
    clock: u64,
    seen: u64,
}

impl Worker {
    pub const ROWS: usize = 64;

    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            rows: Vec::with_capacity(Self::ROWS),
            clock: 0,
            seen: 0,
        }
    }

    /// Begin a dispatch through `cell`, borrowing a worker-owned generation on hit
    /// with a single version load and zero shared-line writes or refcount increments.
    /// Returns `None` if the slot is empty.
    fn begin_cell(&mut self, cell: &Arc<Cell>) -> Option<(usize, u64, BorrowToken)> {
        let updates = self.shared.updates.load(Ordering::Relaxed);
        if updates != self.seen {
            self.seen = updates;
            self.park();
        }
        self.clock += 1;
        let version = cell.version.load(Ordering::Acquire);
        if let Some(at) = self.rows.iter().position(|slot| {
            slot.as_ref()
                .is_some_and(|row| Arc::ptr_eq(&row.cell, cell) && row.version == version)
        }) {
            let row = self.rows[at].as_mut().unwrap();
            row.used = self.clock;
            row.borrows = row.borrows.checked_add(1).expect("worker borrow overflow");
            return Some((row.code.1, version, BorrowToken::Row(at)));
        }
        // Miss: acquire entry and version together under the slot lock.
        let (version, code) = {
            let current = cell.current.lock().unwrap();
            (cell.version.load(Ordering::Relaxed), current.clone()?)
        };
        let entry = code.1;
        let new_row = Row {
            cell: cell.clone(),
            version,
            code,
            used: self.clock,
            borrows: 1,
        };
        // Prefer replacing an unborrowed row for the same slot, then an empty slot,
        // then the LRU unborrowed row. Never overwrite or evict a borrowed row.
        if let Some(at) = self.rows.iter().position(|slot| {
            slot.as_ref()
                .is_some_and(|row| Arc::ptr_eq(&row.cell, cell) && row.borrows == 0)
        }) {
            self.rows[at] = Some(new_row);
            return Some((entry, version, BorrowToken::Row(at)));
        }
        if let Some(at) = self.rows.iter().position(Option::is_none) {
            self.rows[at] = Some(new_row);
            return Some((entry, version, BorrowToken::Row(at)));
        }
        if self.rows.len() < Self::ROWS {
            let at = self.rows.len();
            self.rows.push(Some(new_row));
            return Some((entry, version, BorrowToken::Row(at)));
        }
        if let Some(least) = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                slot.as_ref()
                    .filter(|r| r.borrows == 0)
                    .map(|r| (i, r.used))
            })
            .min_by_key(|&(_, used)| used)
            .map(|(i, _)| i)
        {
            self.rows[least] = Some(new_row);
            return Some((entry, version, BorrowToken::Row(least)));
        }
        // All 64 rows are concurrently borrowed by nested calls: hold an owned pin.
        Some((entry, version, BorrowToken::Owned(new_row.code.0)))
    }

    /// Begin a dispatch through an untyped `RawSlot`.
    pub fn begin_dispatch(&mut self, slot: &RawSlot) -> Option<(usize, BorrowToken)> {
        self.begin_cell(&slot.lease.0)
            .map(|(entry, _, token)| (entry, token))
    }

    /// Release one dispatch borrow on return or unwind.
    pub fn end_dispatch(&mut self, token: BorrowToken) {
        match token {
            BorrowToken::Row(at) => {
                if let Some(row) = self.rows[at].as_mut() {
                    debug_assert!(row.borrows > 0);
                    row.borrows -= 1;
                    if row.borrows == 0 && row.cell.version.load(Ordering::Relaxed) != row.version {
                        self.rows[at] = None;
                    }
                }
            }
            BorrowToken::Owned(image) => drop(image),
            BorrowToken::Promoted => {}
        }
    }

    /// Promote an active borrow into an owning [`CodePin`] before its activation
    /// leaves worker custody (suspension / worker migration). After this returns,
    /// the worker may freely evict or drop the row.
    pub fn promote(&mut self, token: &mut BorrowToken) -> CodePin {
        match std::mem::replace(token, BorrowToken::Promoted) {
            BorrowToken::Row(at) => {
                let row = self.rows[at].as_mut().expect("borrowed worker row");
                let pin = CodePin(row.code.0.clone());
                debug_assert!(row.borrows > 0);
                row.borrows -= 1;
                if row.borrows == 0 && row.cell.version.load(Ordering::Relaxed) != row.version {
                    self.rows[at] = None;
                }
                pin
            }
            BorrowToken::Owned(image) => CodePin(image),
            BorrowToken::Promoted => CodePin::static_pin(),
        }
    }

    /// Call `with` on the slot's current entry, or return `None` for an empty slot. A
    /// hit reads the slot's version and writes nothing another thread reads.
    pub fn dispatch<F: Signature, R>(
        &mut self,
        slot: &Slot<F>,
        with: impl FnOnce(F) -> R,
    ) -> Option<R> {
        self.with_borrow(&slot.lease.0, |entry, _, _, _| {
            // SAFETY: the slot admits only F and the worker pins its generation through with.
            with(unsafe { std::mem::transmute_copy(&entry) })
        })
    }

    /// Call a registration selected from the slot's installation. A replacement
    /// cannot pair an ordinal resolved in the old directory with text in the new one.
    pub fn dispatch_entry<F: Signature, R>(
        &mut self,
        checkout: &Checkout,
        ordinal: usize,
        selection: &EntryIndex<F>,
        with: impl FnOnce(F) -> R,
    ) -> Result<Option<R>, Refusal> {
        const { assert!(size_of::<F>() == size_of::<usize>()) };
        if !Arc::ptr_eq(&self.shared, &checkout.lease.0.shared) {
            return Err(Refusal::NotAdmitted);
        }
        self.with_borrow(&checkout.lease.0, |_, version, worker, token| {
            if version != checkout.version {
                return Err(Refusal::NoEntry);
            }
            let image = match token {
                BorrowToken::Row(at) => worker.rows[*at]
                    .as_ref()
                    .and_then(|row| row.code.0.as_deref()),
                BorrowToken::Owned(image) => image.as_deref(),
                BorrowToken::Promoted => None,
            }
            .ok_or(Refusal::NoEntry)?;
            let candidate = selection.candidate();
            if !std::ptr::eq(image, candidate.image.as_ptr())
                || checkout.order.get(ordinal).map(|&i| i as usize) != Some(candidate.index)
            {
                return Err(Refusal::NoEntry);
            }
            let (entry, _) = image.addresses(candidate.index);
            // SAFETY: resolve checked F on this exact installation and the worker
            // owns the generation until the callback returns or unwinds.
            Ok(with(unsafe { std::mem::transmute_copy(&entry) }))
        })
        .transpose()
    }

    fn with_borrow<R>(
        &mut self,
        cell: &Arc<Cell>,
        with: impl FnOnce(usize, u64, &Self, &BorrowToken) -> R,
    ) -> Option<R> {
        let (entry, version, token) = self.begin_cell(cell)?;
        struct Guard<'a> {
            worker: &'a mut Worker,
            token: Option<BorrowToken>,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if let Some(token) = self.token.take() {
                    self.worker.end_dispatch(token);
                }
            }
        }
        let guard = Guard {
            worker: self,
            token: Some(token),
        };
        Some(with(
            entry,
            version,
            guard.worker,
            guard.token.as_ref().unwrap(),
        ))
    }

    /// Drop every unborrowed row whose slot has moved on. A worker calls this
    /// before it parks. Borrowed rows stay pinned until their calls finish or promote.
    pub fn park(&mut self) {
        for slot in &mut self.rows {
            if let Some(row) = slot
                && row.borrows == 0
                && row.cell.version.load(Ordering::Relaxed) != row.version
            {
                *slot = None;
            }
        }
    }

    /// Whether any row in this worker table currently has an active borrow.
    pub fn active_borrows(&self) -> u32 {
        self.rows
            .iter()
            .filter_map(|s| s.as_ref())
            .map(|r| r.borrows)
            .sum()
    }
}

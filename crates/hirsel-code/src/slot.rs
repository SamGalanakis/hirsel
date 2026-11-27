use super::{Code, Image, Shared, Signature, Stop, host};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Raw = (Arc<Image>, usize);

struct Cell {
    shared: Arc<Shared>,
    version: AtomicU64,
    current: Mutex<Option<Raw>>,
}

impl Drop for Cell {
    fn drop(&mut self) {
        let Some((image, _)) = self.current.get_mut().unwrap().take() else {
            return;
        };
        {
            let mut retired = self.shared.retired.lock().unwrap();
            if image.slots.fetch_sub(1, Ordering::Relaxed) == 1 {
                image.retired.store(true, Ordering::Relaxed);
                *retired += image.bytes;
            }
        }
        drop(image);
    }
}

/// A shared `(version, Code)` that workers dispatch through (PACK-6).
pub struct Slot<F> {
    cell: Arc<Cell>,
    callable: PhantomData<fn() -> F>,
}

impl<F> Clone for Slot<F> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            callable: PhantomData,
        }
    }
}

impl<F: Signature> Slot<F> {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            cell: Arc::new(Cell {
                shared,
                version: AtomicU64::new(0),
                current: Mutex::new(None),
            }),
            callable: PhantomData,
        }
    }

    pub fn version(&self) -> u64 {
        self.cell.version.load(Ordering::Acquire)
    }

    /// Store `code` when the slot is at version `expected`, and return the new version.
    /// It waits for no call, worker or unit that still uses the old code.
    pub fn activate(&self, expected: u64, code: Code<F>) -> Result<u64, Stop> {
        let cell = &*self.cell;
        let new = code.image.clone();
        let old;
        {
            let mut current = cell.current.lock().unwrap();
            let version = cell.version.load(Ordering::Relaxed);
            if version != expected {
                return Err(Stop::Stale(version));
            }
            let same = current
                .as_ref()
                .is_some_and(|(held, _)| Arc::ptr_eq(held, &new));
            if !same {
                let mut retired = cell.shared.retired.lock().unwrap();
                let returns = new.retired.load(Ordering::Relaxed);
                let leaves = current
                    .as_ref()
                    .map(|(held, _)| held)
                    .filter(|held| held.slots.load(Ordering::Relaxed) == 1);
                let after = *retired - if returns { new.bytes } else { 0 }
                    + leaves.map_or(0, |held| held.bytes);
                if leaves.is_some() && after > cell.shared.config.retired_bytes {
                    return Err(Stop::Capacity);
                }
                new.slots.fetch_add(1, Ordering::Relaxed);
                new.retired.store(false, Ordering::Relaxed);
                if let Some((held, _)) = &*current {
                    held.slots.fetch_sub(1, Ordering::Relaxed);
                }
                if let Some(held) = leaves {
                    held.retired.store(true, Ordering::Relaxed);
                }
                *retired = after;
            }
            old = current.replace((new, code.entry));
            cell.version.store(version + 1, Ordering::Release);
            cell.shared.updates.fetch_add(1, Ordering::Release);
        }
        // The old code's count is given up outside every lock.
        drop(old);
        Ok(expected + 1)
    }
}

struct Row {
    cell: Arc<Cell>,
    version: u64,
    code: Raw,
    used: u64,
}

/// One worker's private table of the code it dispatches to (PACK-6).
pub struct Worker {
    shared: Arc<Shared>,
    rows: Vec<Row>,
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

    /// Call `with` on the slot's current entry, or return `None` for an empty slot. A
    /// hit reads the slot's version and writes nothing another thread reads.
    pub fn dispatch<F: Signature, R>(
        &mut self,
        slot: &Slot<F>,
        with: impl FnOnce(F) -> R,
    ) -> Option<R> {
        let updates = self.shared.updates.load(Ordering::Relaxed);
        if updates != self.seen {
            self.seen = updates;
            self.park();
        }
        self.clock += 1;
        let cell = &slot.cell;
        let version = cell.version.load(Ordering::Relaxed);
        let at = self
            .rows
            .iter()
            .position(|row| Arc::ptr_eq(&row.cell, cell));
        let entry = match at {
            Some(at) if self.rows[at].version == version => {
                self.rows[at].used = self.clock;
                self.rows[at].code.1
            }
            _ => {
                let (version, code) = {
                    let current = cell.current.lock().unwrap();
                    (cell.version.load(Ordering::Relaxed), current.clone()?)
                };
                let entry = code.1;
                let row = Row {
                    cell: cell.clone(),
                    version,
                    code,
                    used: self.clock,
                };
                // The replaced row's count is given up after the slot's lock.
                match at {
                    Some(at) => self.rows[at] = row,
                    None if self.rows.len() < Self::ROWS => self.rows.push(row),
                    None => {
                        let least = (0..Self::ROWS)
                            .min_by_key(|at| self.rows[*at].used)
                            .unwrap();
                        self.rows[least] = row;
                    }
                }
                entry
            }
        };
        // SAFETY: as `Code::entry`; this worker's row owns a count while `with` runs.
        Some(host::execute(|| {
            with(unsafe { std::mem::transmute_copy(&entry) })
        }))
    }

    /// Drop every row whose slot has moved on. A worker calls this before it parks.
    pub fn park(&mut self) {
        self.rows
            .retain(|row| row.cell.version.load(Ordering::Relaxed) == row.version);
    }
}

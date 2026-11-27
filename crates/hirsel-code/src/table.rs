//! The PC table: which image owns a code address (PACK-9).
//!
//! The text band is divided into chunks. Each chunk has one immutable sorted array of
//! the rows that overlap it, reached through a directory. A writer builds new arrays
//! and swaps directory entries; a reader names the array it searches in a hazard slot,
//! so it allocates nothing and takes no lock, and a signal handler may be one.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};

const CHUNK: usize = 2 << 20;
/// How many readers can search at once without one waiting for another's slot.
const HAZARDS: usize = 64;

/// One image's text: its offset in the text band, its length and the first 16 bytes of
/// its module digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct Owner {
    pub(crate) start: u32,
    pub(crate) length: u32,
    pub(crate) digest: [u8; 16],
}

const _: () = assert!(size_of::<Owner>() == 24);

/// The image that owns a code address and the address's offset in the image's text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hit {
    /// The first 16 bytes of the module digest.
    pub digest: [u8; 16],
    pub offset: u32,
}

type Rows = AtomicPtr<Vec<Owner>>;

pub(crate) struct Table {
    base: usize,
    bytes: usize,
    chunks: Box<[Rows]>,
    hazards: [Rows; HAZARDS],
}

impl Table {
    pub(crate) fn new(base: usize, bytes: usize) -> Self {
        Self {
            base,
            bytes,
            chunks: (0..bytes.div_ceil(CHUNK))
                .map(|_| Rows::default())
                .collect(),
            hazards: [const { Rows::new(null_mut()) }; HAZARDS],
        }
    }

    /// The image whose text holds `pc`. Async-signal-safe.
    pub(crate) fn resolve(&self, pc: usize) -> Option<Hit> {
        let offset = pc
            .checked_sub(self.base)
            .filter(|offset| *offset < self.bytes)?;
        let entry = &self.chunks[offset / CHUNK];
        let mut rows = entry.load(Ordering::SeqCst);
        let slot = loop {
            if rows.is_null() {
                return None;
            }
            let free = self.hazards.iter().find(|slot| {
                slot.compare_exchange(null_mut(), rows, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            });
            if let Some(slot) = free {
                break slot;
            }
            std::hint::spin_loop();
            rows = entry.load(Ordering::SeqCst);
        };
        // The slot must name the array the directory holds after the slot was written:
        // a writer frees an array only once it is out of the directory and of every slot.
        loop {
            let current = entry.load(Ordering::SeqCst);
            if current == rows {
                break;
            }
            rows = current;
            slot.store(rows, Ordering::SeqCst);
            if rows.is_null() {
                return None;
            }
        }
        // SAFETY: the array was in the directory while the slot named it, so no writer
        // frees it before the slot is cleared.
        let held = unsafe { &*rows };
        let after = held.partition_point(|row| row.start as usize <= offset);
        let hit = after
            .checked_sub(1)
            .map(|at| held[at])
            .filter(|row| offset - (row.start as usize) < row.length as usize)
            .map(|row| Hit {
                digest: row.digest,
                offset: (offset - row.start as usize) as u32,
            });
        slot.store(null_mut(), Ordering::SeqCst);
        hit
    }

    /// Replace the rows that start in `[from, to)` of the text band with `rows`, which
    /// lie in it. One writer runs at a time; it returns when no reader holds an array
    /// it replaced.
    pub(crate) fn change(&self, (from, to): (usize, usize), rows: &[Owner]) {
        if to <= from {
            return;
        }
        let mut replaced = Vec::new();
        for chunk in from / CHUNK..=(to - 1) / CHUNK {
            let entry = &self.chunks[chunk];
            let held = entry.load(Ordering::SeqCst);
            let ends = (chunk * CHUNK, (chunk + 1) * CHUNK);
            let mut next: Vec<Owner> = Vec::new();
            if !held.is_null() {
                // SAFETY: only a writer frees an array, and this is the one writer.
                let kept = unsafe { &*held }.iter();
                next.extend(kept.filter(|row| !(from..to).contains(&(row.start as usize))));
            }
            next.extend(rows.iter().filter(|row| {
                (row.start as usize) < ends.1 && (row.start + row.length) as usize > ends.0
            }));
            next.sort_by_key(|row| row.start);
            let next = match next.is_empty() {
                true => null_mut(),
                false => Box::into_raw(Box::new(next)),
            };
            entry.store(next, Ordering::SeqCst);
            if !held.is_null() {
                replaced.push(held);
            }
        }
        for held in replaced {
            while self
                .hazards
                .iter()
                .any(|slot| slot.load(Ordering::SeqCst) == held)
            {
                std::thread::yield_now();
            }
            // SAFETY: the array left the directory above and no slot names it, and a
            // reader names an array only while the directory holds it.
            drop(unsafe { Box::from_raw(held) });
        }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        for entry in &mut self.chunks {
            let held = *entry.get_mut();
            if !held.is_null() {
                // SAFETY: nothing else reaches the table.
                drop(unsafe { Box::from_raw(held) });
            }
        }
    }
}

/// One live image's literal range in the data band, paired with its independent
/// R/NX `CauseData` owner and copied literal base address (PACK-9.7).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DataRow {
    pub(crate) start: u32,
    pub(crate) length: u32,
    pub(crate) owner: usize,
    pub(crate) ro_base: usize,
}

type DataRows = AtomicPtr<Vec<DataRow>>;

/// Lock-free hazard-protected lookup from a data-band literal range to its
/// independent `CauseData` owner (PACK-9.7). Takes no lock, allocates nothing,
/// and admits nothing on lookup.
pub(crate) struct DataTable {
    base: usize,
    bytes: usize,
    chunks: Box<[DataRows]>,
    hazards: [DataRows; HAZARDS],
}

impl DataTable {
    pub(crate) fn new(base: usize, bytes: usize) -> Self {
        Self {
            base,
            bytes,
            chunks: (0..bytes.div_ceil(CHUNK))
                .map(|_| DataRows::default())
                .collect(),
            hazards: [const { DataRows::new(null_mut()) }; HAZARDS],
        }
    }

    /// Resolve `[ptr, ptr + len)` in the data band, invoking `retain(owner)`
    /// while the hazard slot guarantees the owner's image has not yet released
    /// its count. Returns `(owner, relocated_ro_ptr)`.
    pub(crate) fn resolve_with(
        &self,
        ptr: usize,
        len: usize,
        retain: unsafe fn(usize),
    ) -> Option<(usize, usize)> {
        let offset = ptr
            .checked_sub(self.base)
            .filter(|offset| *offset < self.bytes)?;
        let end = offset.checked_add(len)?;
        if end > self.bytes {
            return None;
        }
        let entry = &self.chunks[offset / CHUNK];
        let mut rows = entry.load(Ordering::SeqCst);
        let slot = loop {
            if rows.is_null() {
                return None;
            }
            let free = self.hazards.iter().find(|slot| {
                slot.compare_exchange(null_mut(), rows, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            });
            if let Some(slot) = free {
                break slot;
            }
            std::hint::spin_loop();
            rows = entry.load(Ordering::SeqCst);
        };
        loop {
            let current = entry.load(Ordering::SeqCst);
            if current == rows {
                break;
            }
            rows = current;
            slot.store(rows, Ordering::SeqCst);
            if rows.is_null() {
                return None;
            }
        }
        // SAFETY: the hazard slot pins `rows` against writer reclamation, so
        // `Shared::reclaim` cannot release `row.owner` until we clear `slot`.
        let held = unsafe { &*rows };
        let after = held.partition_point(|row| row.start as usize <= offset);
        let found = after
            .checked_sub(1)
            .map(|at| held[at])
            .filter(|row| {
                let rel = offset - row.start as usize;
                rel.checked_add(len)
                    .is_some_and(|rel_end| rel_end <= row.length as usize)
            })
            .map(|row| {
                // SAFETY: image still holds a count on `row.owner` under hazard protection.
                unsafe { retain(row.owner) };
                (row.owner, row.ro_base + (offset - row.start as usize))
            });
        slot.store(null_mut(), Ordering::SeqCst);
        found
    }

    /// Replace rows starting in `[from, to)` of the data band with `rows`.
    /// Returns only after no reader holds any replaced array.
    pub(crate) fn change(&self, (from, to): (usize, usize), rows: &[DataRow]) {
        if to <= from {
            return;
        }
        let mut replaced = Vec::new();
        for chunk in from / CHUNK..=(to - 1) / CHUNK {
            let entry = &self.chunks[chunk];
            let held = entry.load(Ordering::SeqCst);
            let ends = (chunk * CHUNK, (chunk + 1) * CHUNK);
            let mut next: Vec<DataRow> = Vec::new();
            if !held.is_null() {
                // SAFETY: only the slab-lock holder writes to `DataTable`.
                let kept = unsafe { &*held }.iter();
                next.extend(kept.filter(|row| !(from..to).contains(&(row.start as usize))));
            }
            next.extend(rows.iter().filter(|row| {
                (row.start as usize) < ends.1 && (row.start + row.length) as usize > ends.0
            }));
            next.sort_by_key(|row| row.start);
            let next = match next.is_empty() {
                true => null_mut(),
                false => Box::into_raw(Box::new(next)),
            };
            entry.store(next, Ordering::SeqCst);
            if !held.is_null() {
                replaced.push(held);
            }
        }
        for held in replaced {
            while self
                .hazards
                .iter()
                .any(|slot| slot.load(Ordering::SeqCst) == held)
            {
                std::thread::yield_now();
            }
            // SAFETY: no directory entry or hazard slot names `held`.
            drop(unsafe { Box::from_raw(held) });
        }
    }
}

impl Drop for DataTable {
    fn drop(&mut self) {
        for entry in &mut self.chunks {
            let held = *entry.get_mut();
            if !held.is_null() {
                // SAFETY: nothing else reaches the table.
                drop(unsafe { Box::from_raw(held) });
            }
        }
    }
}

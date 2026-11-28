//! The band tables: which image owns a code address, and which cause owner holds a
//! literal range (PACK-9).
//!
//! A band is divided into chunks. Each chunk has one immutable sorted array of the
//! rows that overlap it, reached through a directory. A writer builds new arrays and
//! swaps directory entries; a reader names the array it searches in a hazard slot, so
//! it allocates nothing and takes no lock, and a signal handler may be one.

use super::Image;
use std::ptr::null_mut;
use std::sync::Weak;
use std::sync::atomic::{AtomicPtr, Ordering};

const CHUNK: usize = 2 << 20;
/// How many readers can search at once without one waiting for another's slot.
const HAZARDS: usize = 64;

/// A row of a band table: a range of the band and what a reader learns from it.
pub(crate) trait Row: Clone {
    /// The range's offset in the band and its length.
    fn range(&self) -> (u32, u32);
}

/// One image's text: its offset in the text band, its length, the first 16 bytes of
/// its module digest and the image itself, which a reader may pin while its hazard
/// slot names the row's array.
#[derive(Clone)]
pub(crate) struct Owner {
    pub(crate) start: u32,
    pub(crate) length: u32,
    pub(crate) digest: [u8; 16],
    pub(crate) image: Weak<Image>,
}

impl Row for Owner {
    fn range(&self) -> (u32, u32) {
        (self.start, self.length)
    }
}

/// The image that owns a code address and the address's offset in the image's text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hit {
    /// The first 16 bytes of the module digest.
    pub digest: [u8; 16],
    pub offset: u32,
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

impl Row for DataRow {
    fn range(&self) -> (u32, u32) {
        (self.start, self.length)
    }
}

type Rows<R> = AtomicPtr<Vec<R>>;

pub(crate) struct Table<R> {
    base: usize,
    bytes: usize,
    chunks: Box<[Rows<R>]>,
    hazards: [Rows<R>; HAZARDS],
}

impl<R: Row> Table<R> {
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

    /// What `with` makes of the row that holds all of `[at, at + length)` and of the
    /// range's offset in that row. `with` runs while the hazard slot names the row's
    /// array, so the writer that removes the row has not returned. Async-signal-safe
    /// when `with` is.
    pub(crate) fn find<T>(
        &self,
        at: usize,
        length: usize,
        with: impl FnOnce(&R, usize) -> T,
    ) -> Option<T> {
        let offset = at
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
        let after = held.partition_point(|row| row.range().0 as usize <= offset);
        let found = after
            .checked_sub(1)
            .map(|at| (&held[at], offset - held[at].range().0 as usize))
            .filter(|(row, inside)| {
                inside
                    .checked_add(length)
                    .is_some_and(|end| end <= row.range().1 as usize)
            })
            .map(|(row, inside)| with(row, inside));
        slot.store(null_mut(), Ordering::SeqCst);
        found
    }

    /// Replace the rows that start in `[from, to)` of the band with `rows`, which lie
    /// in it. One writer runs at a time; it returns when no reader holds an array it
    /// replaced.
    pub(crate) fn change(&self, (from, to): (usize, usize), rows: &[R]) {
        if to <= from {
            return;
        }
        let mut replaced = Vec::new();
        for chunk in from / CHUNK..=(to - 1) / CHUNK {
            let entry = &self.chunks[chunk];
            let held = entry.load(Ordering::SeqCst);
            let ends = (chunk * CHUNK, (chunk + 1) * CHUNK);
            let mut next: Vec<R> = Vec::new();
            if !held.is_null() {
                // SAFETY: only a writer frees an array, and this is the one writer.
                let kept = unsafe { &*held }.iter();
                let stays = |row: &&R| !(from..to).contains(&(row.range().0 as usize));
                next.extend(kept.filter(stays).cloned());
            }
            let overlaps = |row: &&R| {
                let (start, length) = row.range();
                (start as usize) < ends.1 && (start + length) as usize > ends.0
            };
            next.extend(rows.iter().filter(overlaps).cloned());
            next.sort_by_key(|row| row.range().0);
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

impl<R> Drop for Table<R> {
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

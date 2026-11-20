//! Shards claim aligned task-heap slabs from bitmap-managed ranges.
//!
//! A dropped slab stays claimed and resident in its shard's `retained` stack,
//! up to `RETAINED_SLABS`, and the next claim takes it first. Its native pages
//! go back to the host when the stack is full or when an idle worker calls
//! `super::purge`, which releases retained dedicated pages too.
use super::{Mapping, SLAB};
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const SHARD_COUNT: usize = 16;
// Use one bitmap word per range. Mimalloc's arenas also claim fixed-size
// slices from a bitmap; each Hirsel range has 64 independently owned slabs.
const RANGE_BYTES: usize = SLAB * u64::BITS as usize;
// Dropped slabs each shard keeps resident until a worker idles.
const RETAINED_SLABS: usize = 128;

static SHARDS: [Mutex<Shard>; SHARD_COUNT] = [const {
    Mutex::new(Shard {
        available: Vec::new(),
        retained: Vec::new(),
    })
}; SHARD_COUNT];
// Slabs on every shard's retained stack, so an idle worker with none skips the locks.
static RETAINED: AtomicUsize = AtomicUsize::new(0);

pub(super) fn retaining() -> bool {
    RETAINED.load(Ordering::Relaxed) != 0
}

struct Shard {
    available: Vec<NonNull<Range>>,
    /// Dropped slabs, still claimed in their range's bitmap and resident.
    retained: Vec<(NonNull<Range>, u32)>,
}

// SAFETY: all access to a shard's ranges holds the shard mutex. Each live
// slot pins its range, including when its block moves to another worker.
unsafe impl Send for Shard {}

struct Range {
    mapping: Mapping,
    free: u64,
    available_index: usize,
}

pub(crate) struct Slab {
    range: NonNull<Range>,
    start: NonNull<u8>,
    index: u32,
    shard: usize,
}

// SAFETY: a slab owns its claimed bit. The shard lock protects the bitmap;
// which worker holds the slab does not change the range's lifetime.
unsafe impl Send for Slab {}

impl Slab {
    pub(super) fn allocate(heap_address: usize) -> (Self, NonNull<u8>) {
        let memory = crate::units::memory();
        if let Some(slab) = memory.and_then(|memory| memory.take_slab()) {
            let start = slab.start;
            return (slab, start);
        }
        let shard = (heap_address ^ (heap_address >> 12)) / 16 % SHARD_COUNT;
        let mut pool = SHARDS[shard].lock().unwrap();
        if let Some((range, index)) = pool.retained.pop() {
            RETAINED.fetch_sub(1, Ordering::Relaxed);
            // SAFETY: a retained slab's claimed bit pins its range.
            let start = unsafe { range.as_ref() }.start(index);
            return (
                Self {
                    range,
                    start,
                    index,
                    shard,
                },
                start,
            );
        }
        if pool.available.is_empty() {
            if memory.is_some() {
                // Mapping may wait for the memory thread. It must be able to
                // finish releasing a slab in this shard before taking the job.
                drop(pool);
                let (mapping, _) = Mapping::aligned(RANGE_BYTES, SLAB, 0, false);
                pool = SHARDS[shard].lock().unwrap();
                pool.insert_range(mapping);
            } else {
                let (mapping, _) = Mapping::aligned(RANGE_BYTES, SLAB, 0, false);
                pool.insert_range(mapping);
            }
        }
        let mut range = *pool.available.last().unwrap();
        // SAFETY: the shard owns this nonfull range under its lock.
        let available = unsafe { range.as_mut() };
        let index = available.free.trailing_zeros();
        available.free &= !(1 << index);
        let start = available.start(index);
        if available.free == 0 {
            pool.available.pop();
        }
        (
            Self {
                range,
                start,
                index,
                shard,
            },
            start,
        )
    }

    /// Called by the memory thread for an empty cached slab. Retain its
    /// claimed bit until discard completes, so workers cannot reuse its pages
    /// while the syscall runs without the shard lock.
    pub(crate) fn dispose(self) {
        let slab = std::mem::ManuallyDrop::new(self);
        let mut shard = SHARDS[slab.shard].lock().unwrap();
        // SAFETY: the claimed bit pins the range and the shard lock owns it.
        let range = unsafe { slab.range.as_ref() };
        if range.free | (1 << slab.index) != u64::MAX {
            drop(shard);
            // SAFETY: the empty slab is still claimed, and this handle is its owner.
            unsafe { Mapping::discard(slab.start, SLAB) };
            shard = SHARDS[slab.shard].lock().unwrap();
        }
        let unused = shard.unclaim(slab.range, slab.index);
        drop(shard);
        drop(unused);
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        let mut shard = SHARDS[self.shard].lock().unwrap();
        if shard.retained.len() < RETAINED_SLABS {
            shard.retained.push((self.range, self.index));
            RETAINED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        shard.release(self.range, self.index);
    }
}

/// Return every retained slab's native pages to the host.
pub(super) fn purge() {
    if RETAINED.load(Ordering::Relaxed) == 0 {
        return;
    }
    for shard in &SHARDS {
        let mut shard = shard.lock().unwrap();
        while let Some((range, index)) = shard.retained.pop() {
            RETAINED.fetch_sub(1, Ordering::Relaxed);
            shard.release(range, index);
        }
    }
}

impl Shard {
    /// Unclaim an empty slab: unmap its range once every slab is free,
    /// otherwise discard its pages.
    fn release(&mut self, range_pointer: NonNull<Range>, index: u32) {
        // SAFETY: this slab keeps the range live while the shard lock is held.
        let start = unsafe { range_pointer.as_ref() }.start(index);
        if let Some(unused) = self.unclaim(range_pointer, index) {
            drop(unused);
        } else {
            // SAFETY: the shard lock withholds the empty slab from the next owner.
            unsafe { Mapping::discard(start, SLAB) };
        }
    }

    fn unclaim(&mut self, mut range_pointer: NonNull<Range>, index: u32) -> Option<Box<Range>> {
        // SAFETY: this slab's claimed bit pins the range. The shard lock
        // serializes bitmap updates and releasing the range.
        let range = unsafe { range_pointer.as_mut() };
        let was_full = range.free == 0;
        range.free |= 1 << index;
        if range.free == u64::MAX {
            self.remove(range.available_index);
            // SAFETY: all slab handles are gone and the shard unlinked it.
            return Some(unsafe { Box::from_raw(range_pointer.as_ptr()) });
        }
        if was_full {
            range.available_index = self.available.len();
            self.available.push(range_pointer);
        }
        None
    }

    fn insert_range(&mut self, mapping: Mapping) {
        let range = NonNull::from(Box::leak(Box::new(Range {
            mapping,
            free: u64::MAX,
            available_index: self.available.len(),
        })));
        self.available.push(range);
    }

    fn remove(&mut self, index: usize) {
        self.available.swap_remove(index);
        if let Some(range) = self.available.get_mut(index) {
            // SAFETY: the moved range belongs to this locked shard.
            unsafe { range.as_mut().available_index = index };
        }
    }
}

impl Range {
    fn start(&self, index: u32) -> NonNull<u8> {
        // SAFETY: each bitmap bit represents one aligned slab in this mapping.
        unsafe { self.mapping.base.add(index as usize * SLAB) }
    }
}

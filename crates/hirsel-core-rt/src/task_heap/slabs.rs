//! Shards claim aligned task-heap slabs from bitmap-managed ranges.
use super::{Mapping, SLAB};
use std::ptr::NonNull;
use std::sync::Mutex;

const SHARD_COUNT: usize = 16;
// Use one bitmap word per range. Mimalloc's arenas also claim fixed-size
// slices from a bitmap; each Hirsel range has 64 independently owned slabs.
const RANGE_BYTES: usize = SLAB * u64::BITS as usize;

static SHARDS: [Mutex<Shard>; SHARD_COUNT] = [const {
    Mutex::new(Shard {
        available: Vec::new(),
    })
}; SHARD_COUNT];

struct Shard {
    available: Vec<NonNull<Range>>,
}

// SAFETY: all access to a shard's ranges holds the shard mutex. Each live
// slot pins its range, including when its block moves to another worker.
unsafe impl Send for Shard {}

struct Range {
    mapping: Mapping,
    free: u64,
    available_index: usize,
}

pub(super) struct Slab {
    range: NonNull<Range>,
    index: u32,
    shard: usize,
}

impl Slab {
    pub(super) fn allocate(heap_address: usize) -> (Self, NonNull<u8>) {
        let shard = (heap_address ^ (heap_address >> 12)) / 16 % SHARD_COUNT;
        let mut pool = SHARDS[shard].lock().unwrap();
        let mut range = pool
            .available
            .last()
            .copied()
            .unwrap_or_else(|| pool.map_range());
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
                index,
                shard,
            },
            start,
        )
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        let mut shard = SHARDS[self.shard].lock().unwrap();
        // SAFETY: this handle's claimed bit pins the range. The shard lock
        // serializes bitmap updates and releasing the range.
        let range = unsafe { self.range.as_mut() };
        let was_full = range.free == 0;
        range.free |= 1 << self.index;
        if range.free == u64::MAX {
            shard.remove(range.available_index);
            // SAFETY: all slab handles are gone and the shard unlinked it.
            drop(unsafe { Box::from_raw(self.range.as_ptr()) });
        } else {
            // A live neighbor pins the address range. Give back the physical
            // pages in this slot before another heap can claim it.
            let start = range.start(self.index);
            // SAFETY: this slab is empty and still withheld by the shard lock.
            unsafe { Mapping::discard(start, SLAB) };
            if was_full {
                range.available_index = shard.available.len();
                shard.available.push(self.range);
            }
        }
    }
}

impl Shard {
    fn map_range(&mut self) -> NonNull<Range> {
        let (mapping, _) = Mapping::aligned(RANGE_BYTES, SLAB, 0, false);
        let range = NonNull::from(Box::leak(Box::new(Range {
            mapping,
            free: u64::MAX,
            available_index: self.available.len(),
        })));
        self.available.push(range);
        range
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

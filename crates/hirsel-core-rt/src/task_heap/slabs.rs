//! Shards claim aligned task-heap granules (64 KiB slabs and multi-granule
//! dedicated extents) from geometric, contiguous-prefix-committed OS ranges.
//!
//! Each `Range` reserves a power-of-two virtual span as `PROT_NONE` (0 bytes of
//! `VmData` and RSS) and commits a contiguous prefix `[0, committed)` via
//! `mprotect(..., PROT_READ | PROT_WRITE)`. Because every commit extends the
//! prefix contiguously and Linux merges adjacent anonymous VMAs with matching
//! protections, each live range stays at most two kernel mappings (one `rw-p`
//! prefix and one `---p` tail, collapsing to one `rw-p` VMA once full). Range
//! sizes double from 4 MiB up to 4 GiB, keeping the total VMA count logarithmic
//! in heap size across both slabs and large blocks.
//!
//! A dropped 64 KiB slab stays claimed and resident in its shard's `retained`
//! stack, and the next slab claim takes it first without a syscall. Its native
//! pages go back to the host when an idle worker calls `super::purge` or when a
//! large extent reclaims contiguous space. Releasing interior granules discards
//! their physical pages (`sys::discard`, never splitting a Linux VMA); freeing
//! the trailing committed prefix shrinks `committed` back to `PROT_NONE`
//! (returning `VmData`), and emptying a range unmaps it whole.
use super::{SLAB, host_memory, page_bytes};
use std::cell::Cell;
use std::num::NonZeroUsize;
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const SHARD_COUNT: usize = 16;
/// Smallest range reservation: 64 granules = 4 MiB (one bitmap word).
pub(crate) const MIN_RANGE_GRANULES: usize = u64::BITS as usize;
/// Cap on geometric doubling: 65,536 granules = 4 GiB. Single allocations
/// larger than this still reserve their exact power-of-two granule count.
pub(crate) const MAX_RANGE_GRANULES: usize = 65_536;
/// Maximum speculative commit step: 64 granules = 4 MiB.
const MAX_COMMIT_STEP: usize = 64;

const WRITABLE: libc::c_int = libc::PROT_READ | libc::PROT_WRITE;

static SHARDS: [Mutex<Shard>; SHARD_COUNT] = [const {
    Mutex::new(Shard {
        ranges: Vec::new(),
        retained: Vec::new(),
    })
}; SHARD_COUNT];
// Slabs on every shard's retained stack, so an idle worker with none skips the locks.
static RETAINED: AtomicUsize = AtomicUsize::new(0);

pub(super) fn retaining() -> bool {
    RETAINED.load(Ordering::Relaxed) != 0
}

fn shard_for(heap_address: usize) -> usize {
    (heap_address ^ (heap_address >> 12)) / 16 % SHARD_COUNT
}

struct Shard {
    ranges: Vec<NonNull<Range>>,
    /// Dropped 1-granule slabs, still claimed in their range's bitmap and resident.
    retained: Vec<(NonNull<Range>, u32)>,
}

// SAFETY: all access to a shard's ranges holds the shard mutex. Each live
// slab or extent pins its range, including when its block moves to another worker.
unsafe impl Send for Shard {}

struct Range {
    base: NonNull<u8>,
    /// Total capacity in `SLAB` granules (always a multiple of 64).
    granules: usize,
    /// Committed prefix in `SLAB` granules: `[0, committed)` is `PROT_READ | PROT_WRITE`,
    /// `[committed, granules)` is `PROT_NONE`.
    committed: usize,
    /// Number of currently claimed granules (`0` unmaps the range).
    live: usize,
    /// Highest claimed granule index + 1 (always `<= committed`).
    high_water: usize,
    /// Free bitmap: bit `i == 1` means granule `i` is free.
    free: Vec<u64>,
    /// Lowest word index in `free` that may hold a set bit.
    first_free_word: usize,
}

impl Drop for Range {
    fn drop(&mut self) {
        // SAFETY: the range has zero live granules and has been unlinked from its shard.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.granules * SLAB) };
    }
}

impl Range {
    fn new(base: NonNull<u8>, granules: usize) -> Self {
        debug_assert!(granules >= MIN_RANGE_GRANULES && granules.is_multiple_of(64));
        Self {
            base,
            granules,
            committed: 0,
            live: 0,
            high_water: 0,
            free: vec![u64::MAX; granules / 64],
            first_free_word: 0,
        }
    }

    /// Reserve an anonymous `PROT_NONE` mapping of `len` bytes whose `base` is
    /// `SLAB`-aligned and whose `base + prefix` is `align`-aligned.
    pub(crate) fn map_raw(len: usize, align: usize, prefix: usize) -> Option<NonNull<u8>> {
        let align = align.max(SLAB);
        let extra = if align > page_bytes() { align } else { 0 };
        let total = len.checked_add(extra)?;
        if total > isize::MAX as usize {
            return None;
        }
        #[cfg(test)]
        super::MAP_REQUESTS.with(|count| count.set(count.get() + 1));
        // SAFETY: reserve an anonymous private PROT_NONE mapping without replacing any address.
        let raw = unsafe {
            libc::mmap(
                ptr::null_mut(),
                total,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: a successful NULL-hint mapping is non-null. Trim excess head
        // and tail pages so `base` is SLAB-aligned and `base + prefix` is align-aligned.
        unsafe {
            let raw = NonNull::new_unchecked(raw.cast::<u8>());
            let payload = raw.add(prefix).map_addr(|address| {
                NonZeroUsize::new(address.get().next_multiple_of(align)).unwrap()
            });
            let base = payload.sub(prefix);
            let left = base.addr().get() - raw.addr().get();
            let right = total - left - len;
            if left != 0 {
                libc::munmap(raw.as_ptr().cast(), left);
            }
            if right != 0 {
                libc::munmap(base.add(len).as_ptr().cast(), right);
            }
            Some(base)
        }
    }

    fn start(&self, index: u32) -> NonNull<u8> {
        // SAFETY: `index < self.granules`, lying inside this mapping.
        unsafe { self.base.add(index as usize * SLAB) }
    }

    fn ensure_committed(&mut self, end: usize) -> bool {
        if end <= self.committed {
            return true;
        }
        let step = self.committed.clamp(1, MAX_COMMIT_STEP);
        let target = end.max((self.committed + step).min(self.granules));
        if self.commit_to(target) {
            return true;
        }
        // Under a tight RLIMIT_DATA, retry with the exact granules needed.
        target > end && self.commit_to(end)
    }

    fn commit_to(&mut self, target: usize) -> bool {
        let from = self.committed;
        let delta = target - from;
        // SAFETY: `[from, target)` lies within this range's reserved PROT_NONE tail.
        let ok = unsafe {
            libc::mprotect(
                self.base.add(from * SLAB).as_ptr().cast(),
                delta * SLAB,
                WRITABLE,
            ) == 0
        };
        if ok {
            self.committed = target;
        }
        ok
    }

    fn maybe_shrink(&mut self) {
        let trailing = self.committed - self.high_water;
        if trailing < (self.committed / 4).max(4) {
            return;
        }
        let from = self.high_water;
        // SAFETY: `[from, committed)` has no live granules and was discarded on unclaim.
        let ok = unsafe {
            crate::host::sys::decommit(self.base.add(from * SLAB).addr().get(), trailing * SLAB)
        };
        if ok {
            self.committed = from;
        }
    }

    fn claim_one(&mut self) -> Option<u32> {
        if self.live == self.granules {
            return None;
        }
        while self.first_free_word < self.free.len() && self.free[self.first_free_word] == 0 {
            self.first_free_word += 1;
        }
        let word = *self.free.get(self.first_free_word)?;
        let bit = word.trailing_zeros() as usize;
        let index = self.first_free_word * 64 + bit;
        if !self.ensure_committed(index + 1) {
            return None;
        }
        self.free[self.first_free_word] &= !(1u64 << bit);
        self.live += 1;
        self.high_water = self.high_water.max(index + 1);
        Some(index as u32)
    }

    fn claim_run(&mut self, count: usize, align: usize, prefix: usize) -> Option<u32> {
        if self.granules - self.live < count {
            return None;
        }
        let align_granules = (align.max(SLAB)) / SLAB;
        let prefix_granules = prefix / SLAB;
        let base_granule = self.base.addr().get() / SLAB;
        let offset = (base_granule + prefix_granules) & (align_granules - 1);
        let phase = (align_granules - offset) & (align_granules - 1);
        let start = self.find_run(count, align_granules, phase)?;
        if !self.ensure_committed(start + count) {
            return None;
        }
        self.clear_bits(start, count);
        self.live += count;
        self.high_water = self.high_water.max(start + count);
        Some(start as u32)
    }

    fn try_extend(&mut self, start: u32, old_count: u32, new_count: u32) -> bool {
        let start = start as usize;
        let old_end = start + old_count as usize;
        let new_end = start + new_count as usize;
        if new_end > self.granules {
            return false;
        }
        let added = new_end - old_end;
        if !self.all_free(old_end, added) {
            return false;
        }
        if !self.ensure_committed(new_end) {
            return false;
        }
        self.clear_bits(old_end, added);
        self.live += added;
        self.high_water = self.high_water.max(new_end);
        true
    }

    fn unclaim(&mut self, start: u32, count: u32) -> bool {
        let start = start as usize;
        let count = count as usize;
        self.set_bits(start, count);
        self.first_free_word = self.first_free_word.min(start / 64);
        self.live -= count;
        if self.live == 0 {
            self.high_water = 0;
            return true;
        }
        if start + count == self.high_water {
            self.recompute_high_water();
            self.maybe_shrink();
        }
        false
    }

    fn find_run(&mut self, count: usize, align: usize, phase: usize) -> Option<usize> {
        while self.first_free_word < self.free.len() && self.free[self.first_free_word] == 0 {
            self.first_free_word += 1;
        }
        let mut pos = align_up_phase(self.first_free_word * 64, align, phase);
        while pos + count <= self.granules {
            let mut cur = pos;
            let end = pos + count;
            let mut blocked = None;
            while cur < end {
                let word_index = cur / 64;
                let bit = cur % 64;
                let shifted = self.free[word_index] >> bit;
                let run = shifted.trailing_ones() as usize;
                let avail = 64 - bit;
                if cur + run >= end {
                    return Some(pos);
                }
                if run < avail {
                    blocked = Some(cur + run);
                    break;
                }
                cur += avail;
            }
            if let Some(zero_at) = blocked {
                pos = align_up_phase(zero_at + 1, align, phase);
            } else {
                return Some(pos);
            }
        }
        None
    }

    fn all_free(&self, start: usize, count: usize) -> bool {
        let mut cur = start;
        let end = start + count;
        while cur < end {
            let word_index = cur / 64;
            let bit = cur % 64;
            let bits = (end - cur).min(64 - bit);
            let mask = bit_mask(bit, bits);
            if self.free[word_index] & mask != mask {
                return false;
            }
            cur += bits;
        }
        true
    }

    fn clear_bits(&mut self, start: usize, count: usize) {
        let mut cur = start;
        let end = start + count;
        while cur < end {
            let word_index = cur / 64;
            let bit = cur % 64;
            let bits = (end - cur).min(64 - bit);
            self.free[word_index] &= !bit_mask(bit, bits);
            cur += bits;
        }
    }

    fn set_bits(&mut self, start: usize, count: usize) {
        let mut cur = start;
        let end = start + count;
        while cur < end {
            let word_index = cur / 64;
            let bit = cur % 64;
            let bits = (end - cur).min(64 - bit);
            self.free[word_index] |= bit_mask(bit, bits);
            cur += bits;
        }
    }

    fn recompute_high_water(&mut self) {
        let mut word = (self.high_water - 1) / 64;
        loop {
            let claimed = !self.free[word];
            if claimed != 0 {
                let top_bit = 63 - claimed.leading_zeros() as usize;
                self.high_water = word * 64 + top_bit + 1;
                return;
            }
            if word == 0 {
                self.high_water = 0;
                return;
            }
            word -= 1;
        }
    }
}

fn align_up_phase(pos: usize, align: usize, phase: usize) -> usize {
    let rem = pos & (align - 1);
    if rem <= phase {
        pos + (phase - rem)
    } else {
        pos + (align - rem) + phase
    }
}

fn bit_mask(bit: usize, bits: usize) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        ((1u64 << bits) - 1) << bit
    }
}

pub(crate) fn map_raw_range(len: usize, align: usize, prefix: usize) -> Option<NonNull<u8>> {
    Range::map_raw(len, align, prefix)
}

fn map_range(
    memory: Option<&'static dyn crate::units::SharedMemory>,
    granules: usize,
    min_granules: usize,
    align: usize,
    prefix: usize,
) -> Range {
    let map = |count: usize| -> Option<Range> {
        let len = count.checked_mul(SLAB)?;
        let base = if let Some(memory) = memory {
            memory.map_range(len, align, prefix)?
        } else {
            Range::map_raw(len, align, prefix)?
        };
        Some(Range::new(base, count))
    };
    if let Some(range) = map(granules) {
        return range;
    }
    if min_granules < granules
        && let Some(range) = map(min_granules)
    {
        return range;
    }
    host_memory()
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
    pub(crate) fn allocate(heap_address: usize) -> (Self, NonNull<u8>) {
        if let Err(stop) = crate::fiber::room(crate::fiber::RUNTIME_ROOM) {
            crate::run_stop::raise(stop);
        }
        let memory = crate::units::memory();
        if let Some(slab) = memory.and_then(|memory| memory.take_slab()) {
            let start = slab.start;
            return (slab, start);
        }
        let shard = shard_for(heap_address);
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
        if let Some((range, index, start)) = pool.claim_slab() {
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
        let (want, min) = match pool.try_next_range_granules(1) {
            Ok(pair) => pair,
            Err(stop) => {
                drop(pool);
                crate::run_stop::raise(stop);
            }
        };
        drop(pool);
        let fresh = map_range(memory, want, min, SLAB, 0);
        let mut pool = SHARDS[shard].lock().unwrap();
        let range = pool.insert_range(fresh);
        // SAFETY: `range` was just inserted under this shard lock and is empty.
        let claimed = unsafe {
            let r = range.as_ptr().as_mut().unwrap();
            r.claim_one().map(|index| (index, r.start(index)))
        };
        let Some((index, start)) = claimed else {
            drop(pool);
            host_memory()
        };
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

    /// Called by the memory thread (or `Page::dispose`) for an empty slab.
    /// Retain its claimed bit until discard completes, so workers cannot reuse
    /// its pages while the syscall runs without the shard lock.
    pub(crate) fn dispose(self) {
        let slab = std::mem::ManuallyDrop::new(self);
        release_extent(slab.shard, slab.range, slab.start, slab.index, 1);
    }
}

impl Drop for Slab {
    #[inline(never)]
    fn drop(&mut self) {
        let mut shard = SHARDS[self.shard].lock().unwrap();
        shard.retained.push((self.range, self.index));
        RETAINED.fetch_add(1, Ordering::Relaxed);
    }
}

/// A multi-granule dedicated extent carved from a shard's geometric range.
pub(crate) struct Extent {
    range: NonNull<Range>,
    base: NonNull<u8>,
    start: u32,
    count: Cell<u32>,
    shard: usize,
    /// `0` when no ownership prefix precedes the payload, or `SLAB` so the
    /// payload stays `SLAB`-aligned with a full leading granule for its prefix.
    prefix: usize,
    /// Payload extent that may have been touched since the last purge.
    pub(super) used: Cell<usize>,
    /// Starting stride of the last live allocation, before in-place growth.
    pub(super) born: Cell<usize>,
    /// Whether a live block grew in place since the last purge.
    pub(super) grown: Cell<bool>,
}

// SAFETY: an idle extent is uniquely owned. Moving it preserves its address;
// only its owner changes `count`/`used`/`born`/`grown`, and bitmap mutations
// hold the shard mutex.
unsafe impl Send for Extent {}

impl Extent {
    pub(super) fn allocate(
        heap_address: usize,
        bytes: usize,
        align: usize,
        prefix: usize,
    ) -> (Self, NonNull<u8>) {
        if let Err(stop) = crate::fiber::room(crate::fiber::RUNTIME_ROOM) {
            crate::run_stop::raise(stop);
        }
        let bytes = bytes
            .checked_next_multiple_of(page_bytes())
            .unwrap_or_else(host_memory);
        let total = bytes.checked_add(prefix).unwrap_or_else(host_memory);
        let granules = total.div_ceil(SLAB);
        let align = align.max(SLAB);
        let memory = crate::units::memory();
        if let Some(extent) = memory.and_then(|memory| memory.take_page(granules, align, prefix)) {
            extent.born.set(0);
            extent.grown.set(false);
            // SAFETY: `prefix` lies inside the extent.
            let payload = unsafe { extent.base.add(prefix) };
            return (extent, payload);
        }
        let shard = shard_for(heap_address);
        let mut pool = SHARDS[shard].lock().unwrap();
        let mut claimed = pool.claim_run(granules, align, prefix);
        if claimed.is_none() && !pool.retained.is_empty() {
            let retained = std::mem::take(&mut pool.retained);
            RETAINED.fetch_sub(retained.len(), Ordering::Relaxed);
            drop(pool);
            for (range, index) in retained {
                // SAFETY: each retained slab's claimed bit pins its range until released.
                let start = unsafe { range.as_ref() }.start(index);
                release_extent(shard, range, start, index, 1);
            }
            pool = SHARDS[shard].lock().unwrap();
            claimed = pool.claim_run(granules, align, prefix);
        }
        let (range, start, base) = if let Some(claimed) = claimed {
            drop(pool);
            claimed
        } else {
            let (want, min) = match pool.try_next_range_granules(granules) {
                Ok(pair) => pair,
                Err(stop) => {
                    drop(pool);
                    crate::run_stop::raise(stop);
                }
            };
            drop(pool);
            let fresh = map_range(memory, want, min, align, prefix);
            pool = SHARDS[shard].lock().unwrap();
            let range = pool.insert_range(fresh);
            // SAFETY: `range` was just inserted under this shard lock and aligned
            // so granule 0 satisfies `align` at `base + prefix`.
            let found = unsafe {
                let r = range.as_ptr().as_mut().unwrap();
                r.claim_run(granules, align, prefix)
                    .map(|start| (range, start, r.start(start)))
            };
            drop(pool);
            found.unwrap_or_else(|| host_memory())
        };
        // SAFETY: `prefix` lies inside the claimed extent.
        let payload = unsafe { base.add(prefix) };
        let extent = Self {
            range,
            base,
            start,
            count: Cell::new(granules as u32),
            shard,
            prefix,
            used: Cell::new(0),
            born: Cell::new(0),
            grown: Cell::new(false),
        };
        (extent, payload)
    }

    pub(crate) fn granules(&self) -> usize {
        self.count.get() as usize
    }

    pub(crate) fn prefix(&self) -> usize {
        self.prefix
    }

    pub(crate) fn payload_aligned(&self, align: usize) -> bool {
        (self.base.addr().get() + self.prefix) & (align - 1) == 0
    }

    pub(crate) fn committed_bytes(&self) -> usize {
        self.count.get() as usize * SLAB
    }

    #[cfg(test)]
    pub(super) fn raw_span(&self) -> (NonNull<u8>, usize) {
        (self.base, self.committed_bytes())
    }

    pub(super) fn headroom(&self, payload: NonNull<u8>) -> usize {
        debug_assert_eq!(payload.addr().get() - self.base.addr().get(), self.prefix);
        self.prefix
    }

    fn touched_prefix(&self) -> usize {
        if self.prefix == 0 { 0 } else { page_bytes() }
    }

    pub(super) fn extent(&self, _payload: NonNull<u8>) -> usize {
        self.committed_bytes() - self.prefix
    }

    #[inline(never)]
    pub(super) fn extend(&self, _payload: NonNull<u8>, bytes: usize) -> bool {
        let Some(end) = bytes.checked_next_multiple_of(page_bytes()) else {
            return false;
        };
        let Some(total) = end.checked_add(self.prefix) else {
            return false;
        };
        let needed = total.div_ceil(SLAB);
        let current = self.count.get() as usize;
        if needed <= current {
            return true;
        }
        let Ok(needed_u32) = u32::try_from(needed) else {
            return false;
        };
        let _pool = SHARDS[self.shard].lock().unwrap();
        // SAFETY: this extent pins its range, and the shard lock serializes bitmap updates.
        let range = unsafe { self.range.as_ptr().as_mut().unwrap() };
        if !range.try_extend(self.start, self.count.get(), needed_u32) {
            return false;
        }
        self.count.set(needed_u32);
        true
    }

    pub(super) fn backed(&self, _payload: NonNull<u8>) -> usize {
        let used = self.used.get();
        if used == 0 {
            0
        } else {
            self.touched_prefix() + used
        }
    }

    #[inline(never)]
    pub(super) fn use_extent(&self, payload: NonNull<u8>, bytes: usize) -> usize {
        self.used
            .set(self.used.get().max(bytes.next_multiple_of(page_bytes())));
        self.backed(payload)
    }
}

impl Drop for Extent {
    fn drop(&mut self) {
        release_extent(
            self.shard,
            self.range,
            self.base,
            self.start,
            self.count.get(),
        );
    }
}

fn release_extent(
    shard_index: usize,
    range_pointer: NonNull<Range>,
    base: NonNull<u8>,
    start: u32,
    count: u32,
) {
    let mut shard = SHARDS[shard_index].lock().unwrap();
    // SAFETY: the claimed granules pin the range under the shard lock.
    let empties_range = unsafe { range_pointer.as_ref().live } == count as usize;
    if !empties_range {
        drop(shard);
        // SAFETY: the granules are still claimed and exclusively owned by this caller.
        unsafe { crate::host::sys::discard(base.addr().get(), count as usize * SLAB) };
        shard = SHARDS[shard_index].lock().unwrap();
    }
    let unused = shard.unclaim(range_pointer, start, count);
    drop(shard);
    drop(unused);
}

/// Return every retained slab's native pages to the host.
pub(super) fn purge() {
    if RETAINED.load(Ordering::Relaxed) == 0 {
        return;
    }
    for (shard_index, shard) in SHARDS.iter().enumerate() {
        let retained = {
            let mut shard = shard.lock().unwrap();
            if shard.retained.is_empty() {
                continue;
            }
            let retained = std::mem::take(&mut shard.retained);
            RETAINED.fetch_sub(retained.len(), Ordering::Relaxed);
            retained
        };
        for (range, index) in retained {
            // SAFETY: each retained slab's claimed bit pins its range until released.
            let start = unsafe { range.as_ref() }.start(index);
            release_extent(shard_index, range, start, index, 1);
        }
    }
}

impl Shard {
    fn total_granules(&self) -> usize {
        self.ranges
            .iter()
            .map(|range| {
                // SAFETY: the shard lock owns every listed range.
                unsafe { range.as_ref().granules }
            })
            .sum()
    }

    fn try_next_range_granules(
        &self,
        need: usize,
    ) -> Result<(usize, usize), crate::run_stop::Stop> {
        let min = need
            .max(MIN_RANGE_GRANULES)
            .checked_next_multiple_of(MIN_RANGE_GRANULES)
            .ok_or_else(|| crate::run_stop::Stop::from_static(b"HostMemory"))?;
        let required = need
            .max(MIN_RANGE_GRANULES)
            .checked_next_power_of_two()
            .unwrap_or(min);
        let geometric = self
            .total_granules()
            .max(MIN_RANGE_GRANULES)
            .checked_next_power_of_two()
            .unwrap_or(MAX_RANGE_GRANULES)
            .min(MAX_RANGE_GRANULES);
        Ok((geometric.max(required), min))
    }

    fn claim_slab(&mut self) -> Option<(NonNull<Range>, u32, NonNull<u8>)> {
        for &range_pointer in &self.ranges {
            // SAFETY: the shard lock owns every listed range.
            let range = unsafe { range_pointer.as_ptr().as_mut().unwrap() };
            if let Some(index) = range.claim_one() {
                return Some((range_pointer, index, range.start(index)));
            }
        }
        None
    }

    fn claim_run(
        &mut self,
        count: usize,
        align: usize,
        prefix: usize,
    ) -> Option<(NonNull<Range>, u32, NonNull<u8>)> {
        for &range_pointer in &self.ranges {
            // SAFETY: the shard lock owns every listed range.
            let range = unsafe { range_pointer.as_ptr().as_mut().unwrap() };
            if let Some(start) = range.claim_run(count, align, prefix) {
                return Some((range_pointer, start, range.start(start)));
            }
        }
        None
    }

    fn insert_range(&mut self, range: Range) -> NonNull<Range> {
        let pointer = NonNull::from(Box::leak(Box::new(range)));
        self.ranges.push(pointer);
        pointer
    }

    fn unclaim(
        &mut self,
        range_pointer: NonNull<Range>,
        start: u32,
        count: u32,
    ) -> Option<Box<Range>> {
        // SAFETY: the claimed granules pin the range under the shard lock.
        let range = unsafe { range_pointer.as_ptr().as_mut().unwrap() };
        if range.unclaim(start, count) {
            if let Some(position) = self.ranges.iter().position(|&entry| entry == range_pointer) {
                self.ranges.swap_remove(position);
            }
            // SAFETY: all handles into this range are gone and it is unlinked.
            return Some(unsafe { Box::from_raw(range_pointer.as_ptr()) });
        }
        None
    }
}

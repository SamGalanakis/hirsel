//! Nonmoving bump pools and one logical charge accumulator per payer run.
use crate::allocator::{allocate_raw, deallocate_untracked};
use crate::{Capacity, Context, limit::Payer};
use std::alloc::Layout;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::ptr::{self, NonNull};
use std::rc::{Rc, Weak};

/// Region backing starts at one granule. Subsequent chunks can use its
/// remainder as the region-wide growth allowance permits.
pub(crate) const GRANULE: usize = 4096;

type PoolKey = (usize, usize);
#[allow(clippy::vec_box)] // Keeping the box reuses the pinned ABI allocation.
type Recycled = RefCell<Vec<Box<Region>>>;

/// Only the owning task opens or closes these non-send regions. A weak
/// backreference lets the Rust API's region outlive its original context.
#[derive(Default)]
pub(crate) struct RegionCache(OnceCell<Rc<Recycled>>);

#[derive(Clone, Copy)]
struct Cursor {
    next: *mut u8,
    end: *mut u8,
    capacity: usize,
}
impl Cursor {
    const EMPTY: Self = Self {
        next: ptr::null_mut(),
        end: ptr::null_mut(),
        capacity: 0,
    };
}

#[derive(Clone, Copy)]
struct Entry {
    payer: Payer,
    charge: u64,
}
struct Chunk {
    pointer: NonNull<u8>,
    layout: Layout,
}

impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: the cached backing owns this disjoint native allocation.
        unsafe { deallocate_untracked(self.pointer.as_ptr()) };
        #[cfg(test)]
        CHUNK_RELEASES.with(|count| count.set(count.get() + 1));
    }
}

/// The active pool and last-entry index are published for emitter inlining. A builder
/// remembers its entry index and its own retained charge, never an object header.
#[repr(C)]
pub struct Region {
    pub(crate) cursor: *mut u8,
    pub(crate) end: *mut u8,
    pub(crate) entry: usize,
    pub(crate) payer: *mut crate::limit::LimitRecord,
    pub(crate) charge: u64,
    pub(crate) size: usize,
    pub(crate) alignment: usize,
    pub(crate) occupied: usize,
    pub(crate) stride: usize,
    capacity: usize,
    reserved: usize,
    used_chunks: usize,
    previous_used_chunks: usize,
    pools: HashMap<PoolKey, Cursor>,
    cache: Weak<Recycled>,
    inline: [Option<Entry>; 4],
    extra: Vec<Entry>,
    chunks: Vec<Chunk>,
}
impl Region {
    pub fn open(context: &Context) -> Box<Self> {
        let cache = context.regions.0.get_or_init(Rc::default);
        let mut region = cache
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Box::new(Self::new(Rc::downgrade(cache))));
        region.pools.clear();
        region
    }

    fn new(cache: Weak<Recycled>) -> Self {
        Self {
            cursor: ptr::null_mut(),
            end: ptr::null_mut(),
            entry: usize::MAX,
            payer: ptr::null_mut(),
            charge: 0,
            size: 0,
            alignment: 0,
            occupied: 0,
            stride: 0,
            capacity: 0,
            reserved: 0,
            used_chunks: 0,
            previous_used_chunks: 0,
            pools: HashMap::new(),
            cache,
            inline: [None; 4],
            extra: Vec::new(),
            chunks: Vec::new(),
        }
    }
    #[inline(always)]
    fn entry_mut(&mut self, index: usize) -> &mut Entry {
        if index < 4 {
            self.inline[index].as_mut().unwrap()
        } else {
            &mut self.extra[index - 4]
        }
    }
    #[inline(always)]
    fn charge(&mut self, payer: Payer, charge: u64) {
        if self.charge != 0 && self.payer == payer.record_pointer() {
            self.charge = self.charge.wrapping_add(charge);
        } else {
            self.change_payer(payer, charge);
        }
    }
    #[cold]
    fn change_payer(&mut self, payer: Payer, charge: u64) {
        if !self.payer.is_null() {
            let last = Entry {
                payer: Payer::from_current(self.payer),
                charge: self.charge,
            };
            if self.entry < 4 {
                self.inline[self.entry] = Some(last);
            } else {
                self.extra.push(last);
            }
        }
        self.entry = self.entry.wrapping_add(1);
        self.payer = payer.record_pointer();
        self.charge = charge;
    }
    #[inline]
    pub fn allocate(
        &mut self,
        context: &Context,
        bytes: usize,
        align: usize,
        charge: Option<u64>,
    ) -> Result<NonNull<u8>, Capacity> {
        let payer = context.payer();
        payer.admit(charge)?;
        Ok(self.allocate_admitted(payer, bytes, align, charge.unwrap()))
    }

    #[inline]
    pub(crate) fn allocate_admitted(
        &mut self,
        payer: Payer,
        bytes: usize,
        align: usize,
        charge: u64,
    ) -> NonNull<u8> {
        if charge != 0 {
            self.charge(payer, charge);
        }
        self.bump(bytes, align)
    }

    #[inline]
    fn bump(&mut self, bytes: usize, align: usize) -> NonNull<u8> {
        if (self.size, self.alignment) == (bytes, align) {
            let stride = self.stride;
            if stride <= self.end.addr() - self.cursor.addr() {
                return self.take(stride);
            }
        }
        self.refill(bytes, align)
    }

    fn take(&mut self, stride: usize) -> NonNull<u8> {
        let pointer = self.cursor;
        self.cursor = pointer.wrapping_add(stride);
        self.occupied += stride;
        // SAFETY: the active chunk owns this aligned, disjoint slot.
        unsafe { NonNull::new_unchecked(pointer) }
    }

    #[cold]
    pub(crate) fn refill(&mut self, bytes: usize, align: usize) -> NonNull<u8> {
        let slot = Layout::from_size_align(bytes.max(1), align)
            .unwrap_or_else(|_| crate::process::stop(b"HostMemory"))
            .pad_to_align();
        let stride = slot.size();
        let key = (bytes, align);
        let active = (self.size, self.alignment);
        if key != active {
            if self.capacity != 0 {
                self.pools.insert(
                    active,
                    Cursor {
                        next: self.cursor,
                        end: self.end,
                        capacity: self.capacity,
                    },
                );
            }
            let cursor = self.pools.remove(&key).unwrap_or(Cursor::EMPTY);
            self.cursor = cursor.next;
            self.end = cursor.end;
            self.capacity = cursor.capacity;
            (self.size, self.alignment) = key;
            self.stride = stride;
            if stride <= self.end.addr() - self.cursor.addr() {
                return self.take(stride);
            }
        }
        // Adapt bumpalo's geometric refill to a region-wide ceiling. A pool
        // miss may leave a tail in another pool, so doubling alone can overshoot.
        let occupied = self
            .occupied
            .checked_add(stride)
            .unwrap_or_else(|| crate::process::stop(b"HostMemory"));
        let room = occupied.saturating_mul(2).saturating_add(GRANULE) - self.reserved;
        let desired = if self.reserved == 0 {
            stride
                .checked_next_multiple_of(GRANULE)
                .unwrap_or_else(|| crate::process::stop(b"HostMemory"))
        } else {
            self.capacity.saturating_mul(2).max(stride)
        };
        let layout = Layout::from_size_align(desired.min(room), align)
            .unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
        let pointer = self.chunk(layout);
        self.capacity = layout.size();
        self.reserved += layout.size();
        self.cursor = pointer.as_ptr();
        self.end = pointer.as_ptr().wrapping_add(layout.size());
        let result = self.take(stride);
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::region_refill(self, layout.size());
        result
    }

    fn chunk(&mut self, layout: Layout) -> NonNull<u8> {
        let i = self.used_chunks;
        if let Some(found) = self.chunks[i..]
            .iter()
            .position(|chunk| chunk.layout == layout)
        {
            self.chunks.swap(i, i + found);
        } else {
            let chunk = Chunk {
                pointer: allocate_raw(layout),
                layout,
            };
            #[cfg(test)]
            CHUNK_REQUESTS.with(|count| count.set(count.get() + 1));
            if i == self.chunks.len() {
                self.chunks.push(chunk);
            } else {
                self.chunks[i] = chunk;
            }
        }
        self.used_chunks += 1;
        self.chunks[i].pointer
    }

    /// Refund only this builder's tail through its original payer entry.
    /// # Safety
    /// The live builder owns `previous` bytes in `entry`, and retains at most
    /// that amount. No other builder lowers this reservation.
    pub unsafe fn lower_charge(&mut self, entry: usize, previous: u64, retained: u64) {
        let refund = previous - retained;
        if refund == 0 {
            return;
        }
        if entry == self.entry {
            Payer::from_current(self.payer).refund(refund);
            self.charge = self.charge.wrapping_sub(refund);
        } else {
            let record = self.entry_mut(entry);
            record.payer.refund(refund);
            record.charge = record.charge.wrapping_sub(refund);
        }
    }
    pub fn physical_bytes(&self) -> usize {
        self.reserved
    }

    fn refund(&mut self) {
        if self.charge != 0 {
            Payer::from_current(self.payer).refund(self.charge);
        }
        for entry in self.inline.iter().flatten().chain(self.extra.iter()) {
            entry.payer.refund(entry.charge);
        }
        self.charge = 0;
        self.inline.fill(None);
        self.extra.clear();
    }

    /// Return the recently used chunks and their metadata to the cache.
    /// Closing refunds payer runs and can release one stale chunk.
    pub fn close(mut self: Box<Self>) {
        self.refund();
        if let Some(cache) = self.cache.upgrade() {
            if self.used_chunks != 0 {
                // Adapt bumpalo's reset: keep enough chunks for the
                // larger of the last two uses and retire unused chunks one at a time.
                if self.chunks.len() > self.used_chunks.max(self.previous_used_chunks) {
                    self.chunks.pop();
                }
                self.previous_used_chunks = self.used_chunks;
            }
            self.cursor = ptr::null_mut();
            self.end = ptr::null_mut();
            self.entry = usize::MAX;
            self.payer = ptr::null_mut();
            self.size = 0;
            self.alignment = 0;
            self.occupied = 0;
            self.stride = 0;
            self.capacity = 0;
            self.reserved = 0;
            self.used_chunks = 0;
            cache.borrow_mut().push(self);
        }
    }
}

#[cfg(test)]
pub(crate) fn cached_backing(context: &Context) -> (usize, usize) {
    let cache = context.regions.0.get().unwrap().borrow();
    let chunks = &cache.last().unwrap().chunks;
    (
        chunks.len(),
        chunks.iter().map(|chunk| chunk.layout.size()).sum(),
    )
}

impl Drop for Region {
    fn drop(&mut self) {
        self.refund();
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static CHUNK_REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static CHUNK_RELEASES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

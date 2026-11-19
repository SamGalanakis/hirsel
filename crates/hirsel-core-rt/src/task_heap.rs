//! Fixed-slot pages with one payer and one owner per page. Like mimalloc's
//! page queues, a class keeps only pages that can supply a block. The task
//! owns the queue even if it resumes on a different worker. As mimalloc
//! retires a queue's last page lazily, a queue keeps at most one empty page,
//! at its head, with its free list intact: a block that is allocated and
//! freed alone reuses its slot without resetting the page.
mod slabs;

use crate::{abi_layout::own_prefix, limit::Payer};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const SLAB: usize = 64 * 1024;
const MIN_PAGE_BYTES: usize = 4096;
const GROWTH_RESERVE_FACTOR: usize = 8;
const MIN_GROWTH_RESERVATION: usize = 256 * 1024;
// Bound clean reservations and their external descriptors.
const EMPTY_DEDICATED_LIMIT: usize = 8;
const DEDICATED_PURGE_DELAY: Duration = Duration::from_millis(100);
const MAX_RETAINED_DEDICATED_BYTES: usize = 4 * 1024 * 1024;
const SLAB_BINS: usize = size_bin(SLAB / 2) as usize + 1;

const fn size_bin(stride: usize) -> u8 {
    if stride <= 64 {
        return (stride / 8 - 1) as u8;
    }
    // Four classes per doubling above 64 bytes. Subtract one so the
    // power-of-two endpoint belongs to the preceding group.
    let group = (stride - 1).ilog2();
    let lane = ((stride - 1) >> (group - 2)) & 3;
    (8 + (group as usize - 6) * 4 + lane) as u8
}

// Like Go's size_to_class8 and mimalloc's direct pages, word-aligned blocks
// up to DIRECT_WORDS words take their stride and bin from a table.
const DIRECT_WORDS: usize = 128;
#[derive(Clone, Copy)]
#[repr(C, packed)]
struct DirectClass {
    stride: u16,
    bin: u8,
}

const DIRECT: [DirectClass; DIRECT_WORDS + 1] = {
    let mut table = [DirectClass { stride: 0, bin: 0 }; DIRECT_WORDS + 1];
    let mut words = 1;
    while words <= DIRECT_WORDS {
        let bytes = words * 8;
        let unit = if bytes <= 64 {
            64
        } else {
            bytes.next_power_of_two()
        } / 8;
        let stride = bytes.next_multiple_of(unit);
        table[words] = DirectClass {
            stride: stride as u16,
            bin: size_bin(stride),
        };
        words += 1;
    }
    table
};

// Direct classes are below the smallest native page and fit a slab.
const _: () = assert!(
    DIRECT[DIRECT_WORDS].stride as usize <= MIN_PAGE_BYTES
        && (DIRECT[DIRECT_WORDS].bin as usize) < SLAB_BINS
);

#[derive(Clone, Copy)]
struct Class {
    stride: usize,
    align: usize,
    kind: u8,
    bin: u8,
    growable: bool,
}
impl Class {
    fn for_block(bytes: usize, align: usize, kind: u8, growable: bool) -> Self {
        if let Some(class) = Self::direct(bytes, align, kind, growable) {
            return class;
        }
        // Alignments and bins are powers of two, so rounding is a mask.
        let align = align.max(8);
        let offset = round_up(own_prefix(kind), align).unwrap_or_else(host_memory);
        let bytes = bytes.max(1).checked_add(offset).unwrap_or_else(host_memory);
        let bin = bytes
            .max(64)
            .checked_next_power_of_two()
            .unwrap_or_else(host_memory)
            / 8;
        let stride = round_up(bytes, align.max(bin)).unwrap_or_else(host_memory);
        Self {
            stride,
            align,
            kind,
            bin: size_bin(stride),
            growable,
        }
    }
    /// A word-aligned class from the table. Every ownership prefix is a
    /// whole number of words.
    #[inline(always)]
    fn direct(bytes: usize, align: usize, kind: u8, growable: bool) -> Option<Self> {
        if align > 8 || bytes > DIRECT_WORDS * 8 {
            return None;
        }
        let words = (bytes.max(1) + own_prefix(kind)).div_ceil(8);
        let class = DIRECT.get(words)?;
        Some(Self {
            stride: usize::from(class.stride),
            align: 8,
            kind,
            bin: class.bin,
            growable,
        })
    }

    fn offset(self) -> usize {
        own_prefix(self.kind).next_multiple_of(self.align)
    }
    fn fits_slab(self) -> bool {
        // Small growable slots share a slab until they exceed a native page.
        (self.bin as usize) < SLAB_BINS && (!self.growable || self.stride <= page_bytes())
    }
}

#[inline(always)]
fn round_up(bytes: usize, unit: usize) -> Option<usize> {
    Some(bytes.checked_add(unit - 1)? & !(unit - 1))
}

#[derive(Clone, Copy)]
struct Key {
    payer: usize,
    class: Class,
}

type PagePointer = NonNull<Page>;

struct IndexedPage(PagePointer);

// SAFETY: the index only publishes addresses. Live slots or the owning heap
// keep each descriptor alive. The heap lock serializes page mutation.
unsafe impl Send for IndexedPage {}

// Mimalloc's arena metadata is indexed by slice. Dedicated mappings have no
// common arena, so route them by slice address through independent shards.
type DedicatedIndex = HashMap<usize, IndexedPage, foldhash::fast::FixedState>;

fn dedicated_index(payload: NonNull<u8>) -> &'static Mutex<Option<DedicatedIndex>> {
    static SHARDS: [Mutex<Option<DedicatedIndex>>; 64] = [const { Mutex::new(None) }; 64];
    let slice = payload.addr().get() / SLAB;
    &SHARDS[(slice ^ (slice >> 6) ^ (slice >> 12)) % SHARDS.len()]
}

#[cold]
fn dedicated_page(payload: NonNull<u8>) -> PagePointer {
    dedicated_index(payload)
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|index| index.get(&payload.addr().get()))
        .expect("no entry found for key")
        .0
}

pub(crate) struct Page {
    heap: Arc<Heap>,
    backing: Backing,
    payer: std::cell::UnsafeCell<Payer>,
    key: std::cell::UnsafeCell<Key>,
    slots: std::cell::UnsafeCell<Slots>,
}

enum Backing {
    Slab { _allocation: slabs::Slab },
    Dedicated(Mapping),
}

impl Backing {
    fn dedicated(&self) -> &Mapping {
        match self {
            Self::Dedicated(mapping) => mapping,
            Self::Slab { .. } => unreachable!("dedicated page has a slab backing"),
        }
    }
}

struct Slots {
    first: NonNull<u8>,
    limit: usize,
    bump: usize,
    live: usize,
    free: *mut u8,
    #[cfg(feature = "memcheck")]
    checked_bytes: usize,
    previous: Option<PagePointer>,
    next: Option<PagePointer>,
}

impl Page {
    // SAFETY for all page operations: the mapping and a live block or the
    // heap retain the page. Slot and slot-link changes own the heap mutex;
    // payer and key are installed on empty pages. A one-slot page can
    // also change class while its sole owner grows the live slot.
    unsafe fn key(page: PagePointer) -> Key {
        unsafe { *page.as_ref().key.get() }
    }

    /// # Safety
    /// A live slot pins the page and its original payer across the borrow.
    #[inline(always)]
    pub(crate) unsafe fn funding<'a>(page: PagePointer) -> &'a Payer {
        // SAFETY: occupied slots keep the payer immutable until their release.
        unsafe { &*page.as_ref().payer.get() }
    }

    /// # Safety
    /// A live slot pins this page's metadata.
    #[inline]
    pub(crate) unsafe fn kind(page: PagePointer) -> u8 {
        // SAFETY: a live slot keeps its native kind.
        unsafe { Self::key(page).class.kind }
    }

    fn new(heap: &Arc<Heap>, payer: Payer, key: Key) -> PagePointer {
        let class = key.class;
        let slab = class.fits_slab();
        let (backing, start) = if slab {
            let (allocation, start) = slabs::Slab::allocate(Arc::as_ptr(heap).addr());
            (
                Backing::Slab {
                    _allocation: allocation,
                },
                start,
            )
        } else {
            let headroom = if own_prefix(class.kind) == 0 {
                0
            } else {
                page_bytes()
            };
            let (mapping, start) = Mapping::aligned(
                class.stride,
                class.align.max(SLAB),
                headroom,
                class.growable,
            );
            (Backing::Dedicated(mapping), start)
        };
        let data = Self {
            heap: Arc::clone(heap),
            backing,
            payer: payer.into(),
            key: key.into(),
            slots: Self::slots(start, class).into(),
        };
        if slab {
            let page: PagePointer = start.cast();
            // SAFETY: the start of a new slab has room for its aligned descriptor.
            unsafe { page.as_ptr().write(data) };
            page
        } else {
            let page = NonNull::from(Box::leak(Box::new(data)));
            dedicated_index(start)
                .lock()
                .unwrap()
                .get_or_insert_with(|| {
                    HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0))
                })
                .insert(start.addr().get(), IndexedPage(page));
            page
        }
    }

    fn slots(start: NonNull<u8>, class: Class) -> Slots {
        let (first, limit) = if class.fits_slab() {
            let offset = (size_of::<Self>() + class.offset()).next_multiple_of(class.align);
            // SAFETY: size class guarantees at least one slot in the mapping.
            (
                unsafe { start.add(offset) },
                (SLAB - offset + class.offset()) / class.stride,
            )
        } else {
            (start, 1)
        };
        Slots {
            first,
            limit,
            bump: 0,
            live: 0,
            free: ptr::null_mut(),
            #[cfg(feature = "memcheck")]
            checked_bytes: 0,
            previous: None,
            next: None,
        }
    }

    unsafe fn reuse(page: PagePointer, payer: Payer, key: Key) {
        // SAFETY: the caller holds the heap mutex and verified the empty page fits.
        unsafe {
            page.as_ref().payer.get().write(payer);
            page.as_ref().key.get().write(key);
            let slots = page.as_ref().slots.get();
            let start = if key.class.fits_slab() {
                page.cast()
            } else {
                (*slots).first
            };
            slots.write(Self::slots(start, key.class));
        }
    }

    unsafe fn destroy(page: PagePointer) {
        // SAFETY: no live slots or queues reference this page. Move its fields
        // out before dropping the owner and unmapping the payload.
        let page = unsafe {
            let first = (*page.as_ref().slots.get()).first;
            if first.addr().get() & (SLAB - 1) == 0 {
                dedicated_index(first)
                    .lock()
                    .unwrap()
                    .as_mut()
                    .and_then(|index| index.remove(&first.addr().get()));
                *Box::from_raw(page.as_ptr())
            } else {
                page.as_ptr().read()
            }
        };
        drop(page.backing);
        drop(page.heap);
    }
}

/// The owning task allocates and frees here; a transferred block's last
/// owner frees from another worker. That rare contention spins on a plain
/// flag, as mimalloc's and snmalloc's internal locks do.
#[derive(Default)]
pub(crate) struct Heap {
    locked: AtomicBool,
    state: std::cell::UnsafeCell<State>,
}

// SAFETY: State is reached only through Locked, which holds the flag.
unsafe impl Sync for Heap {}

struct Locked<'a>(&'a Heap);

impl std::ops::Deref for Locked<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        // SAFETY: this guard holds the heap's flag.
        unsafe { &*self.0.state.get() }
    }
}
impl std::ops::DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut State {
        // SAFETY: this guard holds the heap's flag.
        unsafe { &mut *self.0.state.get() }
    }
}
impl Drop for Locked<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

struct State {
    closed: bool,
    ready: [[Option<PagePointer>; SLAB_BINS]; 3],
    empty_slabs: Vec<PagePointer>,
    dedicated: DedicatedCache,
}

#[derive(Default)]
struct DedicatedCache {
    clean: VecDeque<PagePointer>,
    retained: VecDeque<RetainedPage>,
    retained_bytes: usize,
    live_bytes: usize,
    peak_live_bytes: usize,
}

struct RetainedPage {
    page: PagePointer,
    freed_at: Instant,
}

impl Default for State {
    fn default() -> Self {
        Self {
            closed: false,
            ready: [[None; SLAB_BINS]; 3],
            empty_slabs: Vec::new(),
            dedicated: DedicatedCache::default(),
        }
    }
}

// SAFETY: unique live blocks pin their pages and read only immutable funding.
// Queue membership, free lists and page reuse are serialized by the task heap.
unsafe impl Send for State {}

impl State {
    fn ready_head(&mut self, class: Class) -> &mut Option<PagePointer> {
        &mut self.ready[usize::from(class.kind)][usize::from(class.bin)]
    }

    /// The head's empty page, if the queue keeps one.
    fn spare(&mut self, class: Class) -> Option<PagePointer> {
        // SAFETY: this heap owns the ready list and its pages' slots.
        self.ready_head(class)
            .filter(|&page| unsafe { (*page.as_ref().slots.get()).live == 0 })
    }

    fn available(&mut self, payer: Payer, key: Key) -> Option<PagePointer> {
        let head = *self.ready_head(key.class);
        let spare = self.spare(key.class);
        // Prefer partial pages, so the empty head is the last resort.
        let mut current = if spare.is_some() {
            // SAFETY: this heap owns the ready list.
            unsafe { (*head?.as_ref().slots.get()).next }
        } else {
            head
        };
        while let Some(page) = current {
            // SAFETY: this heap owns the ready list. A linked page with a live
            // slot keeps its payer and alignment fixed.
            unsafe {
                let existing = Page::key(page);
                if existing.payer == key.payer && existing.class.align == key.class.align {
                    return Some(page);
                }
                current = (*page.as_ref().slots.get()).next;
            }
        }
        let page = spare?;
        // SAFETY: the empty head has no live slot, so this heap may rebind it.
        // The same bin and alignment keep its stride and free slots valid.
        unsafe {
            let existing = Page::key(page);
            if existing.class.align != key.class.align {
                let slots = &*page.as_ref().slots.get();
                let links = (slots.previous, slots.next);
                Page::reuse(page, payer, key);
                let slots = &mut *page.as_ref().slots.get();
                (slots.previous, slots.next) = links;
            } else if existing.payer != key.payer {
                page.as_ref().payer.get().write(payer);
                page.as_ref().key.get().write(key);
            }
        }
        Some(page)
    }

    unsafe fn link(&mut self, page: PagePointer, class: Class) {
        // SAFETY: exclusively held heap; this page was absent from the class
        // queue. A partial page goes after an empty head, which stays first.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let after = self.spare(class).filter(|_| slots.live != 0);
            slots.previous = after;
            slots.next = match after {
                Some(head) => (*head.as_ref().slots.get()).next.replace(page),
                None => self.ready_head(class).replace(page),
            };
            if let Some(next) = slots.next {
                (*next.as_ref().slots.get()).previous = Some(page);
            }
        }
    }

    unsafe fn unlink(&mut self, page: PagePointer, class: Class) {
        // SAFETY: exclusively held heap; this page belongs to the class queue.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let previous = slots.previous.take();
            let next = slots.next.take();
            if let Some(previous) = previous {
                (*previous.as_ref().slots.get()).next = next;
            } else {
                *self.ready_head(class) = next;
            }
            if let Some(next) = next {
                (*next.as_ref().slots.get()).previous = previous;
            }
        }
    }

    fn cached(&mut self, class: Class) -> Option<PagePointer> {
        if class.fits_slab() {
            return self.empty_slabs.pop();
        }
        self.dedicated.take(class)
    }
}

impl DedicatedCache {
    fn add_live(&mut self, bytes: usize) {
        self.live_bytes += bytes;
        self.peak_live_bytes = self.peak_live_bytes.max(self.live_bytes);
    }

    fn take(&mut self, class: Class) -> Option<PagePointer> {
        if !self.retained.is_empty() {
            self.purge_expired(Instant::now());
        }
        // Prefer the smallest fitting reservation, and the resident payload
        // when two reservations are the same size.
        let (index, _) = self
            .retained
            .iter()
            .map(|entry| entry.page)
            .chain(self.clean.iter().copied())
            .enumerate()
            .filter_map(|(index, page)| {
                // SAFETY: the heap owns these empty pages and their mappings.
                let page = unsafe { page.as_ref() };
                let payload = unsafe { (*page.slots.get()).first };
                let backing = page.backing.dedicated();
                let extent = backing.extent(payload);
                (extent >= class.stride
                    && payload.addr().get() & (class.align - 1) == 0
                    && backing.headroom(payload) >= own_prefix(class.kind))
                .then_some((index, extent))
            })
            .min_by_key(|&(_, extent)| extent)?;
        let page = if index < self.retained.len() {
            self.remove_retained(index)
        } else {
            self.clean
                .swap_remove_back(index - self.retained.len())
                .unwrap()
        };
        // SAFETY: the page has been taken off the cache and has no live slots.
        unsafe {
            let header = page.as_ref();
            let payload = (*header.slots.get()).first;
            if header.backing.dedicated().extend(payload, class.stride) {
                Some(page)
            } else {
                Page::destroy(page);
                None
            }
        }
    }

    fn retain(&mut self, page: PagePointer) {
        // SAFETY: this heap owns the empty page and its mapping.
        let bytes = unsafe { page.as_ref().backing.dedicated().writable.get() };
        self.live_bytes -= bytes;
        let now = Instant::now();
        self.purge_expired(now);

        // Cap the free writable payload at the peak live dedicated footprint,
        // with an absolute ceiling for heaps that briefly held large buffers.
        let cap = self.peak_live_bytes.min(MAX_RETAINED_DEDICATED_BYTES);
        if bytes > cap {
            self.purge(page);
            return;
        }
        while self.retained_bytes > cap - bytes {
            let oldest = self.remove_retained(0);
            self.purge(oldest);
        }
        self.retained_bytes += bytes;
        self.retained.push_back(RetainedPage {
            page,
            freed_at: now,
        });
    }

    fn purge_expired(&mut self, now: Instant) {
        // Check on dedicated allocation and free. An inactive heap holds at
        // most its byte cap and releases its mappings on close.
        while self
            .retained
            .front()
            .is_some_and(|entry| now.duration_since(entry.freed_at) >= DEDICATED_PURGE_DELAY)
        {
            let oldest = self.remove_retained(0);
            self.purge(oldest);
        }
    }

    fn remove_retained(&mut self, index: usize) -> PagePointer {
        let page = self.retained.remove(index).unwrap().page;
        // SAFETY: this heap exclusively owns the removed empty page.
        self.retained_bytes -= unsafe { page.as_ref().backing.dedicated().writable.get() };
        page
    }

    fn purge(&mut self, page: PagePointer) {
        // SAFETY: the heap owns this empty page and its separate descriptor.
        let header = unsafe { page.as_ref() };
        let payload = unsafe { (*header.slots.get()).first };
        if !header.backing.dedicated().discard_writable(payload) {
            // SAFETY: the page is empty and detached from both queues.
            unsafe { Page::destroy(page) };
            return;
        }
        if self.clean.len() == EMPTY_DEDICATED_LIMIT {
            // SAFETY: this removes the cache's only reference to the empty page.
            unsafe { Page::destroy(self.clean.pop_front().unwrap()) };
        }
        self.clean.push_back(page);
    }
}

impl Heap {
    #[inline(always)]
    fn try_lock(&self) -> Option<Locked<'_>> {
        self.locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Locked(self))
    }

    #[inline(always)]
    fn lock(&self) -> Locked<'_> {
        self.try_lock().unwrap_or_else(|| {
            self.contend();
            Locked(self)
        })
    }

    #[cold]
    #[inline(never)]
    fn contend(&self) {
        let mut spins = 0_u32;
        loop {
            while self.locked.load(Ordering::Relaxed) {
                if spins < 64 {
                    spins += 1;
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    #[inline]
    pub(crate) fn allocate(
        heap: &Arc<Self>,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
        growable: bool,
    ) -> NonNull<u8> {
        // Each miss is a tail call, so the hit saves no registers.
        let Some(class) = Class::direct(bytes, align, kind, growable) else {
            return Self::allocate_locking(heap, payer, bytes, align, kind, growable);
        };
        let Some(mut state) = heap.try_lock() else {
            return Self::allocate_locking(heap, payer, bytes, align, kind, growable);
        };
        // SAFETY: this heap owns its queues. The head page of a matching payer
        // and alignment has a free slot and stays partial after this one.
        // Direct classes are below a native page, so they always fit a slab.
        unsafe {
            if let Some(page) = *state.ready_head(class)
                && (*page.as_ref().key.get()).payer == payer.record_pointer().addr()
                && (*page.as_ref().key.get()).class.align == class.align
                && (*page.as_ref().slots.get()).live + 1 < (*page.as_ref().slots.get()).limit
            {
                return Self::take(page, class, bytes);
            }
            Self::allocate_slow(heap, state, payer, bytes, align, kind, growable)
        }
    }

    #[cold]
    #[inline(never)]
    fn allocate_locking(
        heap: &Arc<Self>,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
        growable: bool,
    ) -> NonNull<u8> {
        let state = heap.lock();
        // SAFETY: the lock is held.
        unsafe { Self::allocate_slow(heap, state, payer, bytes, align, kind, growable) }
    }

    /// # Safety
    /// The heap lock is held and `page` has a free slot of `class`.
    #[inline(always)]
    unsafe fn take(page: PagePointer, class: Class, bytes: usize) -> NonNull<u8> {
        // SAFETY: the caller owns the page's slots under the heap lock.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let pointer = if let Some(free) = NonNull::new(slots.free) {
                #[cfg(feature = "memcheck")]
                allocation_check::link_access(free, true);
                slots.free = free.cast::<*mut u8>().read();
                free
            } else {
                let pointer = slots.first.add(slots.bump * class.stride);
                slots.bump += 1;
                pointer
            };
            slots.live += 1;
            #[cfg(feature = "memcheck")]
            {
                slots.checked_bytes = bytes;
                allocation_check::allocated(pointer, bytes, class.kind);
            }
            #[cfg(not(feature = "memcheck"))]
            let _ = bytes;
            #[cfg(any(test, feature = "charge-observation"))]
            crate::peak::allocated(pointer.as_ptr(), class.stride);
            pointer
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn allocate_slow(
        heap: &Arc<Self>,
        mut state: Locked<'_>,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
        growable: bool,
    ) -> NonNull<u8> {
        let class = Class::for_block(bytes, align, kind, growable);
        let key = Key {
            payer: payer.record_pointer().addr(),
            class,
        };
        // SAFETY: this heap owns all queues and the live slot until publication.
        unsafe {
            let slab = class.fits_slab();
            let ready = if slab {
                state.available(payer, key)
            } else {
                None
            };
            let page = if let Some(page) = ready {
                page
            } else if let Some(page) = state.cached(class) {
                Page::reuse(page, payer, key);
                page
            } else {
                Page::new(heap, payer, key)
            };
            if !slab {
                state
                    .dedicated
                    .add_live(page.as_ref().backing.dedicated().writable.get());
            }
            let pointer = Self::take(page, class, bytes);
            let slots = &*page.as_ref().slots.get();
            if slots.live == slots.limit {
                if ready.is_some() {
                    state.unlink(page, class);
                }
            } else if ready.is_none() {
                state.link(page, class);
            }
            pointer
        }
    }

    /// Retire the task's cache; pages with transferred blocks keep their heap
    /// alive and are released by the last free on whichever task owns that block.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        let spares = std::mem::replace(&mut state.ready, [[None; SLAB_BINS]; 3])
            .into_iter()
            .flatten()
            .flatten()
            // SAFETY: the heap lock owns the queued pages' slots.
            .filter(|&page| unsafe { (*page.as_ref().slots.get()).live == 0 })
            .collect::<Vec<_>>();
        let slabs = std::mem::take(&mut state.empty_slabs);
        let dedicated = std::mem::take(&mut state.dedicated);
        drop(state);
        for page in spares.into_iter().chain(slabs) {
            // SAFETY: detached from the cache, with no live slots.
            unsafe { Page::destroy(page) };
        }
        for page in dedicated
            .clean
            .into_iter()
            .chain(dedicated.retained.into_iter().map(|entry| entry.page))
        {
            // SAFETY: detached from the cache, with no live slots.
            unsafe { Page::destroy(page) };
        }
    }
}

/// # Safety
/// A live own or resource block allocated by this heap.
#[inline]
pub(crate) unsafe fn page_of(payload: NonNull<u8>) -> PagePointer {
    let address = payload.as_ptr().addr();
    let base = address & !(SLAB - 1);
    if base == address {
        return dedicated_page(payload);
    }
    // SAFETY: slab slots share their mapping with its aligned descriptor.
    unsafe { NonNull::new_unchecked(payload.as_ptr().with_addr(base).cast()) }
}

/// # Safety
/// A live block pins its page's original payer until its logical refund.
pub(crate) unsafe fn payer<'a>(payload: NonNull<u8>) -> &'a Payer {
    // SAFETY: the page's payer remains immutable throughout this block's lifetime.
    unsafe { Page::funding(page_of(payload)) }
}

/// # Safety
/// A live block whose kind was assigned by the allocator.
pub(crate) unsafe fn kind(payload: NonNull<u8>) -> u8 {
    // SAFETY: exclusively owned slots pin the page metadata.
    unsafe { Page::kind(page_of(payload)) }
}

/// # Safety
/// The caller owns the live buffer exclusively and has already admitted the
/// full new charge. The layout describes the same element type and kind.
pub(crate) unsafe fn try_grow_in_place(
    heap: &Arc<Heap>,
    payload: NonNull<u8>,
    payer: Payer,
    bytes: usize,
    align: usize,
    kind: u8,
) -> bool {
    // Dedicated one-slot pages have slab-aligned payloads. A live page
    // stays off the heap's lists, so its sole owner can extend it directly.
    if payload.addr().get() & (SLAB - 1) != 0 {
        return false;
    }
    // SAFETY: the unique live slot pins the header, mapping and payer.
    unsafe {
        let page = page_of(payload);
        let header = page.as_ref();
        if !Arc::ptr_eq(heap, &header.heap) || *header.payer.get() != payer {
            return false;
        }
        let key = Page::key(page);
        let class = Class::for_block(bytes, align, kind, true);
        let backing = header.backing.dedicated();
        let before = backing.writable.get();
        if !backing.extend(payload, class.stride) {
            return false;
        }
        let added = backing.writable.get() - before;
        if added != 0 {
            heap.lock().dedicated.add_live(added);
        }
        header.key.get().write(Key { class, ..key });
        #[cfg(feature = "memcheck")]
        {
            let slots = &mut *header.slots.get();
            allocation_check::resized(payload, slots.checked_bytes, bytes, kind);
            slots.checked_bytes = bytes;
        }
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::resized(payload.as_ptr(), class.stride);
        true
    }
}

/// # Safety
/// The caller transfers the unique, cleaned slot after its logical refund.
#[inline]
pub(crate) unsafe fn release(payload: NonNull<u8>) {
    // SAFETY: the slot pins its header and the page's heap while live.
    unsafe { release_page(page_of(payload), payload) };
}

/// # Safety
/// The caller transfers the unique cleaned slot after its logical refund, and
/// `page` is the owning page returned by `page_of(payload)` for that slot.
#[inline]
pub(crate) unsafe fn release_page(page: PagePointer, payload: NonNull<u8>) {
    // SAFETY: slot pins this immutable heap handle through the lock and state update.
    let heap = unsafe { &page.as_ref().heap };
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::deallocated(payload.as_ptr());
    let state = heap.lock();
    // SAFETY: the heap lock owns the free list and page lifecycle. This slot is
    // writable and never shared after its caller publishes the refund.
    unsafe {
        #[cfg(feature = "memcheck")]
        allocation_check::freed(payload, (*page.as_ref().key.get()).class.kind);
        let slots = &mut *page.as_ref().slots.get();
        // A partial page keeps its queue position, and an emptied head stays
        // as the spare. A closed heap destroys the page at its last slot.
        if slots.live != slots.limit
            && (slots.live > 1 || (slots.previous.is_none() && !state.closed))
        {
            slots.live -= 1;
            push_free(slots, payload);
        } else {
            release_slow(state, page, payload);
        }
    }
}

/// # Safety
/// The heap lock owns `slots`; `payload` is one of its slots, no longer live.
#[inline(always)]
unsafe fn push_free(slots: &mut Slots, payload: NonNull<u8>) {
    // SAFETY: the released slot holds at least one pointer-sized word.
    unsafe {
        #[cfg(feature = "memcheck")]
        allocation_check::link_access(payload, true);
        payload.cast::<*mut u8>().write(slots.free);
        #[cfg(feature = "memcheck")]
        allocation_check::link_access(payload, false);
    }
    slots.free = payload.as_ptr();
}

/// The release that fills a full page's first hole, empties a page or
/// meets a closed heap.
/// # Safety
/// As [`release`], with the page's heap lock held as `state`.
#[cold]
#[inline(never)]
unsafe fn release_slow(mut state: Locked<'_>, page: PagePointer, payload: NonNull<u8>) {
    // SAFETY: the heap lock owns the free list and page lifecycle.
    unsafe {
        let class = Page::key(page).class;
        let slots = &mut *page.as_ref().slots.get();
        let was_full = slots.live == slots.limit;
        slots.live -= 1;
        if state.closed {
            if slots.live == 0 {
                drop(state);
                Page::destroy(page);
            }
            return;
        }
        if payload.addr().get() & (SLAB - 1) == 0 {
            state.dedicated.retain(page);
            return;
        }
        push_free(slots, payload);
        if slots.live != 0 {
            if was_full {
                state.link(page, class);
            }
        } else if !was_full && slots.previous.is_none() {
            // Already the queue head: it stays as the spare.
        } else if state.spare(class).is_none() {
            if !was_full {
                state.unlink(page, class);
            }
            state.link(page, class);
        } else {
            if !was_full {
                state.unlink(page, class);
            }
            state.empty_slabs.push(page);
        }
    }
}

struct Mapping {
    base: NonNull<u8>,
    len: usize,
    // Dedicated payload bytes made writable. A live slot's sole owner or the
    // empty cache under the heap mutex can update this extent.
    writable: Cell<usize>,
}
impl Mapping {
    const WRITABLE: libc::c_int = libc::PROT_READ | libc::PROT_WRITE;

    fn aligned(bytes: usize, align: usize, headroom: usize, reserve: bool) -> (Self, NonNull<u8>) {
        let bytes = bytes
            .checked_next_multiple_of(page_bytes())
            .unwrap_or_else(host_memory);
        if reserve
            && let Some(space) = bytes.checked_mul(GROWTH_RESERVE_FACTOR)
            && let Some(mapping) =
                Self::try_aligned(bytes, space.max(MIN_GROWTH_RESERVATION), align, headroom)
        {
            return mapping;
        }
        Self::try_aligned(bytes, bytes, align, headroom).unwrap_or_else(host_memory)
    }

    fn try_aligned(
        bytes: usize,
        space: usize,
        align: usize,
        headroom: usize,
    ) -> Option<(Self, NonNull<u8>)> {
        let len = space.checked_add(headroom)?;
        let extra = if align > page_bytes() { align } else { 0 };
        let total = len.checked_add(extra)?;
        if total > isize::MAX as usize {
            return None;
        }
        #[cfg(test)]
        MAP_REQUESTS.with(|count| count.set(count.get() + 1));
        let protection = if space == bytes {
            Self::WRITABLE
        } else {
            libc::PROT_NONE
        };
        // SAFETY: reserve an anonymous private mapping without replacing any address.
        let raw = unsafe {
            libc::mmap(
                ptr::null_mut(),
                total,
                protection,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: a successful NULL-hint mapping is non-null. Trim to align the
        // payload, then make the initial extent and ownership prefix writable.
        unsafe {
            let raw = NonNull::new_unchecked(raw.cast::<u8>());
            let payload = raw.add(headroom).map_addr(|address| {
                std::num::NonZeroUsize::new(address.get().next_multiple_of(align)).unwrap()
            });
            let base = payload.sub(headroom);
            let left = base.as_ptr().addr() - raw.as_ptr().addr();
            let right = total - left - len;
            if left != 0 {
                libc::munmap(raw.as_ptr().cast(), left);
            }
            if right != 0 {
                libc::munmap(base.add(len).as_ptr().cast(), right);
            }
            let mapping = Self {
                base,
                len,
                writable: Cell::new(bytes),
            };
            if space != bytes
                && libc::mprotect(base.as_ptr().cast(), headroom + bytes, Self::WRITABLE) != 0
            {
                return None;
            }
            Some((mapping, payload))
        }
    }

    fn headroom(&self, payload: NonNull<u8>) -> usize {
        payload.addr().get() - self.base.addr().get()
    }

    fn extent(&self, payload: NonNull<u8>) -> usize {
        self.len - self.headroom(payload)
    }

    fn extend(&self, payload: NonNull<u8>, bytes: usize) -> bool {
        let Some(end) = bytes.checked_next_multiple_of(page_bytes()) else {
            return false;
        };
        let start = self.writable.get();
        if end <= start {
            return true;
        }
        if end > self.extent(payload) {
            return false;
        }
        // SAFETY: these whole pages are in this reserved mapping, beyond its
        // writable extent. Only the live slot owner or empty cache can extend.
        if unsafe {
            libc::mprotect(
                payload.add(start).as_ptr().cast(),
                end - start,
                Self::WRITABLE,
            )
        } != 0
        {
            return false;
        }
        self.writable.set(end);
        true
    }

    fn discard_writable(&self, payload: NonNull<u8>) -> bool {
        let bytes = self.headroom(payload) + self.writable.get();
        // SAFETY: the empty dedicated mapping owns this whole writable range.
        unsafe { Mapping::discard(self.base, bytes) }
    }

    /// # Safety
    /// The caller exclusively owns these whole anonymous pages, with no live data.
    unsafe fn discard(base: NonNull<u8>, bytes: usize) -> bool {
        // SAFETY: the caller owns the anonymous range, with no live data.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::madvise(base.as_ptr().cast(), bytes, libc::MADV_DONTNEED) == 0
        }
        #[cfg(not(target_os = "linux"))]
        unsafe {
            libc::mmap(
                base.as_ptr().cast(),
                bytes,
                Self::WRITABLE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            ) != libc::MAP_FAILED
        }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this object owns the remaining complete mapping.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }
}

fn page_bytes() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf reads the kernel's immutable page size.
        let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        match usize::try_from(value) {
            Ok(bytes) if bytes.is_power_of_two() && (MIN_PAGE_BYTES..=SLAB).contains(&bytes) => {
                bytes
            }
            _ => crate::process::stop(b"PageSize"),
        }
    })
}

#[cold]
fn host_memory<T>() -> T {
    crate::process::stop(b"HostMemory")
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::{Context, block};

    fn resident_pages(base: NonNull<u8>, len: usize) -> usize {
        let mut flags = vec![0; len / page_bytes()];
        // SAFETY: the test holds a live or cached mapping across this range.
        assert_eq!(
            unsafe { libc::mincore(base.as_ptr().cast(), len, flags.as_mut_ptr()) },
            0
        );
        flags.into_iter().filter(|flag| flag & 1 != 0).count()
    }

    fn resident(payload: NonNull<u8>) -> bool {
        resident_pages(payload, page_bytes()) != 0
    }

    fn mapping(payload: NonNull<u8>) -> (NonNull<u8>, usize) {
        // SAFETY: the owned buffer pins its backing until free, and the cache
        // retains the reservation during the observations below.
        let backing = unsafe { page_of(payload).as_ref().backing.dedicated() };
        (backing.base, backing.len)
    }

    #[test]
    fn dedicated_payload_retention_is_bounded_and_expires() {
        const SMALL: usize = 8 * 1024;
        const LARGE: usize = 1024 * 1024;
        const OVERSIZED: usize = 8 * LARGE;
        let context = Context::new(OVERSIZED as u64);
        let payer = context.payer();
        let heap = context.heap.get_or_init(Arc::default);
        let allocate = |bytes| {
            block::admit_storage(payer, Some(bytes as u64)).unwrap();
            let pointer = Heap::allocate(heap, payer, bytes, 8, block::PLAIN, true);
            // SAFETY: this test owns the new buffer and initializes its first page.
            unsafe { pointer.as_ptr().write_bytes(0x5a, 4096) };
            pointer
        };
        let free = |pointer, bytes| {
            // SAFETY: the test transfers ownership with its full admitted charge.
            unsafe { block::free(pointer, bytes as u64) };
            assert_eq!(context.available(), OVERSIZED as u64);
        };

        let small = allocate(SMALL);
        free(small, SMALL);
        assert!(resident(small));
        block::admit_storage(payer, Some(SMALL as u64)).unwrap();
        let reused = Heap::allocate(heap, payer, SMALL, 8, block::PLAIN, true);
        assert_eq!(reused, small);
        assert!(resident(reused));
        free(reused, SMALL);

        // 1 MiB needs a separate reservation. Its release would leave 1 MiB
        // plus 8 KiB empty against a 1 MiB peak, so the older page gets purged.
        let large = allocate(LARGE);
        let (base, len) = mapping(large);
        free(large, LARGE);
        assert!(!resident(small));
        assert!(resident(large));
        {
            let mut state = heap.lock();
            assert_eq!(state.dedicated.retained_bytes, LARGE);
            let freed_at = state.dedicated.retained.front().unwrap().freed_at;
            state
                .dedicated
                .purge_expired(freed_at + Duration::from_millis(99));
            assert!(resident(large));
            state
                .dedicated
                .purge_expired(freed_at + Duration::from_millis(100));
            assert_eq!(state.dedicated.retained_bytes, 0);
            assert!(!resident(large));
            let resident = resident_pages(base, len);
            assert_eq!(
                resident, 0,
                "purged plain mapping retained {resident} native pages"
            );
        }

        // An 8 MiB live peak still permits only 4 MiB of retained payload.
        let oversized = allocate(OVERSIZED);
        free(oversized, OVERSIZED);
        assert_eq!(heap.lock().dedicated.retained_bytes, 0);
        assert!(!resident(oversized));

        // A GRAPH buffer's cleanup frame uses the preceding native page.
        // Pressure must discard that page too, while preserving reuse.
        block::admit_storage(payer, Some(OVERSIZED as u64)).unwrap();
        let graph = Heap::allocate(heap, payer, OVERSIZED, 8, block::GRAPH, true);
        let (base, len) = mapping(graph);
        // SAFETY: the exclusive buffer owns its reserved cleanup frame.
        unsafe { graph.as_ptr().sub(own_prefix(block::GRAPH)).write(0x5a) };
        assert!(resident(base));
        free(graph, OVERSIZED);
        let resident = resident_pages(base, len);
        assert_eq!(
            resident, 0,
            "purged graph mapping retained {resident} native pages"
        );
        block::admit_storage(payer, Some(OVERSIZED as u64)).unwrap();
        let graph_again = Heap::allocate(heap, payer, OVERSIZED, 8, block::GRAPH, true);
        assert_eq!(graph_again, graph);
        free(graph_again, OVERSIZED);
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static MAP_REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "memcheck")]
mod allocation_check {
    use super::NonNull;
    use crate::abi_layout::own_prefix;
    use hirsel_stack::valgrind_request;

    fn start(pointer: NonNull<u8>, kind: u8) -> usize {
        pointer.addr().get() - own_prefix(kind)
    }

    pub(super) fn allocated(pointer: NonNull<u8>, bytes: usize, kind: u8) {
        const MALLOCLIKE_BLOCK: u64 = 0x1301;
        valgrind_request(
            0,
            MALLOCLIKE_BLOCK,
            [start(pointer, kind), bytes.max(1) + own_prefix(kind), 0, 0],
        );
    }

    pub(super) fn resized(pointer: NonNull<u8>, old: usize, new: usize, kind: u8) {
        const RESIZEINPLACE_BLOCK: u64 = 0x130b;
        let prefix = own_prefix(kind);
        valgrind_request(
            0,
            RESIZEINPLACE_BLOCK,
            [
                start(pointer, kind),
                old.max(1) + prefix,
                new.max(1) + prefix,
                0,
            ],
        );
    }

    pub(super) fn freed(pointer: NonNull<u8>, kind: u8) {
        const FREELIKE_BLOCK: u64 = 0x1302;
        valgrind_request(0, FREELIKE_BLOCK, [start(pointer, kind), 0, 0, 0]);
    }

    pub(super) fn link_access(pointer: NonNull<u8>, readable: bool) {
        const MAKE_MEM_NOACCESS: u64 = 0x4d43_0000;
        const MAKE_MEM_DEFINED: u64 = 0x4d43_0002;
        let request = if readable {
            MAKE_MEM_DEFINED
        } else {
            MAKE_MEM_NOACCESS
        };
        valgrind_request(0, request, [pointer.addr().get(), size_of::<usize>(), 0, 0]);
    }
}

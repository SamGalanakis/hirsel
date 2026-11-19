//! Fixed-slot pages with one payer and one owner per page. Like mimalloc's
//! page queues, a class keeps only pages that can supply a block. The task
//! owns the queue even if it resumes on a different worker. As mimalloc
//! retires a queue's last page lazily, a queue keeps at most one empty page,
//! at its head, with its free list intact: a block that is allocated and
//! freed alone reuses its slot without resetting the page.
//!
//! As mimalloc's `mi_page_extend_free`, a page threads fresh slots onto its
//! free list one native page at a time, so allocation pops only. A page whose
//! slots have reached its third native page is spread. When its task parks,
//! the worker returns the native pages that hold no live slot, as mimalloc's
//! page reset and jemalloc's dirty-page purge do, and moves the frontier back
//! to the highest live slot. Blocks of a class that has no page with a free
//! slot share one native page of a slab per payer and kind (a nursery), up to
//! a few per class in each nursery, so a payer's scattered small state does
//! not hold a slab per class, even in a heap that serves many payers.
//!
//! Emptied slabs and emptied dedicated pages follow one retention rule, as
//! mimalloc's `purge_delay` does for freed arena memory: they stay resident
//! for reuse, and their native pages go back to the host once a worker has
//! waited `RELEASE_DELAY_MS` with nothing to run (`purge`). A
//! heap's retained dedicated payload never takes its dedicated footprint
//! (live plus retained) above its peak live dedicated bytes, so a program
//! that repeats the same work reuses the same resident pages.
mod slabs;

use crate::{abi_layout::own_prefix, limit::Payer};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const SLAB: usize = 64 * 1024;
const MIN_PAGE_BYTES: usize = 4096;
const GROWTH_RESERVE_FACTOR: usize = 8;
const MIN_GROWTH_RESERVATION: usize = 256 * 1024;
// Bound clean reservations and their external descriptors.
const EMPTY_DEDICATED_LIMIT: usize = 8;
// Detached empty slabs a parked task keeps, each with its header resident.
const EMPTY_SLAB_LIMIT: usize = 8;
/// How long a worker waits with nothing to run before it releases retained pages.
pub(crate) const RELEASE_DELAY_MS: i32 = 100;

// Heaps whose dedicated cache holds retained pages, so an idle worker can
// release them. A heap is listed at most once (`DedicatedCache::listed`).
static RETAINING: Mutex<Vec<Arc<Heap>>> = Mutex::new(Vec::new());
static RETAINING_HEAPS: AtomicUsize = AtomicUsize::new(0);

/// The poll timeout for a worker about to wait `timeout` ms (-1: no limit),
/// and whether it should release retained pages if that wait finds nothing.
pub(crate) fn idle_wait(timeout: i32) -> (i32, bool) {
    let retaining = slabs::retaining() || RETAINING_HEAPS.load(Ordering::Relaxed) != 0;
    if !retaining || (0..=RELEASE_DELAY_MS).contains(&timeout) {
        (timeout, false)
    } else {
        (RELEASE_DELAY_MS, true)
    }
}

/// Return every retained slab's and dedicated page's native pages to the
/// host. A worker calls this after an `idle_wait` that found nothing, or
/// before it parks without a poller.
pub(crate) fn purge() {
    slabs::purge();
    if RETAINING_HEAPS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let heaps = std::mem::take(&mut *RETAINING.lock().unwrap());
    RETAINING_HEAPS.fetch_sub(heaps.len(), Ordering::Relaxed);
    // The free path takes the list lock under a heap lock, so the list lock
    // is released before any heap lock is taken.
    for heap in heaps {
        let mut state = heap.lock();
        state.dedicated.listed = false;
        while !state.dedicated.retained.is_empty() {
            let oldest = state.dedicated.remove_retained(0);
            state.dedicated.purge(oldest);
        }
    }
}
const SLAB_BINS: usize = size_bin(SLAB / 2) as usize + 1;
// Blocks of each class one nursery takes before the class gets a page.
const NURSERY_BLOCKS: u8 = 16;
const NURSERY_STRIDE: usize = 1024;
// The most native pages a slab spans.
const _: () = assert!(SLAB / MIN_PAGE_BYTES <= u16::BITS as usize);

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
const DIRECT: [(u16, u8); DIRECT_WORDS + 1] = {
    let mut table = [(0, 0); DIRECT_WORDS + 1];
    let mut words = 1;
    while words <= DIRECT_WORDS {
        let bytes = words * 8;
        let unit = if bytes <= 64 {
            64
        } else {
            bytes.next_power_of_two()
        } / 8;
        let stride = bytes.next_multiple_of(unit);
        table[words] = (stride as u16, size_bin(stride));
        words += 1;
    }
    table
};

// Direct classes are below the smallest native page and fit a slab.
const _: () = assert!(
    DIRECT[DIRECT_WORDS].0 as usize <= MIN_PAGE_BYTES
        && (DIRECT[DIRECT_WORDS].1 as usize) < SLAB_BINS
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
        let &(stride, bin) = DIRECT.get(words)?;
        Some(Self {
            stride: usize::from(stride),
            align: 8,
            kind,
            bin,
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
    // Survives class changes: it describes the mapping's resident pages.
    spread: std::cell::UnsafeCell<Spread>,
}

#[derive(Default)]
struct Spread {
    /// Bytes from the header that may be resident.
    touched: usize,
    /// `touched` reaches a third native page.
    spread: bool,
    /// On the heap's trim list.
    listed: bool,
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

/// A slab page's slots below `bump` are live, on the free list, or lost:
/// a lost slot overlaps a native page in `discarded`, which a trim returned
/// while no live slot overlapped it. Lost slots rejoin the free list once
/// the frontier reaches `limit`. A nursery has stride 0, keeps `limit` equal
/// to `live` so every release takes the slow path, and bumps by bytes.
/// A release that leaves `watch` or fewer live slots takes the slow path,
/// where a spread page is trimmed or listed for trimming.
struct Slots {
    first: NonNull<u8>,
    limit: usize,
    bump: usize,
    live: usize,
    free: *mut u8,
    discarded: u16,
    watch: usize,
    #[cfg(feature = "memcheck")]
    checked_bytes: usize,
    /// Whether this page is first in its class's current list.
    head: bool,
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
            spread: Spread::default().into(),
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

    /// A slab whose first native page bump-allocates blocks of any stride
    /// for one payer and kind.
    fn nursery(heap: &Arc<Heap>, payer: Payer, kind: u8) -> PagePointer {
        let (allocation, start) = slabs::Slab::allocate(Arc::as_ptr(heap).addr());
        let page: PagePointer = start.cast();
        let key = Key {
            payer: payer.record_pointer().addr(),
            class: Class {
                stride: 0,
                align: 8,
                kind,
                bin: 0,
                growable: false,
            },
        };
        // SAFETY: the start of a new slab has room for its aligned descriptor;
        // blocks start after it.
        unsafe {
            page.as_ptr().write(Self {
                heap: Arc::clone(heap),
                backing: Backing::Slab {
                    _allocation: allocation,
                },
                payer: payer.into(),
                key: key.into(),
                slots: Slots {
                    first: start.add(size_of::<Self>()),
                    limit: 0,
                    bump: 0,
                    live: 0,
                    free: ptr::null_mut(),
                    discarded: 0,
                    watch: 1,
                    #[cfg(feature = "memcheck")]
                    checked_bytes: 0,
                    head: false,
                    previous: None,
                    next: None,
                }
                .into(),
                spread: Spread::default().into(),
            });
        }
        page
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
            discarded: 0,
            watch: 1,
            #[cfg(feature = "memcheck")]
            checked_bytes: 0,
            head: false,
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
    /// `State::listed` is not empty, a nursery emptied, or detached empty
    /// slabs exceed `EMPTY_SLAB_LIMIT`.
    pending: AtomicBool,
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

/// The partial pages one payer can take for one kind, bin and alignment.
type PartialKey = (usize, u8, u8, usize);

fn partial_key(key: Key) -> PartialKey {
    (key.payer, key.class.kind, key.class.bin, key.class.align)
}

struct State {
    closed: bool,
    /// Each slab class's current list: pages of one payer and alignment, the
    /// key read off its first page. The first page may be empty, as the
    /// class's spare; every page after it is partial.
    heads: [[Option<PagePointer>; SLAB_BINS]; 3],
    /// Every other key's partial pages, so a payer's slow allocation finds
    /// its pages without visiting other payers'. Never holds an empty page or
    /// the current list's key.
    partial: HashMap<PartialKey, PagePointer, foldhash::fast::FixedState>,
    empty_slabs: VecDeque<PagePointer>,
    dedicated: DedicatedCache,
    /// Spread pages to trim when the task parks.
    listed: Vec<PagePointer>,
    /// The nursery that takes new blocks, by payer and kind.
    nurseries: HashMap<(usize, u8), Nursery, foldhash::fast::FixedState>,
    /// A nursery emptied or filled since the last trim.
    idle_nurseries: bool,
}

/// A payer's placements of one kind. Its budget outlives each page, so a
/// payer places as many blocks per class as a heap with one payer does.
struct Nursery {
    page: Option<PagePointer>,
    /// Placements left per class.
    left: [u8; SLAB_BINS],
}

#[derive(Default)]
struct DedicatedCache {
    clean: VecDeque<PagePointer>,
    /// Empty pages with resident payload, oldest first.
    retained: VecDeque<PagePointer>,
    retained_bytes: usize,
    /// Block strides of live pages: what the program asked for.
    demand: usize,
    peak_demand: usize,
    /// The heap is on `RETAINING`.
    listed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            closed: false,
            heads: [[None; SLAB_BINS]; 3],
            partial: HashMap::default(),
            empty_slabs: VecDeque::new(),
            dedicated: DedicatedCache::default(),
            listed: Vec::new(),
            nurseries: HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0)),
            idle_nurseries: false,
        }
    }
}

// SAFETY: unique live blocks pin their pages and read only immutable funding.
// Queue membership, free lists and page reuse are serialized by the task heap.
unsafe impl Send for State {}

impl State {
    fn head(&mut self, class: Class) -> &mut Option<PagePointer> {
        &mut self.heads[usize::from(class.kind)][usize::from(class.bin)]
    }

    /// The head's empty page, if the class keeps one.
    fn spare(&mut self, class: Class) -> Option<PagePointer> {
        // SAFETY: this heap owns the head and its page's slots.
        self.head(class)
            .filter(|&page| unsafe { (*page.as_ref().slots.get()).live == 0 })
    }

    fn available(&mut self, payer: Payer, key: Key) -> Option<PagePointer> {
        let wanted = partial_key(key);
        // SAFETY: this heap owns the lists. A linked page keeps its key while
        // it has a live slot, and the empty head keeps the key of the pages
        // behind it until it is rebound here.
        unsafe {
            let head = *self.head(key.class);
            if let Some(page) = head
                && partial_key(Page::key(page)) == wanted
            {
                // Prefer partial pages, so the empty head is the last resort.
                let slots = &*page.as_ref().slots.get();
                return Some(if slots.live == 0 {
                    slots.next.unwrap_or(page)
                } else {
                    page
                });
            }
            // Switch the class to the wanted key: file the current list's
            // partial pages under their key and take the wanted key's list.
            let spare = head.filter(|&page| (*page.as_ref().slots.get()).live == 0);
            let filed = match (head, spare) {
                (Some(page), Some(_)) => (*page.as_ref().slots.get()).next.take(),
                (head, None) => head,
                (None, Some(_)) => None,
            };
            if let Some(page) = filed {
                let slots = &mut *page.as_ref().slots.get();
                slots.head = false;
                slots.previous = None;
                let displaced = self.partial.insert(partial_key(Page::key(page)), page);
                debug_assert!(displaced.is_none(), "the current list's key is never filed");
            }
            let list = if self.partial.is_empty() {
                None
            } else {
                self.partial.remove(&wanted)
            };
            let Some(page) = spare else {
                *self.head(key.class) = list;
                if let Some(page) = list {
                    (*page.as_ref().slots.get()).head = true;
                }
                return list;
            };
            // The empty head stays first, rebound to the wanted key, with the
            // wanted partial pages after it. The same bin keeps its stride.
            if Page::key(page).class.align != key.class.align {
                Page::reuse(page, payer, key);
            } else {
                page.as_ref().payer.get().write(payer);
                page.as_ref().key.get().write(key);
            }
            let slots = &mut *page.as_ref().slots.get();
            slots.head = true;
            slots.previous = None;
            slots.next = list;
            if let Some(first) = list {
                (*first.as_ref().slots.get()).previous = Some(page);
            }
            // Prefer partial pages, so the empty head is the last resort.
            Some(list.unwrap_or(page))
        }
    }

    /// File the class's current list under its key, leaving the class with
    /// no current list.
    /// # Safety
    /// The heap is exclusively held, and the class has no spare.
    unsafe fn file_current(&mut self, class: Class) {
        // SAFETY: as the caller states; the current list's pages are partial
        // and its key is never filed while it is current.
        unsafe {
            if let Some(first) = self.head(class).take() {
                (*first.as_ref().slots.get()).head = false;
                let displaced = self.partial.insert(partial_key(Page::key(first)), first);
                debug_assert!(displaced.is_none(), "the current list's key is never filed");
            }
        }
    }

    /// Link a page that is in no list. A page of the current list's key, or
    /// any page while the class has none, goes first in the current list; a
    /// partial page of another key goes first in its own list. An empty page
    /// of another key never comes here: it becomes the spare after the
    /// current list is filed, or joins the empty cache.
    unsafe fn link(&mut self, page: PagePointer, class: Class) {
        // SAFETY: exclusively held heap; a linked page's key is fixed.
        unsafe {
            let key = partial_key(Page::key(page));
            let head = *self.head(class);
            let live = (*page.as_ref().slots.get()).live;
            let next = match head {
                None => {
                    // No current list: the key's filed pages, if any, follow.
                    if self.partial.is_empty() {
                        None
                    } else {
                        self.partial.remove(&key)
                    }
                }
                Some(first) if partial_key(Page::key(first)) == key => {
                    let first_slots = &mut *first.as_ref().slots.get();
                    if first_slots.live == 0 {
                        // The spare stays first; the partial page goes after it.
                        debug_assert!(live != 0, "a class keeps one spare");
                        let slots = &mut *page.as_ref().slots.get();
                        slots.head = false;
                        slots.previous = Some(first);
                        slots.next = first_slots.next.replace(page);
                        if let Some(next) = slots.next {
                            (*next.as_ref().slots.get()).previous = Some(page);
                        }
                        return;
                    }
                    first_slots.head = false;
                    Some(first)
                }
                Some(_) => {
                    debug_assert!(live != 0, "an empty page of another key is cached");
                    let next = self.partial.insert(key, page);
                    let slots = &mut *page.as_ref().slots.get();
                    slots.head = false;
                    slots.previous = None;
                    slots.next = next;
                    if let Some(next) = next {
                        (*next.as_ref().slots.get()).previous = Some(page);
                    }
                    return;
                }
            };
            let slots = &mut *page.as_ref().slots.get();
            slots.head = true;
            slots.previous = None;
            slots.next = next;
            if let Some(next) = next {
                (*next.as_ref().slots.get()).previous = Some(page);
            }
            *self.head(class) = Some(page);
        }
    }

    unsafe fn unlink(&mut self, page: PagePointer, class: Class) {
        // SAFETY: exclusively held heap; this page is in the current list or
        // in its key's partial list, whose key it keeps while linked.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let previous = slots.previous.take();
            let next = slots.next.take();
            if let Some(next) = next {
                (*next.as_ref().slots.get()).previous = previous;
            }
            if let Some(previous) = previous {
                (*previous.as_ref().slots.get()).next = next;
            } else if slots.head {
                slots.head = false;
                if let Some(next) = next {
                    (*next.as_ref().slots.get()).head = true;
                }
                *self.head(class) = next;
            } else {
                let key = partial_key(Page::key(page));
                match next {
                    Some(next) => {
                        self.partial.insert(key, next);
                    }
                    None => {
                        self.partial.remove(&key);
                    }
                }
            }
        }
    }

    /// An empty page for `class`, and the payload bytes reuse makes newly resident.
    fn cached(&mut self, class: Class) -> Option<(PagePointer, usize)> {
        if class.fits_slab() {
            return self.empty_slabs.pop_back().map(|page| (page, 0));
        }
        self.dedicated.take(class)
    }
}

impl DedicatedCache {
    /// Count a live block's change from stride `from` to `to`. When `fresh`
    /// bytes became resident, purge the oldest retained pages while live
    /// demand plus retained payload exceeds the peak demand. Reusing resident
    /// payload adds nothing, so a program that repeats its work keeps its
    /// pages.
    fn add_live(&mut self, from: usize, to: usize, fresh: usize) {
        self.demand = self.demand - from + to;
        self.peak_demand = self.peak_demand.max(self.demand);
        while fresh != 0
            && !self.retained.is_empty()
            && self.demand + self.retained_bytes > self.peak_demand
        {
            let oldest = self.remove_retained(0);
            self.purge(oldest);
        }
    }

    fn take(&mut self, class: Class) -> Option<(PagePointer, usize)> {
        // A retained page goes back only to a block of the stride it was first
        // allocated at, as an emptied slab serves only its own class: a program
        // that repeats its work gets the same pages back, and a page never
        // gathers the touched bytes of differently shaped blocks. Otherwise
        // take a clean page, which holds none of its payload.
        let fits = |page: &PagePointer, born: bool| {
            // SAFETY: the heap owns these empty pages and their mappings.
            let page = unsafe { page.as_ref() };
            let payload = unsafe { (*page.slots.get()).first };
            let backing = page.backing.dedicated();
            (!born || backing.born.get() == class.stride)
                && backing.extent(payload) >= class.stride
                && payload.addr().get() & (class.align - 1) == 0
                && backing.headroom(payload) >= own_prefix(class.kind)
        };
        let (page, resident) =
            if let Some(index) = self.retained.iter().rposition(|page| fits(page, true)) {
                let page = self.remove_retained(index);
                // SAFETY: this heap exclusively owns the removed empty page.
                (page, unsafe {
                    page.as_ref().backing.dedicated().writable.get()
                })
            } else {
                let index = self.clean.iter().position(|page| fits(page, false))?;
                (self.clean.swap_remove_back(index).unwrap(), 0)
            };
        // SAFETY: the page has been taken off the cache and has no live slots.
        unsafe {
            let header = page.as_ref();
            let payload = (*header.slots.get()).first;
            let backing = header.backing.dedicated();
            if backing.extend(payload, class.stride) {
                Some((page, backing.writable.get() - resident))
            } else {
                Page::destroy(page);
                None
            }
        }
    }

    /// Keep an emptied page resident for reuse; its stride leaves the live
    /// demand and its payload joins the retained bytes. Returns whether the heap must be put on `RETAINING`.
    fn retain(&mut self, page: PagePointer) -> bool {
        // SAFETY: this heap owns the empty page, its mapping and its key.
        let (bytes, stride) = unsafe {
            (
                page.as_ref().backing.dedicated().writable.get(),
                Page::key(page).class.stride,
            )
        };
        self.demand -= stride;
        self.retained_bytes += bytes;
        self.retained.push_back(page);
        !std::mem::replace(&mut self.listed, true)
    }

    fn remove_retained(&mut self, index: usize) -> PagePointer {
        let page = self.retained.remove(index).unwrap();
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
        // Build the guard only on success: a guard made for a failed attempt
        // would release the holder's lock when it drops.
        self.locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then(|| Locked(self))
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
            if let Some(page) = *state.head(class)
                && (*page.as_ref().key.get()).payer == payer.record_pointer().addr()
                && (*page.as_ref().key.get()).class.align == class.align
                && (*page.as_ref().slots.get()).live + 1 < (*page.as_ref().slots.get()).limit
            {
                if (*page.as_ref().slots.get()).free.is_null() {
                    return Self::extend_take(state, page, bytes);
                }
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

    /// Extend `page`'s empty free list, then take its head. A miss of the
    /// inline path, called last with the page's own class so the hit
    /// neither saves registers nor spills its class.
    /// # Safety
    /// As [`Self::take`], with the heap lock held as `state`.
    #[cold]
    #[inline(never)]
    unsafe fn extend_take(mut state: Locked<'_>, page: PagePointer, bytes: usize) -> NonNull<u8> {
        // SAFETY: the caller's contract; the lock pins the page's key.
        unsafe {
            let class = Page::key(page).class;
            extend(&mut state, page, class);
            Self::take(page, class, bytes)
        }
    }

    /// # Safety
    /// The heap lock is held and `page`'s free list holds a slot of `class`.
    #[inline(always)]
    unsafe fn take(page: PagePointer, class: Class, bytes: usize) -> NonNull<u8> {
        // SAFETY: the caller owns the page's slots under the heap lock.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let free = NonNull::new_unchecked(slots.free);
            #[cfg(feature = "memcheck")]
            allocation_check::link_access(free, true);
            slots.free = free.cast::<*mut u8>().read();
            let pointer = free;
            slots.live += 1;
            #[cfg(feature = "memcheck")]
            {
                slots.checked_bytes = bytes;
                allocation_check::allocated(pointer, bytes, class.kind);
            }
            #[cfg(not(feature = "memcheck"))]
            let _ = bytes;
            #[cfg(not(any(test, feature = "charge-observation", feature = "memcheck")))]
            let _ = class;
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
            if ready.is_none()
                && class.stride <= NURSERY_STRIDE
                && !growable
                && let Some(pointer) = Self::nurse(heap, &mut state, payer, class, bytes)
            {
                return pointer;
            }
            let (page, fresh) = if let Some(page) = ready {
                (page, None)
            } else if let Some((page, fresh)) = state.cached(class) {
                Page::reuse(page, payer, key);
                (page, Some(fresh))
            } else {
                (Page::new(heap, payer, key), None)
            };
            if !slab {
                let backing = page.as_ref().backing.dedicated();
                backing.born.set(class.stride);
                let writable = backing.writable.get();
                state
                    .dedicated
                    .add_live(0, class.stride, fresh.unwrap_or(writable));
            }
            if (*page.as_ref().slots.get()).free.is_null() {
                extend(&mut state, page, class);
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

    /// Place a block of a class without a page in the payer's nursery,
    /// while the class has placements left.
    /// # Safety
    /// The caller holds the heap lock as `state`.
    unsafe fn nurse(
        heap: &Arc<Self>,
        state: &mut Locked<'_>,
        payer: Payer,
        class: Class,
        bytes: usize,
    ) -> Option<NonNull<u8>> {
        let key = (payer.record_pointer().addr(), class.kind);
        let nursery = state.nurseries.entry(key).or_insert_with(|| Nursery {
            page: None,
            left: [NURSERY_BLOCKS; SLAB_BINS],
        });
        let left = &mut nursery.left[usize::from(class.bin)];
        *left = left.checked_sub(1)?;
        let page = *nursery
            .page
            .get_or_insert_with(|| Page::nursery(heap, payer, class.kind));
        // SAFETY: the heap lock owns the nursery's slots.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let start = (slots.first.addr().get() + slots.bump).next_multiple_of(class.align);
            let end = start + class.stride;
            if end > page.as_ptr().addr() + page_bytes() {
                // Full: it leaves with its last block.
                nursery.page = None;
                state.idle_nurseries = true;
                state.0.pending.store(true, Ordering::Relaxed);
                if slots.live == 0 {
                    Page::destroy(page);
                }
                return None;
            }
            slots.bump = end - slots.first.addr().get();
            slots.live += 1;
            slots.limit = slots.live;
            let pointer = slots.first.with_addr(std::num::NonZeroUsize::new_unchecked(
                start + class.offset(),
            ));
            #[cfg(feature = "memcheck")]
            allocation_check::allocated(pointer, bytes, class.kind);
            #[cfg(not(feature = "memcheck"))]
            let _ = bytes;
            #[cfg(not(any(test, feature = "charge-observation", feature = "memcheck")))]
            let _ = class;
            #[cfg(any(test, feature = "charge-observation"))]
            crate::peak::allocated(pointer.as_ptr(), class.stride);
            Some(pointer)
        }
    }

    /// Whether the task's heap has pages to trim while it waits.
    #[inline]
    pub(crate) fn pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }

    /// Return the listed pages' native pages that hold no live slot, drop
    /// empty and filled nurseries, and unmap the oldest detached empty slabs
    /// beyond `EMPTY_SLAB_LIMIT`.
    #[cold]
    #[inline(never)]
    pub(crate) fn trim(&self) {
        let mut state = self.lock();
        self.pending.store(false, Ordering::Relaxed);
        let mut listed = std::mem::take(&mut state.listed);
        for page in listed.drain(..) {
            // SAFETY: listed pages are live slab pages owned by this heap;
            // close empties the list before destroying any of them.
            unsafe { trim_page(page) };
        }
        state.listed = listed;
        if std::mem::take(&mut state.idle_nurseries) {
            // A departed payer leaves neither a page nor a budget behind.
            state.nurseries.retain(|_, nursery| match nursery.page {
                None => false,
                // SAFETY: the heap lock owns the nurseries' slots; an empty
                // current nursery has no other reference.
                Some(page) => unsafe {
                    if (*page.as_ref().slots.get()).live != 0 {
                        return true;
                    }
                    Page::destroy(page);
                    false
                },
            });
        }
        while state.empty_slabs.len() > EMPTY_SLAB_LIMIT {
            let oldest = state.empty_slabs.pop_front().unwrap();
            // SAFETY: the cache held the empty slab's only reference, and the
            // trim list, just emptied, no longer names it.
            unsafe { Page::destroy(oldest) };
        }
    }

    /// Retire the task's cache; pages with transferred blocks keep their heap
    /// alive and are released by the last free on whichever task owns that block.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.listed = Vec::new();
        self.pending.store(false, Ordering::Relaxed);
        let nurseries = std::mem::take(&mut state.nurseries)
            .into_values()
            .filter_map(|nursery| nursery.page)
            // SAFETY: the heap lock owns the nurseries' slots.
            .filter(|&page| unsafe { (*page.as_ref().slots.get()).live == 0 })
            .collect::<Vec<_>>();
        state.partial = HashMap::default();
        let spares = std::mem::replace(&mut state.heads, [[None; SLAB_BINS]; 3])
            .into_iter()
            .flatten()
            .flatten()
            // SAFETY: the heap lock owns the queued pages' slots.
            .filter(|&page| unsafe { (*page.as_ref().slots.get()).live == 0 })
            .collect::<Vec<_>>();
        let slabs = std::mem::take(&mut state.empty_slabs);
        let dedicated = std::mem::take(&mut state.dedicated);
        drop(state);
        if dedicated.listed {
            // An idle worker may have taken the entry already.
            let mut heaps = RETAINING.lock().unwrap();
            if let Some(index) = heaps
                .iter()
                .position(|heap| ptr::eq(Arc::as_ptr(heap), self))
            {
                heaps.swap_remove(index);
                RETAINING_HEAPS.fetch_sub(1, Ordering::Relaxed);
            }
        }
        for page in spares.into_iter().chain(slabs).chain(nurseries) {
            // SAFETY: detached from the cache, with no live slots.
            unsafe { Page::destroy(page) };
        }
        for page in dedicated.clean.into_iter().chain(dedicated.retained) {
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
        heap.lock()
            .dedicated
            .add_live(key.class.stride, class.stride, added);
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
#[inline(always)]
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
        // as the spare unless it is spread. A closed heap destroys the page at
        // its last slot.
        if slots.live != slots.limit
            && (slots.live > slots.watch
                || (slots.live == 1
                    && slots.head
                    && !state.closed
                    && !(*page.as_ref().spread.get()).spread))
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

/// Put a spread page on its heap's trim list.
/// # Safety
/// The heap lock is held as `state` and owns `page`.
unsafe fn list(state: &mut Locked<'_>, page: PagePointer) {
    // SAFETY: the heap lock owns the page's spread record.
    let spread = unsafe { &mut *page.as_ref().spread.get() };
    if !spread.listed && !state.closed {
        spread.listed = true;
        state.listed.push(page);
        state.0.pending.store(true, Ordering::Relaxed);
    }
}

/// Thread fresh slots onto an empty free list.
/// # Safety
/// The heap lock is held as `state`; `page` has a free slot of `class`
/// and an empty free list.
#[cold]
#[inline(never)]
unsafe fn extend(state: &mut Locked<'_>, page: PagePointer, class: Class) {
    // SAFETY: the heap lock owns the slots. Every threaded slot lies in the
    // page's mapping below `limit`.
    unsafe {
        let slots = &mut *page.as_ref().slots.get();
        let stride = class.stride;
        let header = page.as_ptr().addr();
        let region = slots.first.addr().get() - class.offset();
        let native = page_bytes();
        if slots.bump == slots.limit {
            // Only lost slots remain, and none of them is live.
            for index in (0..slots.limit).rev() {
                let start = region + index * stride - header;
                if overlaps(slots.discarded, start, stride, native) {
                    push_free(slots, slots.first.add(index * stride));
                }
            }
            slots.discarded = 0;
            return;
        }
        let start = region + slots.bump * stride;
        let boundary = (start & !(native - 1)) + native;
        let count = (boundary - start)
            .div_ceil(stride)
            .min(slots.limit - slots.bump);
        for index in (slots.bump..slots.bump + count).rev() {
            push_free(slots, slots.first.add(index * stride));
        }
        slots.bump += count;
        if class.fits_slab() {
            let spread = &mut *page.as_ref().spread.get();
            let end = region + slots.bump * stride - header;
            if end > spread.touched {
                spread.touched = end;
                if end > 2 * native {
                    spread.spread = true;
                    slots.watch = (slots.bump / 8).max(1);
                    list(state, page);
                }
            }
        }
    }
}

/// Whether the slot at `start..start + stride` from the header overlaps a
/// native page in `mask`.
fn overlaps(mask: u16, start: usize, stride: usize, native: usize) -> bool {
    let first = start / native;
    let last = (start + stride - 1) / native;
    let span = (u32::MAX >> (31 - (last - first) as u32)) << first;
    u32::from(mask) & span != 0
}

/// Return a listed page's native pages that hold no live slot, and lower
/// its frontier to just past its highest live slot.
/// # Safety
/// The heap lock is held, and `page` is a live slab page of this heap.
unsafe fn trim_page(page: PagePointer) {
    // SAFETY: the heap lock owns the slots, free list and spread record.
    // Free slots are not live, so their native pages may be discarded.
    unsafe {
        let spread = &mut *page.as_ref().spread.get();
        spread.listed = false;
        let class = Page::key(page).class;
        let slots = &mut *page.as_ref().slots.get();
        let native = page_bytes();
        let header = page.as_ptr().addr();
        let stride = class.stride;
        let region = slots.first.addr().get() - class.offset() - header;
        // Too few free slots to cover a native page: nothing to return.
        if slots.live != 0 && (slots.bump - slots.live) * stride < native {
            return;
        }
        let mut free = [0_u64; SLAB / 8 / 64];
        let mut cursor = slots.free;
        while let Some(slot) = NonNull::new(cursor) {
            let index = (slot.addr().get() - slots.first.addr().get()) / stride;
            free[index / 64] |= 1 << (index % 64);
            #[cfg(feature = "memcheck")]
            allocation_check::link_access(slot, true);
            cursor = slot.cast::<*mut u8>().read();
            #[cfg(feature = "memcheck")]
            allocation_check::link_access(slot, false);
        }
        let is_free = |index: usize| free[index / 64] & (1 << (index % 64)) != 0;
        // Native pages with a live slot, and the header's.
        let mut keep = 1_u32;
        let mut top = 0;
        if slots.live != 0 {
            for index in 0..slots.bump {
                let start = region + index * stride;
                if is_free(index) || overlaps(slots.discarded, start, stride, native) {
                    continue;
                }
                let (first, last) = (start / native, (start + stride - 1) / native);
                keep |= (u32::MAX >> (31 - (last - first) as u32)) << first;
                top = index + 1;
            }
        }
        let live_end = region + top * stride;
        let mut lost = 0_u32;
        let mut run: Option<usize> = None;
        let pages = spread.touched.div_ceil(native).min(SLAB / native);
        for at in 1..=pages {
            let returned =
                at < pages && keep & (1 << at) == 0 && u32::from(slots.discarded) & (1 << at) == 0;
            if at < pages && keep & (1 << at) == 0 && at * native < live_end {
                lost |= 1 << at;
            }
            match (returned, run) {
                (true, None) => run = Some(at),
                (false, Some(from)) => {
                    discard(header + from * native, (at - from) * native);
                    run = None;
                }
                _ => {}
            }
        }
        // Rebuild the free list in address order from free slots that
        // overlap no returned page.
        slots.free = ptr::null_mut();
        for index in (0..top).rev() {
            if is_free(index) && !overlaps(lost as u16, region + index * stride, stride, native) {
                push_free(slots, slots.first.add(index * stride));
            }
        }
        slots.bump = top;
        slots.discarded = lost as u16;
        spread.touched = (32 - keep.leading_zeros()) as usize * native;
        spread.spread = spread.touched > 2 * native;
    }
}

fn discard(start: usize, bytes: usize) {
    // SAFETY: whole anonymous native pages of a live slab mapping that hold
    // no live slot; the allocator rewrites a slot before handing it out.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::madvise(
            ptr::without_provenance_mut(start),
            bytes,
            libc::MADV_DONTNEED,
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (start, bytes);
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
        let key = Page::key(page);
        let class = key.class;
        let slots = &mut *page.as_ref().slots.get();
        if class.stride == 0 {
            slots.live -= 1;
            slots.limit = slots.live;
            if slots.live == 0 {
                if !state.closed
                    && state
                        .nurseries
                        .get(&(key.payer, class.kind))
                        .is_some_and(|nursery| nursery.page == Some(page))
                {
                    // Kept for the payer's next block; the park trim
                    // returns it if the payer allocates nothing until then.
                    slots.bump = 0;
                    state.idle_nurseries = true;
                    state.0.pending.store(true, Ordering::Relaxed);
                } else {
                    drop(state);
                    Page::destroy(page);
                }
            }
            return;
        }
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
            if state.dedicated.retain(page) {
                RETAINING
                    .lock()
                    .unwrap()
                    .push(Arc::clone(&page.as_ref().heap));
                RETAINING_HEAPS.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        push_free(slots, payload);
        if (*page.as_ref().spread.get()).spread && slots.live <= slots.watch {
            // The owner trims its listed pages when it parks. Another task's
            // release may come after that park, so it trims here.
            if ptr::eq(crate::tasks::current_heap(), state.0) {
                slots.watch = 1;
                list(&mut state, page);
            } else {
                trim_page(page);
                slots.watch = if (*page.as_ref().spread.get()).spread {
                    (slots.live / 8).max(1)
                } else {
                    1
                };
            }
        }
        if slots.live != 0 {
            if was_full {
                state.link(page, class);
            }
        } else if !was_full && slots.head {
            // Already the queue head: it stays as the spare.
        } else if state.spare(class).is_none() {
            // The emptied page becomes the spare, its key the current list's.
            if !was_full {
                state.unlink(page, class);
            }
            state.file_current(class);
            state.link(page, class);
        } else {
            if !was_full {
                state.unlink(page, class);
            }
            state.empty_slabs.push_back(page);
            if state.empty_slabs.len() > EMPTY_SLAB_LIMIT {
                // A running task reuses its burst's slabs; it trims them when it parks.
                state.0.pending.store(true, Ordering::Relaxed);
            }
        }
    }
}

struct Mapping {
    base: NonNull<u8>,
    len: usize,
    // Dedicated payload bytes made writable. A live slot's sole owner or the
    // empty cache under the heap mutex can update this extent.
    writable: Cell<usize>,
    // The stride of the block a dedicated page was last allocated for, before
    // any growth in place. Updated by the allocating heap under its mutex.
    born: Cell<usize>,
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
                born: Cell::new(0),
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

pub(crate) fn page_bytes() -> usize {
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

    /// A worker's free of a transferred block contends with the owner's
    /// allocation. A failed attempt must leave the holder's lock held, or
    /// both edit one free list and hand the same slot to two blocks.
    #[test]
    fn a_failed_try_lock_leaves_the_holder_locked() {
        let heap = Heap::default();
        let held = heap.lock();
        assert!(heap.try_lock().is_none());
        assert!(
            heap.try_lock().is_none(),
            "a failed attempt released the lock"
        );
        drop(held);
    }

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

    // Tests that observe residency against `purge`, which is
    // process-wide.
    static IDLE: Mutex<()> = Mutex::new(());

    fn mapping(payload: NonNull<u8>) -> (NonNull<u8>, usize) {
        // SAFETY: the owned buffer pins its backing until free, and the cache
        // retains the reservation during the observations below.
        let backing = unsafe { page_of(payload).as_ref().backing.dedicated() };
        (backing.base, backing.len)
    }

    /// A heap that serves many payers, as a session server's does, nurses
    /// each payer's first small blocks instead of giving the later payers a
    /// slab per class, and a payer that frees everything leaves no nursery
    /// once the task parks.
    #[test]
    fn every_payer_nurses_its_first_blocks_and_leaves_nothing_after_purge() {
        let _idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
        const PAYERS: usize = 3 * NURSERY_BLOCKS as usize;
        const LIMIT: u64 = 1 << 20;
        let contexts: Vec<_> = (0..PAYERS).map(|_| Context::new(LIMIT)).collect();
        let heap = contexts[0].heap.get_or_init(Arc::default);
        let blocks: Vec<_> = contexts
            .iter()
            .flat_map(|context| {
                let payer = context.payer();
                [8, 32, 40, 128].map(|bytes| {
                    block::admit_storage(payer, Some(bytes as u64)).unwrap();
                    let pointer = Heap::allocate(heap, payer, bytes, 8, block::PLAIN, false);
                    (pointer, bytes)
                })
            })
            .collect();
        // Each payer's first small blocks share one native page.
        let pages: Vec<_> = blocks
            .chunks(4)
            .map(|row| {
                let page = row[0].0.addr().get() / page_bytes();
                for &(pointer, bytes) in row {
                    let other = pointer.addr().get() / page_bytes();
                    assert_eq!(other, page, "a {bytes}-byte block left its payer's page");
                }
                page * page_bytes()
            })
            .collect();
        for (pointer, bytes) in blocks {
            // SAFETY: the test frees each block once with its full charge.
            unsafe { block::free(pointer, bytes as u64) };
        }
        heap.trim();
        // The worker blocks with nothing to run.
        purge();
        for page in pages {
            let mut flag = 0;
            // SAFETY: mincore only reads the page tables; an unmapped range fails with ENOMEM.
            let mapped = unsafe { libc::mincore(page as *mut _, page_bytes(), &mut flag) } == 0;
            assert!(
                !mapped || flag & 1 == 0,
                "an emptied payer's page stayed resident"
            );
        }
        for context in &contexts {
            assert_eq!(context.available(), LIMIT);
        }
    }

    #[test]
    fn emptied_dedicated_pages_stay_resident_within_the_peak_until_idle() {
        let _idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
        const SMALL: usize = 8 * 1024;
        const LARGE: usize = 1024 * 1024;
        const OVERSIZED: usize = 8 * LARGE;
        let context = Context::new(OVERSIZED as u64);
        let payer = context.payer();
        let heap = context.heap.get_or_init(Arc::default);
        let allocate = |bytes, kind| {
            block::admit_storage(payer, Some(bytes as u64)).unwrap();
            let pointer = Heap::allocate(heap, payer, bytes, 8, kind, true);
            // SAFETY: this test owns the new buffer and initializes its first page.
            unsafe { pointer.as_ptr().write_bytes(0x5a, 4096) };
            pointer
        };
        let free = |pointer, bytes| {
            // SAFETY: the test transfers ownership with its full admitted charge.
            unsafe { block::free(pointer, bytes as u64) };
            assert_eq!(context.available(), OVERSIZED as u64);
        };

        // An emptied page keeps its payload resident for the next block.
        let small = allocate(SMALL, block::PLAIN);
        free(small, SMALL);
        assert!(resident(small));
        let reused = allocate(SMALL, block::PLAIN);
        assert_eq!(reused, small);
        free(reused, SMALL);

        // 1 MiB cannot reuse the 8 KiB page and raises the peak to 1 MiB, so
        // keeping the 8 KiB page would hold more than the peak.
        let large = allocate(LARGE, block::PLAIN);
        let (base, len) = mapping(large);
        assert!(!resident(small));
        free(large, LARGE);
        assert!(resident(large));
        // However long the heap stays busy, its payload stays until a worker idles.
        purge();
        assert_eq!(heap.lock().dedicated.retained_bytes, 0);
        assert_eq!(resident_pages(base, len), 0);

        // A GRAPH buffer's cleanup frame uses the preceding native page;
        // release discards that page too, and the reservation is reused.
        let graph = allocate(OVERSIZED, block::GRAPH);
        let (base, len) = mapping(graph);
        // SAFETY: the exclusive buffer owns its reserved cleanup frame.
        unsafe { graph.as_ptr().sub(own_prefix(block::GRAPH)).write(0x5a) };
        free(graph, OVERSIZED);
        assert!(resident(base));
        purge();
        let resident = resident_pages(base, len);
        assert_eq!(
            resident, 0,
            "released graph mapping retained {resident} native pages"
        );
        let graph_again = allocate(OVERSIZED, block::GRAPH);
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

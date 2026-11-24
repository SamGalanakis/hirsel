//! Each serial custodian owns fixed-slot slabs shared by its payers.
//! Live small slots store their own payer ahead of the ABI ownership prefix.
//! Dedicated mappings keep their payer in the descriptor.
//!
//! Only the custodian changes open free lists. Foreign workers publish
//! returned slots, and an outstanding slot pins its page until collection.
mod returns;
pub(crate) mod slabs;

use crate::{abi_layout::own_prefix, limit::Payer};
use std::cell::{Cell, UnsafeCell};
use std::collections::{HashMap, VecDeque};
use std::mem::MaybeUninit;
use std::num::NonZeroUsize;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
pub(crate) fn before_foreign_return(pause: impl FnOnce() + 'static) {
    returns::BEFORE_PUBLISH.replace(Some(Box::new(pause)));
}

pub(crate) const SLAB: usize = 64 * 1024;
const MIN_PAGE_BYTES: usize = 4096;
const PAYER_BYTES: usize = size_of::<Payer>();
const NATIVE_KINDS: usize = crate::abi_layout::NATIVE_SPLIT_BYTES as usize + 1;
const _: () = assert!(
    own_prefix(crate::block::GRAPH) + PAYER_BYTES <= i8::MAX as usize
        && own_prefix(crate::block::RESOURCE) + PAYER_BYTES <= i8::MAX as usize
        && own_prefix(crate::block::SPLIT_BYTES) + PAYER_BYTES <= i8::MAX as usize
);
const GROWTH_RESERVE_FACTOR: usize = 8;
const MIN_GROWTH_RESERVATION: usize = 256 * 1024;
// Bound clean reservations and their external descriptors.
const EMPTY_DEDICATED_LIMIT: usize = 8;
// A reused reservation may coexist with other warm extents from its growth sequence.
const DEDICATED_REUSE_MARGIN: usize = 4 << 20;
// A block of another stride reuses a resident extent of at most this many
// times its size, so live slack that only `free` returns stays bounded.
const DEDICATED_REUSE_WASTE: usize = 4;
/// How long a worker waits with nothing to run before it releases retained pages.
pub(crate) const RELEASE_DELAY_MS: i32 = 100;

// Heaps with retained pages, so an idle worker can release them.
// A heap is listed at most once (`PageCache::listed`).
static RETAINING: Mutex<Vec<Arc<Heap>>> = Mutex::new(Vec::new());
static RETAINING_HEAPS: AtomicUsize = AtomicUsize::new(0);

/// The poll timeout for a worker about to wait `timeout` ms (-1: no limit), and the
/// time after which a wait that finds nothing releases retained pages and stacks.
pub(crate) fn idle_wait(timeout: i32, warm_stacks: bool) -> (i32, Option<u64>) {
    let retaining = warm_stacks
        || slabs::retaining()
        || returns::orphaned_pending()
        || RETAINING_HEAPS.load(Ordering::Relaxed) != 0
        || crate::fiber::surplus();
    if !retaining || (0..=RELEASE_DELAY_MS).contains(&timeout) {
        (timeout, None)
    } else {
        let due = crate::os::monotonic_ns() + RELEASE_DELAY_MS as u64 * 1_000_000;
        (RELEASE_DELAY_MS, Some(due))
    }
}

/// Return every retained slab's and dedicated page's native pages to the
/// host. A worker calls this after an `idle_wait` that found nothing, or
/// before it parks without a poller.
pub(crate) fn purge() {
    returns::purge();
    slabs::purge();
    if RETAINING_HEAPS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let heaps = std::mem::take(&mut *RETAINING.lock().unwrap());
    RETAINING_HEAPS.fetch_sub(heaps.len(), Ordering::Relaxed);
    // Returns list the heap while holding its page cache, so release
    // the list lock before taking any cache lock.
    for heap in heaps {
        let mut cache = heap.cache().lock().unwrap();
        cache.listed = false;
        cache.purge_slabs();
        while !cache.dedicated.retained.is_empty() {
            let oldest = cache.dedicated.remove_retained(0);
            cache.dedicated.purge(oldest);
        }
    }
}
const SLAB_BINS: usize = size_bin(SLAB / 2) as usize + 1;
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
    slab: bool,
    /// Negative displacement to a slab slot's payer word.
    payer_offset: i8,
}
impl Class {
    fn for_block(bytes: usize, align: usize, kind: u8, growable: bool) -> Self {
        if let Some(class) = Self::direct(bytes, align, kind, growable) {
            return class;
        }
        let small = Self::rounded(bytes, align, kind, growable, true);
        if small.stride <= SLAB / 2 && (!growable || small.stride <= page_bytes()) {
            small
        } else {
            Self::dedicated(bytes, align, kind, growable)
        }
    }

    fn dedicated(bytes: usize, align: usize, kind: u8, growable: bool) -> Self {
        Self::rounded(bytes, align, kind, growable, false)
    }

    fn rounded(bytes: usize, align: usize, kind: u8, growable: bool, slab: bool) -> Self {
        let align = align.max(8);
        let payer_offset = if slab { Self::payer_offset(kind) } else { 0 };
        let prefix = own_prefix(kind) + if slab { PAYER_BYTES } else { 0 };
        let offset = round_up(prefix, align).unwrap_or_else(host_memory);
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
            slab,
            payer_offset,
        }
    }
    /// A word-aligned class from the table. Every ownership prefix is a
    /// whole number of words.
    #[inline(always)]
    fn direct(bytes: usize, align: usize, kind: u8, growable: bool) -> Option<Self> {
        if align > 8 || bytes > DIRECT_WORDS * 8 {
            return None;
        }
        let prefix = own_prefix(kind) + PAYER_BYTES;
        // The size guard above bounds this addition by the direct table
        // range plus the fixed prefix and one word of rounding.
        let words = (bytes.max(1) + prefix + 7) >> 3;
        let &(stride, bin) = DIRECT.get(words)?;
        Some(Self {
            stride: usize::from(stride),
            align: 8,
            kind,
            bin,
            growable,
            slab: true,
            payer_offset: -(prefix as i8),
        })
    }

    #[inline(always)]
    const fn payer_offset(kind: u8) -> i8 {
        -((own_prefix(kind) + PAYER_BYTES) as i8)
    }

    fn offset(self) -> usize {
        (own_prefix(self.kind) + if self.slab { PAYER_BYTES } else { 0 })
            .next_multiple_of(self.align)
    }
    fn fits_slab(self) -> bool {
        self.slab
    }
}

#[inline(always)]
fn round_up(bytes: usize, unit: usize) -> Option<usize> {
    Some(bytes.checked_add(unit - 1)? & !(unit - 1))
}

type PagePointer = NonNull<Page>;

struct IndexedPage(PagePointer);

// SAFETY: the index only publishes addresses. Live slots or the owning heap
// keep each descriptor alive. The index does not access page state.
unsafe impl Send for IndexedPage {}

// Mimalloc's arena metadata is indexed by slice. Dedicated mappings have no
// common arena, so route them by slice address through independent shards.
type DedicatedIndex = HashMap<usize, IndexedPage, foldhash::fast::FixedState>;

fn dedicated_index(payload: NonNull<u8>) -> &'static Mutex<Option<DedicatedIndex>> {
    static SHARDS: [Mutex<Option<DedicatedIndex>>; 64] = [const { Mutex::new(None) }; 64];
    let slice = payload.addr().get() / SLAB;
    &SHARDS[(slice ^ (slice >> 6) ^ (slice >> 12)) % SHARDS.len()]
}

/// Whether `payload` starts a dedicated mapping's block: a private anonymous
/// mapping that this heap made with `mmap` and owns whole.
pub(crate) fn is_dedicated(payload: NonNull<u8>) -> bool {
    payload.addr().get() & (SLAB - 1) == 0
        && dedicated_index(payload)
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|index| index.contains_key(&payload.addr().get()))
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
    returned: returns::Inbox,
    backing: Backing,
    dedicated_payer: UnsafeCell<MaybeUninit<Payer>>,
    payer_delta: UnsafeCell<usize>,
    class: UnsafeCell<Class>,
    slots: std::cell::UnsafeCell<Slots>,
    // Survives class changes: it describes the mapping's resident pages.
    spread: std::cell::UnsafeCell<Spread>,
}

#[derive(Default)]
struct Spread {
    /// Bytes from the header that may be resident.
    touched: u32,
    /// `touched` reaches a third native page.
    spread: bool,
    /// Index on the heap's trim list, if listed.
    listed: Option<NonZeroUsize>,
}

enum Backing {
    Slab(slabs::Slab),
    Dedicated(Mapping),
}

impl Backing {
    fn dedicated(&self) -> &Mapping {
        match self {
            Self::Dedicated(mapping) => mapping,
            Self::Slab(_) => unreachable!("expected dedicated backing"),
        }
    }
}

/// A slab page's slots below `bump` are live, on the free list, or lost:
/// a lost slot overlaps a native page in `discarded`, which a trim returned
/// while no live slot overlapped it. Lost slots rejoin the free list once
/// the frontier reaches `limit`.
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
    // SAFETY: a live slot retains the descriptor and its class. Open slot
    // and queue changes require the custodian. Only an empty page or the
    // sole owner of a dedicated mapping can change its class.
    #[inline(always)]
    unsafe fn class(page: PagePointer) -> Class {
        unsafe { *page.as_ref().class.get() }
    }

    /// # Safety
    /// The unrefunded slot belongs to this page.
    #[inline(always)]
    pub(crate) unsafe fn funding(page: PagePointer, payload: NonNull<u8>) -> Payer {
        // SAFETY: the live slot keeps its payer tag (or dedicated descriptor)
        // immutable until settlement. The slot's charge or zero pin keeps the
        // limit record occupied.
        unsafe {
            let address = payload
                .addr()
                .get()
                .wrapping_add(*page.as_ref().payer_delta.get());
            // The page pointer has the slab mapping's provenance for a
            // small slot and the descriptor's provenance for a dedicated one.
            page.as_ptr().with_addr(address).cast::<Payer>().read()
        }
    }

    /// # Safety
    /// The offset names the payer word in this slab slot.
    #[inline(always)]
    unsafe fn payer_tag(payload: NonNull<u8>, offset: i8) -> *mut Payer {
        // SAFETY: the slab layout reserves this aligned word ahead of the prefix.
        unsafe { payload.as_ptr().offset(isize::from(offset)).cast() }
    }

    /// # Safety
    /// A live slot pins this page's metadata.
    #[inline(always)]
    pub(crate) unsafe fn kind(page: PagePointer) -> u8 {
        // SAFETY: a live slot keeps its native kind.
        unsafe { (*page.as_ref().class.get()).kind }
    }

    /// # Safety
    /// The custodian owns the page's list state.
    unsafe fn set_head(page: PagePointer, head: bool) {
        // SAFETY: only the custodian accesses these fields while open.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            slots.head = head;
            if !(*page.as_ref().spread.get()).spread {
                // An unspread head can become its own empty spare.
                slots.watch = usize::from(!head);
            }
        }
    }

    fn new(heap: &Arc<Heap>, class: Class, dedicated_payer: Option<Payer>) -> PagePointer {
        if returns::orphaned_pending() {
            returns::purge();
        }
        let slab = class.fits_slab();
        let (backing, start) = if slab {
            let (allocation, start) = slabs::Slab::allocate(Arc::as_ptr(heap).addr());
            (Backing::Slab(allocation), start)
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
            returned: returns::Inbox::new(),
            backing,
            dedicated_payer: dedicated_payer
                .map_or_else(MaybeUninit::uninit, MaybeUninit::new)
                .into(),
            payer_delta: (class.payer_offset as usize).into(),
            class: class.into(),
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
            // SAFETY: the boxed descriptor has a stable address and has
            // yet to publish the mapping's sole payload.
            unsafe {
                let header = page.as_ref();
                let payer_address = header.dedicated_payer.get().addr();
                header
                    .payer_delta
                    .get()
                    .write(payer_address.wrapping_sub(start.addr().get()));
            }
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
            discarded: 0,
            watch: 1,
            #[cfg(feature = "memcheck")]
            checked_bytes: 0,
            head: false,
            previous: None,
            next: None,
        }
    }

    #[inline(never)]
    unsafe fn reuse(page: PagePointer, class: Class, dedicated_payer: Option<Payer>) {
        // SAFETY: the caller owns the empty page and verified it fits.
        unsafe {
            let header = page.as_ref();
            if let Some(payer) = dedicated_payer {
                header.dedicated_payer.get().write(MaybeUninit::new(payer));
            } else {
                header.payer_delta.get().write(class.payer_offset as usize);
            }
            header.class.get().write(class);
            let slots = page.as_ref().slots.get();
            let start = if class.slab {
                page.cast()
            } else {
                (*slots).first
            };
            slots.write(Self::slots(start, class));
        }
    }

    unsafe fn destroy(page: PagePointer) {
        // SAFETY: the caller owns the unreachable page.
        unsafe { Self::destroy_with(page, false) };
    }

    unsafe fn dispose(page: PagePointer) {
        // SAFETY: the caller owns the unreachable page.
        unsafe { Self::destroy_with(page, true) };
    }

    unsafe fn destroy_with(page: PagePointer, dispose: bool) {
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
        if dispose {
            match page.backing {
                Backing::Slab(slab) => {
                    if let Some(memory) = crate::units::memory() {
                        memory.dispose_slab(slab);
                    } else {
                        slab.dispose();
                    }
                }
                Backing::Dedicated(_) => unreachable!("only slabs are disposed here"),
            }
        } else if let Some(memory) = crate::units::memory() {
            match page.backing {
                Backing::Slab(slab) => memory.put_slab(slab),
                Backing::Dedicated(mapping) => memory.put_page(mapping),
            }
        } else {
            drop(page.backing);
        }
        drop(page.heap);
    }
}

/// A serial dispatcher owns the local lists. Foreign workers may publish a
/// page notification while its custodian runs, waits, or changes workers.
pub(crate) struct Heap {
    returned: returns::Queue,
    // An idle worker can purge detached stock while the task runs.
    cache: OnceLock<Box<Mutex<PageCache>>>,
    /// `State::listed` has slabs to trim.
    pending: Cell<bool>,
    // Either EMPTY_HEADS or the start of this heap's State allocation.
    heads: Cell<NonNull<Heads>>,
}

impl Default for Heap {
    fn default() -> Self {
        Self {
            returned: returns::Queue::new(),
            cache: OnceLock::new(),
            pending: Cell::new(false),
            heads: Cell::new(empty_heads()),
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let heads = self.heads.get();
        if heads != empty_heads() {
            // SAFETY: this is the start of the heap's State box, and no owner outlives the heap.
            drop(unsafe { Box::from_raw(heads.cast::<State>().as_ptr()) });
        }
    }
}

// SAFETY: context custody can transfer between threads. A live slot retains
// its page and its heap independently of the context.
unsafe impl Send for Heap {}

// SAFETY: only the exclusive context custodian accesses local state. Other
// workers access the return queue and page cache through synchronization.
unsafe impl Sync for Heap {}

/// The context custodian's access to the open heap. Each operation keeps
/// custody until it returns.
struct Local<'a>(&'a Heap);

impl Local<'_> {
    #[inline(always)]
    fn head(&self, class: Class) -> Option<PagePointer> {
        // SAFETY: the context custodian owns the private heads, or reads the sentinel.
        let heads = unsafe { self.0.heads.get().as_ref() };
        heads[usize::from(class.kind)][usize::from(class.bin)]
    }

    fn pages(&mut self) -> Option<&mut State> {
        let heads = self.0.heads.get();
        if heads == empty_heads() {
            return None;
        }
        // SAFETY: a non-sentinel pointer starts this heap's State box. The
        // context custodian owns the box for the returned borrow.
        Some(unsafe { heads.cast::<State>().as_mut() })
    }

    #[inline(never)]
    fn private_pages(&mut self) -> &mut State {
        self.pages().expect("the heap owns private pages")
    }

    #[cold]
    #[inline(never)]
    fn ensure_pages(&mut self) -> &mut State {
        let mut heads = self.0.heads.get();
        if heads == empty_heads() {
            heads = NonNull::from(Box::leak(Box::new(State::empty()))).cast();
            self.0.heads.set(heads);
        }
        // SAFETY: the pointer starts this heap's initialized State box and the
        // custodian owns the box for the returned borrow.
        unsafe { heads.cast::<State>().as_mut() }
    }
}

#[inline(always)]
fn finish_allocation(pointer: NonNull<u8>, class: Class) -> NonNull<u8> {
    #[cfg(feature = "allocation-count")]
    crate::allocation_count::allocated();
    crate::backing_observation::observe!(allocate, pointer.as_ptr(), class.stride, TaskHeap);
    #[cfg(not(any(test, feature = "charge-observation", feature = "prof")))]
    let _ = class;
    pointer
}

/// The classes with non-current alignments.
type PartialKey = (u8, u8, usize);

fn partial_key(class: Class) -> PartialKey {
    (class.kind, class.bin, class.align)
}

type Heads = [[Option<PagePointer>; SLAB_BINS]; NATIVE_KINDS];

struct EmptyHeads(UnsafeCell<Heads>);

// SAFETY: the sentinel is only read. The cell lets the linker place its
// zero-initialized heads in BSS.
unsafe impl Sync for EmptyHeads {}

fn empty_heads() -> NonNull<Heads> {
    static EMPTY_HEADS: EmptyHeads = EmptyHeads(UnsafeCell::new([[None; SLAB_BINS]; NATIVE_KINDS]));
    NonNull::new(EMPTY_HEADS.0.get()).unwrap()
}

// The pointer in Heap names the leading heads field and retains the provenance
// of the entire allocation when it points to a private State.
#[repr(C)]
struct State {
    /// Each slab class's current list: pages of one alignment, the
    /// key read off its first page. The first page may be empty, as the
    /// class's spare; every page after it is partial.
    heads: Heads,
    /// Every other key's partial pages, indexed by layout. Never holds an empty page or
    /// the current list's key.
    partial: HashMap<PartialKey, PagePointer, foldhash::fast::FixedState>,
    /// Spread pages to trim when the custodian parks.
    listed: Vec<PagePointer>,
}

#[derive(Default)]
struct PageCache {
    dedicated: DedicatedCache,
    /// Detached slabs, reused before claiming a fresh slab. A claim can
    /// raise the heap's slab count only when every slab is in use, so live
    /// and cached slabs together stay within peak simultaneous demand.
    slabs: Vec<PagePointer>,
    /// The heap is on `RETAINING`.
    listed: bool,
}

#[derive(Default)]
struct DedicatedCache {
    clean: VecDeque<PagePointer>,
    /// Empty pages with resident backing, oldest first.
    retained: VecDeque<PagePointer>,
    retained_bytes: usize,
    /// Backing that can be resident in live mappings, including reuse slack.
    demand: usize,
    peak_demand: usize,
}

impl State {
    const fn empty() -> Self {
        Self {
            heads: [[None; SLAB_BINS]; NATIVE_KINDS],
            partial: HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0)),
            listed: Vec::new(),
        }
    }
}

impl State {
    fn head(&mut self, class: Class) -> &mut Option<PagePointer> {
        &mut self.heads[usize::from(class.kind)][usize::from(class.bin)]
    }

    /// Unlink the class's head if the inline path filled it. Only the head may
    /// be full while linked, so a list that moves on leaves no full page behind.
    #[inline(never)]
    fn drop_full_head(&mut self, class: Class) {
        // SAFETY: this heap owns the head and its page's slots.
        unsafe {
            if let Some(page) = *self.head(class) {
                let slots = &*page.as_ref().slots.get();
                if slots.live == slots.limit {
                    self.unlink(page, class);
                }
            }
        }
    }

    /// The head's empty page, if the class keeps one.
    fn spare(&mut self, class: Class) -> Option<PagePointer> {
        // SAFETY: this heap owns the head and its page's slots.
        self.head(class)
            .filter(|&page| unsafe { (*page.as_ref().slots.get()).live == 0 })
    }

    fn available(&mut self, class: Class) -> Option<PagePointer> {
        self.drop_full_head(class);
        let wanted = partial_key(class);
        // SAFETY: this heap owns the lists. A linked page keeps its key while
        // it has a live slot, and the empty head keeps the key of the pages
        // behind it until it is rebound here.
        unsafe {
            let head = *self.head(class);
            if let Some(page) = head
                && partial_key(Page::class(page)) == wanted
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
                Page::set_head(page, false);
                let slots = &mut *page.as_ref().slots.get();
                slots.previous = None;
                let displaced = self.partial.insert(partial_key(Page::class(page)), page);
                debug_assert!(displaced.is_none(), "the current list's key is never filed");
            }
            let list = if self.partial.is_empty() {
                None
            } else {
                self.partial.remove(&wanted)
            };
            let Some(page) = spare else {
                *self.head(class) = list;
                if let Some(page) = list {
                    Page::set_head(page, true);
                }
                return list;
            };
            // The empty head stays first, rebound to the wanted key, with the
            // wanted partial pages after it. The same bin keeps its stride.
            if Page::class(page).align != class.align {
                Page::reuse(page, class, None);
            }
            Page::set_head(page, true);
            let slots = &mut *page.as_ref().slots.get();
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
        self.drop_full_head(class);
        // SAFETY: as the caller states; the current list's pages are partial
        // and its key is never filed while it is current.
        unsafe {
            if let Some(first) = self.head(class).take() {
                Page::set_head(first, false);
                let displaced = self.partial.insert(partial_key(Page::class(first)), first);
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
        self.drop_full_head(class);
        // SAFETY: exclusively held heap; a linked page's key is fixed.
        unsafe {
            let key = partial_key(Page::class(page));
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
                Some(first) if partial_key(Page::class(first)) == key => {
                    let first_slots = &mut *first.as_ref().slots.get();
                    if first_slots.live == 0 {
                        // The spare stays first; the partial page goes after it.
                        debug_assert!(live != 0, "a class keeps one spare");
                        let slots = &mut *page.as_ref().slots.get();
                        slots.previous = Some(first);
                        slots.next = first_slots.next.replace(page);
                        if let Some(next) = slots.next {
                            (*next.as_ref().slots.get()).previous = Some(page);
                        }
                        return;
                    }
                    Page::set_head(first, false);
                    Some(first)
                }
                Some(_) => {
                    debug_assert!(live != 0, "an empty page of another key is cached");
                    let next = self.partial.insert(key, page);
                    let slots = &mut *page.as_ref().slots.get();
                    slots.previous = None;
                    slots.next = next;
                    if let Some(next) = next {
                        (*next.as_ref().slots.get()).previous = Some(page);
                    }
                    return;
                }
            };
            Page::set_head(page, true);
            let slots = &mut *page.as_ref().slots.get();
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
                slots.watch = slots.watch.max(1);
                if let Some(next) = next {
                    Page::set_head(next, true);
                }
                *self.head(class) = next;
            } else {
                let key = partial_key(Page::class(page));
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
}

impl PageCache {
    fn list_retaining(&mut self, heap: &Arc<Heap>) {
        if !self.listed && !crate::units::untied() {
            let mut heaps = RETAINING.lock().unwrap();
            heaps.push(Arc::clone(heap));
            RETAINING_HEAPS.fetch_add(1, Ordering::Relaxed);
            self.listed = true;
        }
    }

    fn purge_slabs(&mut self) {
        for page in self.slabs.drain(..) {
            // SAFETY: the cache exclusively owns these empty, unlisted slabs.
            unsafe { Page::dispose(page) };
        }
    }
}

impl DedicatedCache {
    /// When backing grows, return older stock above the live high-water mark.
    /// Reusing a reservation can bring back a later phase of the same
    /// workload, so leave room for its other extents.
    #[inline(never)]
    fn add_live(&mut self, from: usize, to: usize, grew: bool, reused_reservation: bool) {
        self.demand = self.demand - from + to;
        self.peak_demand = self.peak_demand.max(self.demand);
        if !grew {
            return;
        }
        let margin = if reused_reservation {
            (self.peak_demand / 6).min(DEDICATED_REUSE_MARGIN)
        } else {
            0
        };
        while !self.retained.is_empty()
            && self.demand + self.retained_bytes > self.peak_demand + margin
        {
            let oldest = self.remove_retained(0);
            self.purge(oldest);
        }
    }

    fn take(&mut self, class: Class) -> Option<PagePointer> {
        let needed = class.stride.checked_next_multiple_of(page_bytes())?;
        let fit = |page: PagePointer| {
            // SAFETY: the heap owns these empty pages and their mappings.
            let (backing, payload) = unsafe {
                let page = page.as_ref();
                (page.backing.dedicated(), (*page.slots.get()).first)
            };
            let extent = backing.extent(payload);
            (extent >= needed
                && payload.addr().get() & (class.align - 1) == 0
                && backing.headroom(payload) >= own_prefix(class.kind))
            .then(|| (backing.used.get(), backing.born.get(), backing.grown.get()))
        };
        // Preserve the reservation for a repeated growth sequence. Only
        // extents that have not grown since their last purge can serve a
        // different starting size. Among those, prefer the smallest resident
        // fit, so buffers that cycle through varying sizes keep reusing their
        // warm extents instead of evicting them for fresh ones.
        let mut best = None;
        let mut best_used = usize::MAX;
        for (index, &page) in self.retained.iter().enumerate().rev() {
            if let Some((used, born, grown)) = fit(page) {
                if born == class.stride {
                    best = Some(index);
                    break;
                }
                if !grown
                    && used >= needed
                    && used < best_used
                    && used / DEDICATED_REUSE_WASTE <= needed
                {
                    best = Some(index);
                    best_used = used;
                }
            }
        }
        let page = if let Some(index) = best {
            self.remove_retained(index)
        } else {
            let index = self
                .clean
                .iter()
                .copied()
                .position(|page| fit(page).is_some())?;
            self.clean.swap_remove_back(index).unwrap()
        };
        // SAFETY: the page has been taken off the cache and has no live slots.
        unsafe {
            let header = page.as_ref();
            let payload = (*header.slots.get()).first;
            let backing = header.backing.dedicated();
            if backing.extend(payload, class.stride) {
                Some(page)
            } else {
                Page::destroy(page);
                None
            }
        }
    }

    /// Move an emptied page's backing from live demand to retained stock.
    fn retain(&mut self, page: PagePointer) {
        // SAFETY: this heap owns the empty page, its mapping and its key.
        let bytes = unsafe {
            let page = page.as_ref();
            page.backing.dedicated().backed((*page.slots.get()).first)
        };
        self.demand -= bytes;
        self.retained_bytes += bytes;
        self.retained.push_back(page);
    }

    fn remove_retained(&mut self, index: usize) -> PagePointer {
        let page = self.retained.remove(index).unwrap();
        // SAFETY: this heap exclusively owns the removed empty page.
        self.retained_bytes -= unsafe {
            let page = page.as_ref();
            page.backing.dedicated().backed((*page.slots.get()).first)
        };
        page
    }

    fn purge(&mut self, page: PagePointer) {
        // SAFETY: the heap owns this empty page and its separate descriptor.
        let header = unsafe { page.as_ref() };
        let payload = unsafe { (*header.slots.get()).first };
        if !header.backing.dedicated().discard_used(payload) {
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
    fn cache(&self) -> &Mutex<PageCache> {
        self.cache.get_or_init(Box::default)
    }

    /// # Safety
    /// The caller has exclusive context custody. The heap is open.
    unsafe fn collect_returns(&self) {
        // SAFETY: context custody excludes close and other open consumers.
        let mut batch = unsafe { self.returned.take() };
        while let Some((page, mut slots)) = batch.pop() {
            while let Some(payload) = slots.pop() {
                // SAFETY: the batch owns the slot and its live count.
                unsafe { release_local(Local(self), page, payload) };
            }
        }
    }

    #[inline(always)]
    pub(crate) fn allocate(
        context: &crate::Context,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
        growable: bool,
    ) -> NonNull<u8> {
        let heap = context.heap.get_or_init(Arc::default);
        // SAFETY: the context pins this open heap under exclusive custody.
        let payload = unsafe { Self::allocate_owned(heap, payer, bytes, align, kind, growable) };
        #[cfg(any(test, feature = "memcheck"))]
        crate::allocator::checked::allocated(payload, bytes.max(1));
        payload
    }

    /// # Safety
    /// The caller has exclusive context custody. The heap is open.
    #[inline]
    unsafe fn allocate_owned(
        heap: &Arc<Self>,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
        growable: bool,
    ) -> NonNull<u8> {
        // Keep heap creation and allocation misses out of the hit path.
        let Some(class) = Class::direct(bytes, align, kind, growable) else {
            // SAFETY: the custodian owns the open heap.
            return unsafe { Self::allocate_slow(heap, payer, bytes, align, kind, growable) };
        };
        let state = Local(heap);
        // SAFETY: this heap owns its queues. A matching head can hand out
        // a free slot and may stay linked when full until the list next moves.
        // Direct classes are below a native page, so they always fit a slab.
        unsafe {
            if let Some(page) = state.head(class)
                && (*page.as_ref().class.get()).align == class.align
            {
                let slots = &*page.as_ref().slots.get();
                if !slots.free.is_null() {
                    let pointer = Self::take(page, payer, class, bytes);
                    return finish_allocation(pointer, class);
                }
                if slots.live < slots.limit && !heap.returned.pending() {
                    return Self::extend_take(state, page, payer, bytes);
                }
            }
            Self::allocate_slow(heap, payer, bytes, align, kind, growable)
        }
    }

    /// Extend `page`'s empty free list, then take its head. A miss of the
    /// inline path, called last with the page's own class so the hit
    /// neither saves registers nor spills its class.
    /// # Safety
    /// As [`Self::take`], with the exclusive heap custody held as `state`.
    #[cold]
    #[inline(never)]
    unsafe fn extend_take(
        mut state: Local<'_>,
        page: PagePointer,
        payer: Payer,
        bytes: usize,
    ) -> NonNull<u8> {
        // SAFETY: the caller's contract; the live page keeps its key.
        unsafe {
            let class = Page::class(page);
            let heap = state.0;
            let pages = state.private_pages();
            extend(heap, pages, page, class);
            let pointer = Self::take(page, payer, class, bytes);
            finish_allocation(pointer, class)
        }
    }

    /// # Safety
    /// The caller owns the heap and `page`'s free list holds a slot of `class`.
    #[inline(always)]
    unsafe fn take(page: PagePointer, payer: Payer, class: Class, bytes: usize) -> NonNull<u8> {
        // SAFETY: the caller owns the page's slots.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let free = NonNull::new_unchecked(slots.free);
            #[cfg(feature = "memcheck")]
            allocation_check::link_access(free, true);
            slots.free = free.cast::<*mut u8>().read();
            let pointer = free;
            slots.live += 1;
            if class.slab {
                Page::payer_tag(pointer, class.payer_offset).write(payer);
            }
            #[cfg(feature = "memcheck")]
            {
                slots.checked_bytes = bytes;
                allocation_check::allocated(pointer, bytes, class.kind);
            }
            #[cfg(not(feature = "memcheck"))]
            let _ = bytes;
            pointer
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn allocate_slow(
        heap: &Arc<Self>,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
        growable: bool,
    ) -> NonNull<u8> {
        // SAFETY: only the context custodian consumes an open heap's returns.
        unsafe { heap.collect_returns() };
        let mut state = Local(heap);
        let class = Class::for_block(bytes, align, kind, growable);
        // SAFETY: this heap owns all queues and the live slot until publication.
        unsafe {
            let slab = class.fits_slab();
            let pages = state.ensure_pages();
            let ready = if slab { pages.available(class) } else { None };
            let dedicated_payer = (!slab).then_some(payer);
            let mut dedicated = (!slab).then(|| heap.cache().lock().unwrap());
            let mut cached = || match dedicated.as_mut() {
                Some(cache) => cache.dedicated.take(class),
                None => heap.cache().lock().unwrap().slabs.pop(),
            };
            let page = if let Some(page) = ready {
                page
            } else if let Some(page) = cached() {
                Page::reuse(page, class, dedicated_payer);
                if slab && (*page.as_ref().spread.get()).spread {
                    list(heap, pages, page);
                }
                page
            } else {
                Page::new(heap, class, dedicated_payer)
            };
            if let Some(cache) = dedicated.as_mut() {
                let payload = (*page.as_ref().slots.get()).first;
                let backing = page.as_ref().backing.dedicated();
                let reused_reservation = backing.born.get() != 0;
                backing.born.set(class.stride);
                let before = backing.backed(payload);
                let after = backing.use_extent(payload, class.stride);
                cache
                    .dedicated
                    .add_live(0, after, after > before, reused_reservation);
            }
            drop(dedicated);
            if (*page.as_ref().slots.get()).free.is_null() {
                extend(heap, pages, page, class);
            }
            let pointer = Self::take(page, payer, class, bytes);
            let slots = &*page.as_ref().slots.get();
            if slots.live == slots.limit {
                if ready.is_some() {
                    pages.unlink(page, class);
                }
            } else if ready.is_none() {
                pages.link(page, class);
            }
            finish_allocation(pointer, class)
        }
    }

    /// Whether the heap has pages to trim while its custodian waits.
    #[inline]
    /// # Safety
    /// The caller is the context custodian or its dispatcher during handoff.
    pub(crate) unsafe fn pending(&self) -> bool {
        self.pending.get() || self.returned.pending()
    }

    /// Collect foreign returns and discard native pages holding no live slot.
    ///
    /// # Safety
    /// The caller has exclusive context custody. The heap is open.
    #[cold]
    #[inline(never)]
    pub(crate) unsafe fn trim(&self) {
        // SAFETY: the dispatcher owns this heap throughout the trim.
        unsafe { self.collect_returns() };
        let mut local = Local(self);
        self.pending.set(false);
        let Some(state) = local.pages() else {
            return;
        };
        // Untied units park through their own dispatcher, which does not
        // call the worker's delayed purge.
        if crate::units::untied()
            && let Some(cache) = self.cache.get()
        {
            cache.lock().unwrap().purge_slabs();
        }
        let mut listed = std::mem::take(&mut state.listed);
        for page in listed.drain(..) {
            // SAFETY: listed pages are live slab pages owned by this heap;
            // close empties the list before destroying any of them.
            unsafe { trim_page(page) };
        }
        state.listed = listed;
    }

    /// Detach every local cache before handing the live counts to
    /// the workers that will retire the remaining slots.
    ///
    /// # Safety
    /// The caller has exclusive context custody and closes this heap once.
    pub(crate) unsafe fn close(&self) {
        let mut local = Local(self);
        if let Some(state) = local.pages() {
            state.listed.clear();
            state.partial = HashMap::default();
            for head in state.heads.as_flattened_mut() {
                if let Some(page) = *head {
                    *head = None;
                    // SAFETY: a live head pins itself and any empty head is detached.
                    unsafe {
                        if (*page.as_ref().slots.get()).live == 0 {
                            Page::destroy(page);
                        }
                    }
                }
            }
        }
        if let Some(cache) = self.cache.get() {
            let cache = std::mem::take(&mut *cache.lock().unwrap());
            if cache.listed {
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
            for page in cache
                .dedicated
                .clean
                .into_iter()
                .chain(cache.dedicated.retained)
                .chain(cache.slabs)
            {
                // SAFETY: detached from the cache, with no outstanding slots.
                unsafe { Page::destroy(page) };
            }
        }
        // SAFETY: no code can access the open slot records after this handoff.
        unsafe { self.returned.close().retire() };
    }
}

/// # Safety
/// A live own or resource block allocated by this heap.
#[inline]
pub(crate) unsafe fn page_of(payload: NonNull<u8>) -> PagePointer {
    #[cfg(any(test, feature = "memcheck"))]
    crate::allocator::checked::require_live(payload);
    let address = payload.addr().get();
    if address & (SLAB - 1) == 0 {
        return dedicated_page(payload);
    }
    // SAFETY: slab slots share their mapping with its aligned descriptor.
    unsafe { NonNull::new_unchecked(payload.as_ptr().with_addr(address & !(SLAB - 1)).cast()) }
}

/// # Safety
/// A live block whose kind was assigned by the allocator.
pub(crate) unsafe fn kind(payload: NonNull<u8>) -> u8 {
    // SAFETY: exclusively owned slots pin the page metadata.
    unsafe { Page::kind(page_of(payload)) }
}

/// # Safety
/// The caller owns the live buffer exclusively and has admitted the full
/// new charge. On success the caller must install the new logical capacity
/// and settle the returned payer with the old charge.
#[cfg(not(feature = "memcheck"))]
pub(crate) unsafe fn try_reassign_plain_slab(
    payload: NonNull<u8>,
    payer: Payer,
    bytes: usize,
    align: usize,
) -> Option<Payer> {
    if payload.addr().get() & (SLAB - 1) == 0 {
        return None;
    }
    // SAFETY: the unique live slot pins its slab and owns its payer word.
    unsafe {
        let class = Page::class(page_of(payload));
        // A plain slab slot has only one word of prefix for its payer.
        // Its alignment is at least that large, so padding the prefix to
        // the slot's alignment makes the payload start at that alignment.
        if class.kind != crate::block::PLAIN
            || class.align < align
            || bytes > class.stride - class.align
        {
            return None;
        }
        let tag = Page::payer_tag(payload, class.payer_offset);
        Some(tag.replace(payer))
    }
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
        if !Arc::ptr_eq(heap, &header.heap) || Page::funding(page, payload) != payer {
            return false;
        }
        let class = Class::dedicated(bytes, align, kind, true);
        let backing = header.backing.dedicated();
        let reused_reservation = class.stride <= backing.writable.get();
        if !backing.extend(payload, class.stride) {
            return false;
        }
        backing.grown.set(true);
        let before = backing.backed(payload);
        let after = backing.use_extent(payload, class.stride);
        heap.cache().lock().unwrap().dedicated.add_live(
            before,
            after,
            after > before,
            reused_reservation,
        );
        #[cfg(feature = "prof")]
        let old_bytes = (*header.class.get()).stride;
        header.class.get().write(class);
        #[cfg(any(test, feature = "memcheck"))]
        crate::allocator::checked::resized(payload, bytes.max(1));
        #[cfg(feature = "memcheck")]
        {
            let slots = &mut *header.slots.get();
            allocation_check::resized(payload, slots.checked_bytes, bytes, kind);
            slots.checked_bytes = bytes;
        }
        crate::backing_observation::observe!(resize, payload.as_ptr(), old_bytes, class.stride);
        true
    }
}

/// # Safety
/// The caller transfers the unique, cleaned slot after its logical refund.
#[inline]
pub(crate) unsafe fn release(context: &crate::Context, payload: NonNull<u8>) {
    // SAFETY: the slot pins its header and the page's heap while live.
    unsafe { release_page(page_of(payload), payload, Some(context)) };
}

/// # Safety
/// The caller transfers the unique cleaned slot after its logical refund, and
/// `page` is the owning page returned by `page_of(payload)` for that slot.
#[inline(always)]
pub(crate) unsafe fn release_page(
    page: PagePointer,
    payload: NonNull<u8>,
    context: Option<&crate::Context>,
) {
    // SAFETY: the outstanding slot pins the page through publication.
    let heap = unsafe { &page.as_ref().heap };
    crate::backing_observation::observe!(
        free,
        payload.as_ptr(),
        unsafe { Page::class(page) }.stride,
    );
    #[cfg(any(test, feature = "memcheck"))]
    if crate::allocator::checked::enabled() {
        // SAFETY: the caller transfers this slot and its outstanding page count.
        unsafe {
            crate::allocator::checked::retire(
                payload,
                crate::allocator::checked::Storage::Task(page),
            )
        };
        return;
    }
    #[cfg(feature = "memcheck")]
    // SAFETY: the outstanding slot pins its immutable kind.
    unsafe {
        allocation_check::freed(payload, Page::kind(page))
    };
    let local = match context {
        Some(context) => context
            .heap
            .get()
            .is_some_and(|owner| Arc::ptr_eq(owner, heap)),
        None => crate::tasks::owns_current_heap(heap),
    };
    if local {
        // SAFETY: this context is the page's custodian and the heap is open.
        unsafe { release_local(Local(heap), page, payload) };
    } else {
        // SAFETY: the publisher owns this slot until its successful CAS.
        unsafe {
            page.as_ref()
                .returned
                .publish(&heap.returned, page, payload)
        };
    }
}

/// Return a quarantined slot without assuming custody of its original heap.
/// # Safety
/// The quarantine transfers the exclusive slot and its live page count.
#[cfg(any(test, feature = "memcheck"))]
pub(crate) unsafe fn release_quarantined(page: PagePointer, payload: NonNull<u8>) {
    // SAFETY: the retained live count pins the page until publication, even
    // after heap closure. The inbox handles returns to a sealed heap.
    unsafe {
        let header = page.as_ref();
        #[cfg(feature = "memcheck")]
        allocation_check::freed(payload, Page::kind(page));
        header
            .returned
            .publish(&header.heap.returned, page, payload);
    }
}

/// # Safety
/// `local` has exclusive context custody and owns the returned slot.
#[inline(always)]
unsafe fn release_local(state: Local<'_>, page: PagePointer, payload: NonNull<u8>) {
    // SAFETY: only the custodian changes slots or recycles pages while open.
    unsafe {
        let slots = &mut *page.as_ref().slots.get();
        // A partial page or the head keeps its queue position, and an emptied
        // head stays as the spare unless it is spread.
        if slots.live > slots.watch && (slots.live != slots.limit || slots.head) {
            slots.live -= 1;
            push_free(slots, payload);
        } else {
            release_slow(state, page, payload);
        }
    }
}

/// # Safety
/// The context custodian owns `slots`; `payload` is one of its slots, no longer live.
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
/// The context custodian owns `pages` and `page`.
unsafe fn list(heap: &Heap, pages: &mut State, page: PagePointer) {
    // SAFETY: the context custodian owns the page's spread record.
    let spread = unsafe { &mut *page.as_ref().spread.get() };
    if spread.listed.is_none() {
        pages.listed.push(page);
        spread.listed = NonZeroUsize::new(pages.listed.len());
        heap.pending.set(true);
    }
}

/// # Safety
/// The context custodian owns `page` and every slab on the trim list.
unsafe fn unlist(pages: &mut State, page: PagePointer) {
    // SAFETY: custody covers the removed slab and the entry that takes its place.
    unsafe {
        if let Some(index) = (*page.as_ref().spread.get()).listed.take() {
            let at = index.get() - 1;
            pages.listed.swap_remove(at);
            if let Some(moved) = pages.listed.get(at) {
                (*moved.as_ref().spread.get()).listed = Some(index);
            }
        }
    }
}

/// Thread fresh slots onto an empty free list.
/// # Safety
/// The context custodian owns `pages` and `page`. The page has a free
/// slot of `class` and an empty free list.
#[cold]
#[inline(never)]
unsafe fn extend(heap: &Heap, pages: &mut State, page: PagePointer, class: Class) {
    // SAFETY: the context custodian owns the slots. Every threaded slot lies in the
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
            let end = (region + slots.bump * stride - header) as u32;
            if end > spread.touched {
                spread.touched = end;
                if end as usize > 2 * native {
                    spread.spread = true;
                    slots.watch = (slots.bump / 8).max(1);
                    list(heap, pages, page);
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
/// The context custodian owns this live slab page.
unsafe fn trim_page(page: PagePointer) {
    // SAFETY: the context custodian owns the slots, free list and spread record.
    // Free slots are not live, so their native pages may be discarded.
    unsafe {
        let spread = &mut *page.as_ref().spread.get();
        spread.listed = None;
        let class = Page::class(page);
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
        let pages = (spread.touched as usize)
            .div_ceil(native)
            .min(SLAB / native);
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
        spread.touched = (32 - keep.leading_zeros()) * native as u32;
        spread.spread = spread.touched as usize > 2 * native;
        if !spread.spread {
            slots.watch = usize::from(!slots.head);
        }
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
/// lowers a spread page to its trim threshold.
/// # Safety
/// As [`release`], with the page's exclusive heap custody held as `state`.
#[cold]
#[inline(never)]
unsafe fn release_slow(mut state: Local<'_>, page: PagePointer, payload: NonNull<u8>) {
    // SAFETY: the context custodian owns the free list and page lifecycle.
    unsafe {
        let class = Page::class(page);
        let slots = &mut *page.as_ref().slots.get();
        // A full page is in no list, unless it is the head the inline path filled.
        let was_out = slots.live == slots.limit && !slots.head;
        slots.live -= 1;
        let heap = state.0;
        if payload.addr().get() & (SLAB - 1) == 0 {
            let mut cache = heap.cache().lock().unwrap();
            cache.dedicated.retain(page);
            cache.list_retaining(&page.as_ref().heap);
            return;
        }
        let pages = state.private_pages();
        push_free(slots, payload);
        if (*page.as_ref().spread.get()).spread && slots.live <= slots.watch {
            slots.watch = 1;
            list(heap, pages, page);
        }
        if slots.live != 0 {
            if was_out {
                pages.link(page, class);
            }
        } else if !was_out && slots.head {
            // Already the queue head: it stays as the spare.
        } else if pages.spare(class).is_none() {
            // The emptied page becomes the spare, its key the current list's.
            if !was_out {
                pages.unlink(page, class);
            }
            pages.file_current(class);
            pages.link(page, class);
        } else {
            if !was_out {
                pages.unlink(page, class);
            }
            unlist(pages, page);
            let mut cache = heap.cache().lock().unwrap();
            cache.slabs.push(page);
            cache.list_retaining(&page.as_ref().heap);
            heap.pending.set(true);
        }
    }
}

pub(crate) struct Mapping {
    base: NonNull<u8>,
    len: usize,
    prefix: usize,
    // Dedicated payload bytes made writable. A live slot's sole owner or the
    // empty cache under its mutex can update this extent.
    writable: Cell<usize>,
    // The payload extent that may have been touched since the last purge.
    // Writable pages beyond it are still clean.
    used: Cell<usize>,
    // The starting stride of the last live allocation, before in-place growth.
    born: Cell<usize>,
    // A live block grew in place since the last purge, so `used` holds its
    // growth sequence's extent for a later block of stride `born`.
    grown: Cell<bool>,
}
// SAFETY: an idle mapping is uniquely owned. Moving it preserves the address;
// only the owner can unmap it or change its writable extent.
unsafe impl Send for Mapping {}

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
        let mapping = if let Some(memory) = crate::units::memory() {
            memory.aligned_page(bytes, space, align, headroom)?
        } else {
            Self::map_aligned(bytes, space, align, headroom)?
        };
        // SAFETY: the complete mapping contains this requested prefix.
        let payload = unsafe { mapping.base.add(headroom) };
        mapping.born.set(0);
        mapping.grown.set(false);
        Some((mapping, payload))
    }

    pub(crate) fn pooled_page(
        memory: &crate::units::Memory,
        bytes: usize,
        space: usize,
        align: usize,
        headroom: usize,
    ) -> Option<Self> {
        let len = space.checked_add(headroom)?;
        memory
            .take_page(|mapping| {
                mapping.len == len
                    && mapping.prefix == headroom
                    && mapping.writable.get() >= bytes
                    && (mapping.base.addr().get() + headroom) & (align - 1) == 0
            })
            .or_else(|| memory.map(move || Self::map_aligned(bytes, space, align, headroom)))
    }

    fn map_aligned(bytes: usize, space: usize, align: usize, headroom: usize) -> Option<Self> {
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
                prefix: headroom,
                writable: Cell::new(bytes),
                used: Cell::new(0),
                born: Cell::new(0),
                grown: Cell::new(false),
            };
            if space != bytes
                && libc::mprotect(base.as_ptr().cast(), headroom + bytes, Self::WRITABLE) != 0
            {
                return None;
            }
            Some(mapping)
        }
    }

    pub(crate) fn committed_bytes(&self) -> usize {
        self.prefix + self.writable.get()
    }

    fn headroom(&self, payload: NonNull<u8>) -> usize {
        payload.addr().get() - self.base.addr().get()
    }

    fn extent(&self, payload: NonNull<u8>) -> usize {
        self.len - self.headroom(payload)
    }

    #[inline(never)]
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

    fn backed(&self, payload: NonNull<u8>) -> usize {
        let used = self.used.get();
        if used == 0 {
            0
        } else {
            self.headroom(payload) + used
        }
    }

    #[inline(never)]
    fn use_extent(&self, payload: NonNull<u8>, bytes: usize) -> usize {
        self.used
            .set(self.used.get().max(bytes.next_multiple_of(page_bytes())));
        self.backed(payload)
    }

    fn discard_used(&self, payload: NonNull<u8>) -> bool {
        // SAFETY: the empty dedicated mapping owns this whole writable range.
        if unsafe { Mapping::discard(self.base, self.backed(payload)) } {
            self.used.set(0);
            self.grown.set(false);
            true
        } else {
            false
        }
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
    use std::collections::HashSet;
    use std::sync::Barrier;

    #[test]
    fn trim_and_purge_keep_a_slot_until_its_foreign_return_is_published() {
        let _idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
        const BYTES: usize = 512;
        const CHARGE: u64 = 17;
        const LIMIT: u64 = 1 << 20;
        const MARKER: u64 = 0x517a_83ce_c301_a991;
        let context = Context::new(LIMIT);
        let mut blocks: Vec<_> = (0..64)
            .map(|_| block::Block::new(&context, Some(CHARGE), BYTES, 8).unwrap())
            .collect();
        let index = blocks
            .iter()
            .position(|block| block.pointer().addr().get() % SLAB >= 3 * page_bytes())
            .expect("the fixture reaches a third native page");
        let pending = blocks.swap_remove(index).into_raw();
        // SAFETY: the fixture still owns this block and the second word is writable.
        unsafe { pending.cast::<u64>().add(1).write(MARKER) };
        let address = pending.as_ptr().expose_provenance();
        for block in blocks {
            // SAFETY: the owning context consumes each block with its full charge.
            unsafe { block::free_in(&context, block.into_raw(), CHARGE) };
        }

        let paused = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let gate = paused.clone();
            let foreign = scope.spawn(move || {
                before_foreign_return(move || {
                    gate.wait();
                    gate.wait();
                });
                let pointer = NonNull::new(ptr::with_exposed_provenance_mut(address)).unwrap();
                // SAFETY: this thread owns the transferred slot and charge.
                unsafe { block::free(pointer, CHARGE) };
            });
            paused.wait();
            context.current.drain();
            let refunded = context.available();
            let heap = context.heap.get().unwrap();
            // SAFETY: the custodian trims while the foreign return owns
            // its unpublished slot. No other thread writes the marker.
            let marker = unsafe {
                heap.trim();
                purge();
                pending.cast::<u64>().add(1).read()
            };
            paused.wait();
            foreign.join().unwrap();
            assert_eq!(refunded, LIMIT, "refund waited for physical publication");
            assert_eq!(marker, MARKER, "purge discarded a slot before publication");
            // SAFETY: the fixture still has exclusive context custody.
            unsafe { heap.trim() };
        });
        context.current.drain();
        assert_eq!(
            context.available(),
            LIMIT,
            "physical collection refunded twice"
        );
    }

    #[test]
    fn concurrent_foreign_returns_to_one_page_reuse_every_slot_once() {
        use hirsel_core_net::SplitMix64;

        const LIMIT: u64 = 4096;
        const COUNT: usize = 8;
        const CHARGE: u64 = 5;
        for seed in 0..32 {
            let context = Context::new(LIMIT);
            let payer = context.payer();
            let allocate = || {
                block::admit_storage(payer, Some(CHARGE)).unwrap();
                Heap::allocate(&context, payer, 16, 8, block::PLAIN, true)
            };
            let mut slots = [0; COUNT];
            for address in &mut slots {
                *address = allocate().as_ptr().expose_provenance();
            }
            let original = HashSet::from(slots);
            let mut choice = SplitMix64::new(seed, 0);
            let gate = Arc::new(Barrier::new(COUNT));
            std::thread::scope(|scope| {
                for address in slots {
                    let gate = gate.clone();
                    let yields = choice.choose(4);
                    scope.spawn(move || {
                        before_foreign_return(move || {
                            // Every producer reads the same queue head before
                            // any can publish. Retry must retain every slot.
                            gate.wait();
                            for _ in 0..yields {
                                std::thread::yield_now();
                            }
                        });
                        let pointer =
                            NonNull::new(ptr::with_exposed_provenance_mut(address)).unwrap();
                        // SAFETY: each producer owns its distinct slot and exact charge.
                        unsafe { block::free(pointer, CHARGE) };
                    });
                }
            });
            context.current.drain();
            assert_eq!(context.available(), LIMIT, "seed {seed}");
            let heap = context.heap.get().unwrap();
            // SAFETY: this thread is the open context's sole custodian.
            unsafe { heap.collect_returns() };
            let reused: [_; COUNT] = std::array::from_fn(|_| allocate());
            assert_eq!(context.available(), LIMIT - COUNT as u64 * CHARGE);
            assert_eq!(
                reused
                    .iter()
                    .map(|p| p.addr().get())
                    .collect::<HashSet<_>>(),
                original,
                "a return was lost or a slot was reused twice at seed {seed}",
            );
            for pointer in reused {
                // SAFETY: the owner transfers each distinct reused slot exactly once.
                unsafe { block::free_in(&context, pointer, CHARGE) };
            }
            assert_eq!(context.available(), LIMIT, "seed {seed}");
        }
    }

    #[test]
    fn mixed_payers_keep_their_refunds_across_foreign_return_and_slot_reuse() {
        use crate::Capacity;

        // The budget ledger below follows the supplied grants, charges and
        // refunds. No physical allocator counter is part of the oracle.
        fn balance(payer: Payer, available: u64, outstanding: u64) {
            // SAFETY: the fixture owns the active payer or its live blocks;
            // no peer is using its table during the snapshot.
            let record = unsafe { &*payer.record_pointer() };
            assert_eq!(
                (record.available, record.outstanding),
                (available, outstanding)
            );
            let requested = available + 1;
            assert_eq!(
                payer.admit(Some(requested)),
                Err(Capacity {
                    requested,
                    available
                })
            );
        }

        fn foreign_return(block: block::Block) {
            let charge = block.charge();
            let address = block.into_raw().as_ptr().expose_provenance();
            std::thread::spawn(move || {
                let foreign = Context::new(64);
                let anchor = block::Block::new(&foreign, Some(1), 24, 8).unwrap();
                let returned = NonNull::new(ptr::with_exposed_provenance_mut(address)).unwrap();
                // SAFETY: the foreign custodian owns both live slots, one
                // transferred from the other heap and one of its own.
                unsafe {
                    block::free_in(&foreign, returned, charge);
                    block::free_in(&foreign, anchor.into_raw(), 1);
                }
                assert_eq!(foreign.available(), 64);
            })
            .join()
            .unwrap();
        }

        // A deliberately misrouted refund can corrupt the retired-domain sum.
        // Keep cleanup from aborting over the original arithmetic assertion.
        let mut context = std::mem::ManuallyDrop::new(Context::new(128));
        let root = context.payer();
        context.limit_enter(64).unwrap();
        let a = context.payer();
        balance(root, 64, 0);
        balance(a, 64, 0);
        let a1 = block::Block::new(&context, Some(7), 24, 8).unwrap();
        balance(a, 57, 7);

        context.limit_enter(24).unwrap();
        let b = context.payer();
        balance(a, 33, 7);
        balance(b, 24, 0);
        let b1 = block::Block::new(&context, Some(11), 24, 8).unwrap();
        balance(a, 33, 7);
        balance(b, 13, 11);
        let a2 = block::Block::paid(&context, a, Some(6), 24, 8, block::PLAIN).unwrap();
        balance(a, 27, 13);
        balance(b, 13, 11);
        let b2 = block::Block::new(&context, Some(5), 24, 8).unwrap();
        balance(a, 27, 13);
        balance(b, 8, 16);
        // SAFETY: all four blocks pin their slab descriptors.
        unsafe {
            let page = page_of(a1.pointer());
            for block in [&b1, &a2, &b2] {
                assert_eq!(page_of(block.pointer()), page);
            }
        }

        foreign_return(a1);
        balance(a, 27, 13);
        balance(b, 8, 16);
        // Hold another handle across scope exit so the last handle takes the
        // general retirement path while both of B's blocks remain live.
        let b_handle = context.current.clone();
        let b_address = b.record_pointer();
        context.limit_exit();
        drop(b_handle);
        balance(a, 35, 13);

        context.limit_enter(10).unwrap();
        let c = context.payer();
        assert_ne!(
            c.record_pointer(),
            b_address,
            "reused a payer with 16 outstanding bytes"
        );
        balance(a, 25, 13);
        balance(c, 10, 0);
        context.limit_exit();
        balance(a, 35, 13);

        context.current.drain();
        balance(a, 42, 6);
        // SAFETY: B's two blocks still pin its forwarding record.
        assert_eq!(unsafe { (*b_address).outstanding }, 16);
        foreign_return(b1);
        balance(a, 42, 6);
        // SAFETY: B's second block and the pending refund keep B occupied.
        assert_eq!(unsafe { (*b_address).outstanding }, 16);
        context.current.drain();
        balance(a, 53, 6);
        // SAFETY: B's second block still pins the slot.
        assert_eq!(unsafe { (*b_address).outstanding }, 5);
        // SAFETY: the owner frees B's final live block exactly once.
        unsafe { block::free_in(&context, b2.into_raw(), 5) };
        balance(a, 58, 6);

        context.limit_enter(9).unwrap();
        let d = context.payer();
        assert_eq!(
            d.record_pointer(),
            b_address,
            "settled payer was not reused"
        );
        balance(a, 49, 6);
        balance(d, 9, 0);
        let d1 = block::Block::new(&context, Some(4), 24, 8).unwrap();
        balance(a, 49, 6);
        balance(d, 5, 4);
        // Collect A's and B's already settled slots while D occupies B's
        // old record. Their stale tag words cannot settle anything again.
        // SAFETY: the fixture has sole physical custody of its open heap.
        unsafe { context.heap.get().unwrap().trim() };
        balance(a, 49, 6);
        balance(d, 5, 4);

        // SAFETY: the owner frees D's slot exactly once, with its charge.
        unsafe { block::free_in(&context, d1.into_raw(), 4) };
        balance(d, 9, 0);
        context.limit_exit();
        balance(a, 58, 6);
        foreign_return(a2);
        balance(a, 58, 6);
        context.current.drain();
        balance(a, 64, 0);
        context.limit_exit();
        balance(root, 128, 0);
        drop(std::mem::ManuallyDrop::into_inner(context));
    }

    /// The inline path takes a head page's last slot and leaves the full page
    /// linked. Only the head may be full while linked: before another page
    /// goes first, or the class's list is filed, the full head leaves the
    /// lists. A full page left behind the head is relinked by its next free
    /// while still listed, which cycles the list or hands a slot out twice.
    #[test]
    fn a_full_head_leaves_the_lists_before_another_page_goes_first() {
        const LIMIT: u64 = 1 << 24;
        const BYTES: usize = 40;
        let context = Context::new(LIMIT);
        let heap = context.heap.get_or_init(Arc::default);
        let payer = context.payer();
        let allocate = || {
            block::admit_storage(payer, Some(BYTES as u64)).unwrap();
            Heap::allocate(&context, payer, BYTES, 8, block::PLAIN, false)
        };
        // SAFETY: each block is live and freed once with its full charge.
        let free =
            |pointer: NonNull<u8>| unsafe { block::free_in(&context, pointer, BYTES as u64) };
        // A slab page's (full, head) state.
        let state = |pointer: NonNull<u8>| {
            // SAFETY: the block is live, so its page is; this thread is the
            // heap's only user and holds no borrow of the slots.
            unsafe {
                let page = page_of(pointer);
                let slots = &*page.as_ref().slots.get();
                Some((slots.live == slots.limit, slots.head))
            }
        };
        // Every listed page is listed once with matching back links, and
        // only a current list's first page may be full.
        let check = || {
            let mut local = Local(heap);
            let state = local.pages().unwrap();
            let mut seen = HashSet::new();
            let firsts = state
                .heads
                .iter()
                .flatten()
                .flatten()
                .map(|&page| (page, true));
            let filed = state.partial.values().map(|&page| (page, false));
            for (first, current) in firsts.chain(filed) {
                let mut previous = None;
                let mut at = Some(first);
                while let Some(page) = at {
                    assert!(seen.insert(page), "a page is listed twice");
                    // SAFETY: the heap lock owns every listed page's slots.
                    let slots = unsafe { &*page.as_ref().slots.get() };
                    assert_eq!(slots.previous, previous, "a broken back link");
                    let leads = current && previous.is_none();
                    assert_eq!(slots.head, leads, "a stale head mark");
                    assert!(
                        slots.live < slots.limit || leads,
                        "a full page behind the head"
                    );
                    previous = at;
                    at = slots.next;
                }
            }
        };
        let mut live = Vec::new();
        // Fill pages until a full head is linked while an earlier full page is out.
        let (out, head) = loop {
            let pointer = allocate();
            live.push(pointer);
            if state(pointer) == Some((true, true))
                && let Some(&out) = live.iter().find(|&&q| state(q) == Some((true, false)))
            {
                break (out, pointer);
            }
        };
        check();
        // The out page's free links it first, ahead of the full head.
        free(out);
        live.retain(|&pointer| pointer != out);
        check();
        // The head's free finds it out of the lists, or relinks a listed page.
        free(head);
        live.retain(|&pointer| pointer != head);
        check();
        for _ in 0..4096 {
            live.push(allocate());
        }
        check();
        let mut addresses: Vec<_> = live.iter().map(|pointer| pointer.addr().get()).collect();
        addresses.sort_unstable();
        addresses.dedup();
        assert_eq!(addresses.len(), live.len(), "a slot was handed out twice");
        live.into_iter().for_each(free);
        assert_eq!(context.available(), LIMIT);
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

    #[test]
    fn emptied_dedicated_pages_stay_resident_within_the_peak_until_idle() {
        let _idle = IDLE.lock().unwrap_or_else(|poison| poison.into_inner());
        const SMALL: usize = 8 * 1024;
        const LARGE: usize = 1024 * 1024;
        const OVERSIZED: usize = 8 * LARGE;
        let context = Context::new(OVERSIZED as u64);
        let payer = context.payer();
        let heap = context.heap.get_or_init(Arc::default);
        let allocate_unwritten = |bytes, kind| {
            block::admit_storage(payer, Some(bytes as u64)).unwrap();
            Heap::allocate(&context, payer, bytes, 8, kind, true)
        };
        let allocate = |bytes, kind| {
            let pointer = allocate_unwritten(bytes, kind);
            // SAFETY: this test owns the new buffer and initializes its first page.
            unsafe { pointer.as_ptr().write_bytes(0x5a, 4096) };
            pointer
        };
        let free = |pointer, bytes| {
            // SAFETY: the test transfers ownership with its full admitted charge.
            unsafe { block::free_in(&context, pointer, bytes as u64) };
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
        // SAFETY: the test owns LARGE writable bytes.
        unsafe { large.add(page_bytes()).write(0x5a) };
        assert!(!resident(small));
        free(large, LARGE);
        assert!(resident(large));

        // A different stride that fits the resident mapping reuses it without
        // faulting, and MEM-21 charges the new request independently of its backing.
        const MEDIUM: usize = LARGE / 2;
        let medium = allocate_unwritten(MEDIUM, block::PLAIN);
        // SAFETY: the second page lies within MEDIUM. Allocation itself
        // touches the first page to thread the slot, so observe the second.
        assert!(
            resident(unsafe { medium.add(page_bytes()) }),
            "a compatible dedicated page lost residency before reuse"
        );
        assert_eq!(medium, large);
        assert_eq!(context.available(), (OVERSIZED - MEDIUM) as u64);
        free(medium, MEDIUM);
        // The original stride comes back to the same warm extent, though the
        // smaller block reused it last.
        let large_again = allocate_unwritten(LARGE, block::PLAIN);
        assert_eq!(large_again, large);
        // SAFETY: the second page lies within LARGE.
        assert!(
            resident(unsafe { large_again.add(page_bytes()) }),
            "an extent last reused at a smaller stride lost residency"
        );
        free(large_again, LARGE);

        // However long the heap stays busy, its payload stays until a worker idles.
        purge();
        assert_eq!(heap.cache().lock().unwrap().dedicated.retained_bytes, 0);
        assert_eq!(resident_pages(base, len), 0);

        // A repeated growth sequence starts small and needs its previously
        // touched larger extent again. Admit the full replacement charge
        // before growth and refund the old charge when the extent commits.
        const GROWN: usize = 8 * SMALL;
        let grow = |pointer: NonNull<u8>| {
            block::admit_storage(payer, Some(GROWN as u64)).unwrap();
            // SAFETY: the test owns the slot and admitted the replacement.
            assert!(unsafe { try_grow_in_place(heap, pointer, payer, GROWN, 8, block::PLAIN) });
            payer.settle(SMALL as u64);
            assert_eq!(context.available(), (OVERSIZED - GROWN) as u64);
            // SAFETY: the returned page is within the grown payload.
            unsafe { pointer.add(GROWN - page_bytes()) }
        };
        let start = allocate_unwritten(SMALL, block::PLAIN);
        let tail = grow(start);
        // SAFETY: the test owns GROWN writable bytes.
        unsafe { tail.write(0x5a) };
        free(start, GROWN);
        // An unrelated request can use a compatible ungrown mapping. It
        // must leave the reservation for the upcoming growth sequence.
        let unrelated = allocate_unwritten(GROWN / 2, block::PLAIN);
        free(unrelated, GROWN / 2);
        let start_again = allocate_unwritten(SMALL, block::PLAIN);
        assert!(
            resident(grow(start_again)),
            "restarting growth lost residency at the end of the grown extent"
        );
        free(start_again, GROWN);
        purge();

        // A smaller, prefix-bearing extent has to use a separate mapping.
        // It remains warm while a previously cleaned large reservation
        // comes back into use in the same burst.
        let recent = allocate(SMALL, block::GRAPH);
        free(recent, SMALL);
        let large_again = allocate_unwritten(LARGE, block::PLAIN);
        assert!(
            resident(recent),
            "reusing a reservation discarded the recent extent"
        );
        free(large_again, LARGE);
        purge();

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
        slab_residency_until_idle();
    }

    fn slab_residency_until_idle() {
        const BYTES: usize = 24 * 1024;
        const SLABS: usize = 12;
        // The 24 KiB payload and payer tag round to a 28 KiB stride. Two
        // slots fit per 64 KiB slab, leaving eleven detached slabs and one
        // class spare when all 24 blocks are freed.
        const LIMIT: u64 = (SLABS * 2 * BYTES) as u64;
        let context = Context::new(LIMIT);
        let payer = context.payer();
        let heap = context.heap.get_or_init(Arc::default);
        let burst = || -> Vec<_> {
            (0..SLABS * 2)
                .map(|_| {
                    block::admit_storage(payer, Some(BYTES as u64)).unwrap();
                    Heap::allocate(&context, payer, BYTES, 8, block::PLAIN, false)
                })
                .collect()
        };
        let free = |blocks: Vec<NonNull<u8>>| {
            for pointer in blocks {
                // SAFETY: each slot belongs to this heap and carries the admitted charge.
                unsafe { block::free_in(&context, pointer, BYTES as u64) };
            }
            assert_eq!(context.available(), LIMIT);
        };
        let probes = |blocks: &[NonNull<u8>]| {
            let mut pages: Vec<_> = blocks
                .iter()
                .map(|&pointer| {
                    let base = pointer.addr().get() & !(SLAB - 1);
                    pointer.with_addr(NonZeroUsize::new(base + page_bytes()).unwrap())
                })
                .collect();
            pages.sort_unstable();
            pages.dedup();
            pages
        };
        let count_resident = |pages: &[NonNull<u8>]| {
            pages
                .iter()
                .filter(|page| {
                    let mut flag = 0;
                    // SAFETY: mincore queries residency without touching the
                    // page, including after a fully empty range was unmapped.
                    let mapped =
                        unsafe { libc::mincore(page.as_ptr().cast(), page_bytes(), &mut flag) };
                    mapped == 0 && flag & 1 != 0
                })
                .count()
        };

        let first = burst();
        for pointer in &first {
            // SAFETY: the first burst owns every slot and touches its payload.
            unsafe { pointer.as_ptr().write_bytes(0x5a, BYTES) };
        }
        let pages = probes(&first);
        assert_eq!(pages.len(), SLABS);
        assert_eq!(context.available(), 0);
        free(first);
        assert_eq!(count_resident(&pages), SLABS);
        assert!(matches!(idle_wait(-1, false), (RELEASE_DELAY_MS, Some(_))));
        let second = burst();
        assert_eq!(probes(&second), pages);
        assert_eq!(count_resident(&pages), SLABS);
        free(second);

        // SAFETY: the test is the sole custodian. The dispatcher trims the
        // attached class spare, then the idle worker purges detached slabs.
        unsafe { heap.trim() };
        purge();
        assert_eq!(count_resident(&pages), 0);
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static MAP_REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "memcheck")]
mod allocation_check;

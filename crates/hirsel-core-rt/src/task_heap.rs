//! Fixed-slot pages with one payer and one owner per page. Like mimalloc's
//! page queues, a class keeps only pages that can supply a block. The task
//! owns the queue even if it resumes on a different worker.
use crate::{abi_layout::own_prefix, limit::Payer};
use std::cell::Cell;
use std::collections::VecDeque;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, OnceLock};

const SLAB: usize = 64 * 1024;
const HEADER_BACK: usize = 4096;
const GROWTH_RESERVE_FACTOR: usize = 8;
const MIN_GROWTH_RESERVATION: usize = 256 * 1024;
// Each cached reservation keeps its header resident even after discarding its payload.
const EMPTY_DEDICATED_LIMIT: usize = 8;
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
        let align = align.max(8);
        let offset = own_prefix(kind)
            .checked_next_multiple_of(align)
            .unwrap_or_else(host_memory);
        let bytes = bytes.max(1).checked_add(offset).unwrap_or_else(host_memory);
        let bin = bytes
            .max(64)
            .checked_next_power_of_two()
            .unwrap_or_else(host_memory)
            / 8;
        let stride = bytes
            .checked_next_multiple_of(align.max(bin))
            .unwrap_or_else(host_memory);
        Self {
            stride,
            align,
            kind,
            bin: size_bin(stride),
            growable,
        }
    }
    fn offset(self) -> usize {
        own_prefix(self.kind).next_multiple_of(self.align)
    }
    fn fits_slab(self) -> bool {
        // A dedicated mapping would cost a header page for a sub-page payload.
        (self.bin as usize) < SLAB_BINS && (!self.growable || self.stride <= page_bytes())
    }
}

#[derive(Clone, Copy)]
struct Key {
    payer: usize,
    class: Class,
}

type PagePointer = NonNull<Page>;

struct Page {
    heap: Arc<Heap>,
    backing: Mapping,
    payer: std::cell::UnsafeCell<Payer>,
    key: std::cell::UnsafeCell<Key>,
    slots: std::cell::UnsafeCell<Slots>,
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

    fn new(heap: &Arc<Heap>, payer: Payer, key: Key) -> PagePointer {
        let class = key.class;
        let (backing, header) = if class.fits_slab() {
            let (map, start) = Mapping::aligned(SLAB, SLAB, 0, false);
            (map, start)
        } else {
            let (map, payload) = Mapping::aligned(
                class.stride,
                class.align.max(SLAB),
                page_bytes(),
                class.growable,
            );
            // SAFETY: this mapping reserves a full native page before payload.
            (map, unsafe { payload.sub(HEADER_BACK) })
        };
        let page: PagePointer = header.cast();
        let slots = Self::slots(header, class);
        // SAFETY: the header is aligned, in the new mapping, and disjoint from slots.
        unsafe {
            page.as_ptr().write(Self {
                heap: Arc::clone(heap),
                backing,
                payer: payer.into(),
                key: key.into(),
                slots: slots.into(),
            });
        }
        page
    }

    fn slots(header: NonNull<u8>, class: Class) -> Slots {
        let (first, limit) = if class.fits_slab() {
            let offset = (size_of::<Self>() + class.offset()).next_multiple_of(class.align);
            // SAFETY: size class guarantees at least one slot in the mapping.
            (
                unsafe { header.add(offset) },
                (SLAB - offset + class.offset()) / class.stride,
            )
        } else {
            // SAFETY: the sole payload begins exactly HEADER_BACK bytes after the header.
            (unsafe { header.add(HEADER_BACK) }, 1)
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
            page.as_ref()
                .slots
                .get()
                .write(Self::slots(page.cast(), key.class));
        }
    }

    unsafe fn destroy(page: PagePointer) {
        // SAFETY: no live slots or queues reference this page. Read its fields
        // out of the mapping before dropping the owner and unmapping it.
        let page = unsafe { page.as_ptr().read() };
        drop(page.backing);
        drop(page.heap);
    }
}

#[derive(Default)]
pub(crate) struct Heap(Mutex<State>);

struct State {
    closed: bool,
    ready: [[Option<PagePointer>; SLAB_BINS]; 3],
    empty_slabs: Vec<PagePointer>,
    empty_dedicated: VecDeque<PagePointer>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            closed: false,
            ready: [[None; SLAB_BINS]; 3],
            empty_slabs: Vec::new(),
            empty_dedicated: VecDeque::new(),
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

    fn available(&mut self, key: Key) -> Option<PagePointer> {
        let mut current = *self.ready_head(key.class);
        while let Some(page) = current {
            // SAFETY: this heap owns the ready list. Only pages with a live
            // slot are linked here, and their payer and alignment stay fixed.
            unsafe {
                let existing = Page::key(page);
                if existing.payer == key.payer && existing.class.align == key.class.align {
                    return Some(page);
                }
                current = (*page.as_ref().slots.get()).next;
            }
        }
        None
    }

    unsafe fn link(&mut self, page: PagePointer, class: Class) {
        // SAFETY: exclusively held heap; this page was absent from the class queue.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            slots.previous = None;
            slots.next = self.ready_head(class).replace(page);
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
        // Use the smallest reservation that fits so a later large request can
        // take a larger one. The last occupant's logical size is irrelevant.
        let (index, _) = self
            .empty_dedicated
            .iter()
            .enumerate()
            .filter_map(|(index, &page)| {
                // SAFETY: the heap owns these empty pages and their mappings.
                let page = unsafe { page.as_ref() };
                let payload = unsafe { (*page.slots.get()).first };
                let extent = page.backing.extent(payload);
                (extent >= class.stride && payload.addr().get() & (class.align - 1) == 0)
                    .then_some((index, extent))
            })
            .min_by_key(|&(_, extent)| extent)?;
        let page = self.empty_dedicated.swap_remove_back(index).unwrap();
        // SAFETY: the page has been taken off the cache and has no live slots.
        unsafe {
            let header = page.as_ref();
            let payload = (*header.slots.get()).first;
            if header.backing.extend(payload, class.stride) {
                Some(page)
            } else {
                Page::destroy(page);
                None
            }
        }
    }

    fn recycle(&mut self, page: PagePointer, class: Class) -> Option<PagePointer> {
        if class.fits_slab() {
            self.empty_slabs.push(page);
            return None;
        }
        // SAFETY: the heap owns this empty page. The payload starts on a
        // native page boundary, after the header and any ownership prefix.
        let header = unsafe { page.as_ref() };
        let payload = unsafe { (*header.slots.get()).first };
        if !header.backing.discard_payload(payload) {
            return Some(page);
        }
        let evicted = if self.empty_dedicated.len() == EMPTY_DEDICATED_LIMIT {
            self.empty_dedicated.pop_front()
        } else {
            None
        };
        self.empty_dedicated.push_back(page);
        evicted
    }
}

impl Heap {
    pub(crate) fn allocate(
        heap: &Arc<Self>,
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
        let mut state = heap.0.lock().unwrap();
        // SAFETY: this heap owns all queues and the live slot until publication.
        unsafe {
            let ready = if class.fits_slab() {
                state.available(key)
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
            if slots.live == slots.limit {
                if ready.is_some() {
                    state.unlink(page, class);
                }
            } else if ready.is_none() {
                state.link(page, class);
            }
            #[cfg(feature = "memcheck")]
            {
                slots.checked_bytes = bytes;
                allocation_check::allocated(pointer, bytes, kind);
            }
            #[cfg(any(test, feature = "charge-observation"))]
            crate::peak::allocated(pointer.as_ptr(), class.stride);
            pointer
        }
    }

    /// Retire the task's cache; pages with transferred blocks keep their heap
    /// alive and are released by the last free on whichever task owns that block.
    pub(crate) fn close(&self) {
        let mut state = self.0.lock().unwrap();
        state.closed = true;
        state.ready = [[None; SLAB_BINS]; 3];
        let slabs = std::mem::take(&mut state.empty_slabs);
        let dedicated = std::mem::take(&mut state.empty_dedicated);
        drop(state);
        for page in slabs {
            // SAFETY: detached from the cache, with no live slots.
            unsafe { Page::destroy(page) };
        }
        for page in dedicated {
            // SAFETY: detached from the cache, with no live slots.
            unsafe { Page::destroy(page) };
        }
    }
}

/// # Safety
/// A live own or resource block allocated by this heap.
unsafe fn page_of(payload: NonNull<u8>) -> PagePointer {
    let address = payload.as_ptr().addr();
    let base = address & !(SLAB - 1);
    let header = if base == address {
        base - HEADER_BACK
    } else {
        base
    };
    // SAFETY: slot placement and Mapping::aligned reserve this header.
    unsafe { NonNull::new_unchecked(payload.as_ptr().with_addr(header).cast()) }
}

/// # Safety
/// A live block pins its page's original payer until its logical refund.
pub(crate) unsafe fn payer<'a>(payload: NonNull<u8>) -> &'a Payer {
    // SAFETY: the page's payer remains immutable throughout this block's lifetime.
    unsafe { &*page_of(payload).as_ref().payer.get() }
}

/// # Safety
/// A live block whose kind was assigned by the allocator.
pub(crate) unsafe fn kind(payload: NonNull<u8>) -> u8 {
    // SAFETY: exclusively owned slots pin the page metadata.
    unsafe { Page::key(page_of(payload)).class.kind }
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
        if !header.backing.extend(payload, class.stride) {
            return false;
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
pub(crate) unsafe fn release(payload: NonNull<u8>) {
    // SAFETY: the slot pins its header and the page's heap while live.
    let page = unsafe { page_of(payload) };
    // SAFETY: slot pins this immutable heap handle through the lock and state update.
    let heap = unsafe { &page.as_ref().heap };
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::deallocated(payload.as_ptr());
    let mut state = heap.0.lock().unwrap();
    // SAFETY: the heap lock owns the free list and page lifecycle. This slot is
    // writable and never shared after its caller publishes the refund.
    unsafe {
        let key = Page::key(page);
        #[cfg(feature = "memcheck")]
        allocation_check::freed(payload, key.class.kind);
        let slots = &mut *page.as_ref().slots.get();
        let was_full = slots.live == slots.limit;
        slots.live -= 1;
        if state.closed {
            if slots.live == 0 {
                drop(state);
                Page::destroy(page);
            }
        } else if slots.live == 0 {
            if !was_full {
                state.unlink(page, key.class);
            }
            if let Some(retired) = state.recycle(page, key.class) {
                drop(state);
                Page::destroy(retired);
            }
        } else {
            #[cfg(feature = "memcheck")]
            allocation_check::link_access(payload, true);
            payload.cast::<*mut u8>().write(slots.free);
            #[cfg(feature = "memcheck")]
            allocation_check::link_access(payload, false);
            slots.free = payload.as_ptr();
            if was_full {
                state.link(page, key.class);
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
        // payload, then make only the initial extent and header writable.
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

    fn extent(&self, payload: NonNull<u8>) -> usize {
        self.len - (payload.addr().get() - self.base.addr().get())
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

    fn discard_payload(&self, payload: NonNull<u8>) -> bool {
        let bytes = self.writable.get();
        // SAFETY: only empty dedicated mappings reach here, and this range
        // consists of whole anonymous payload pages past the mapping header.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::madvise(payload.as_ptr().cast(), bytes, libc::MADV_DONTNEED) == 0
        }
        #[cfg(not(target_os = "linux"))]
        unsafe {
            libc::mmap(
                payload.as_ptr().cast(),
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
            Ok(bytes) if bytes.is_power_of_two() && (HEADER_BACK..=SLAB).contains(&bytes) => bytes,
            _ => crate::process::stop(b"PageSize"),
        }
    })
}

#[cold]
fn host_memory<T>() -> T {
    crate::process::stop(b"HostMemory")
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

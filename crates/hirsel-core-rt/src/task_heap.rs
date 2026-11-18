//! Fixed-slot pages with one payer and one owner per page. Like mimalloc's
//! page queues, a class keeps only pages that can supply a block. The task
//! owns the queue even if it resumes on a different worker.
use crate::{abi_layout::own_prefix, limit::Payer};
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, OnceLock};

const SLAB: usize = 64 * 1024;
const HEADER_BACK: usize = 4096;
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
}
impl Class {
    fn for_block(bytes: usize, align: usize, kind: u8) -> Self {
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
        }
    }
    fn offset(self) -> usize {
        own_prefix(self.kind).next_multiple_of(self.align)
    }
    fn fits_slab(self) -> bool {
        (self.bin as usize) < SLAB_BINS
    }
    fn large_bin(self) -> usize {
        usize::from(self.bin) - SLAB_BINS
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
    previous: Option<PagePointer>,
    next: Option<PagePointer>,
}

impl Page {
    // SAFETY for all page operations: the mapping and a live block or the
    // heap retain the page. Slot and slot-link changes own the heap mutex;
    // key and payer only change when the page has no live blocks.
    unsafe fn key(page: PagePointer) -> Key {
        unsafe { *page.as_ref().key.get() }
    }

    fn new(heap: &Arc<Heap>, payer: Payer, key: Key) -> PagePointer {
        let class = key.class;
        let (backing, header) = if class.fits_slab() {
            let (map, start) = Mapping::aligned(SLAB, SLAB, 0);
            (map, start)
        } else {
            let (map, payload) =
                Mapping::aligned(class.stride, class.align.max(SLAB), page_bytes());
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
            previous: None,
            next: None,
        }
    }

    unsafe fn reuse(page: PagePointer, payer: Payer, key: Key) {
        // SAFETY: the caller owns the heap mutex and an empty page of matching extent.
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
    empty_large: Vec<Option<PagePointer>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            closed: false,
            ready: [[None; SLAB_BINS]; 3],
            empty_slabs: Vec::new(),
            empty_large: Vec::new(),
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
        let mut link = self.empty_large.get_mut(class.large_bin())?;
        while let Some(page) = *link {
            // SAFETY: the heap owns these empty mappings and their links.
            let slots = unsafe { &mut *page.as_ref().slots.get() };
            // Large mappings have this bin's exact stride and a fixed
            // payload offset. Any mapping with sufficient alignment works.
            if slots.first.addr().get() & (class.align - 1) == 0 {
                *link = slots.next.take();
                return Some(page);
            }
            link = &mut slots.next;
        }
        None
    }

    fn recycle(&mut self, page: PagePointer, class: Class) {
        if class.fits_slab() {
            self.empty_slabs.push(page);
        } else {
            let bin = class.large_bin();
            if bin >= self.empty_large.len() {
                self.empty_large.resize(bin + 1, None);
            }
            let head = self.empty_large[bin].replace(page);
            // SAFETY: this one-slot page is empty and never joins the ready list.
            unsafe { (*page.as_ref().slots.get()).next = head };
        }
    }
}

impl Heap {
    pub(crate) fn allocate(
        heap: &Arc<Self>,
        payer: Payer,
        bytes: usize,
        align: usize,
        kind: u8,
    ) -> NonNull<u8> {
        let class = Class::for_block(bytes, align, kind);
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
            allocation_check::allocated(pointer, bytes, kind);
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
        let large = std::mem::take(&mut state.empty_large);
        drop(state);
        for page in slabs {
            // SAFETY: detached from the cache, with no live slots.
            unsafe { Page::destroy(page) };
        }
        for mut current in large {
            while let Some(page) = current {
                // SAFETY: the detached list contains only empty pages.
                unsafe {
                    current = (*page.as_ref().slots.get()).next;
                    Page::destroy(page);
                }
            }
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
            state.recycle(page, key.class);
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
}
impl Mapping {
    fn aligned(bytes: usize, align: usize, headroom: usize) -> (Self, NonNull<u8>) {
        let page = page_bytes();
        let bytes = bytes
            .checked_next_multiple_of(page)
            .unwrap_or_else(host_memory);
        let len = bytes.checked_add(headroom).unwrap_or_else(host_memory);
        let extra = if align > page { align } else { 0 };
        let total = len.checked_add(extra).unwrap_or_else(host_memory);
        if total > isize::MAX as usize {
            host_memory::<()>();
        }
        #[cfg(test)]
        MAP_REQUESTS.with(|count| count.set(count.get() + 1));
        // SAFETY: new anonymous private writable mapping; no address is replaced.
        let raw = unsafe {
            libc::mmap(
                ptr::null_mut(),
                total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            host_memory::<()>();
        }
        // SAFETY: a successful NULL-hint mapping is non-null. Trimming aligns
        // the payload and leaves only whole switched-out OS pages unassigned.
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
            (Self { base, len }, payload)
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

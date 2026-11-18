//! Fixed-slot pages with one payer and one owner per page. Like mimalloc's
//! page queues, a class keeps only pages that can supply a block. The task
//! owns the queue even if it resumes on a different worker.
use crate::{abi_layout::own_prefix, limit::Payer};
use std::collections::HashMap;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, OnceLock};

const SLAB: usize = 64 * 1024;
const HEADER_BACK: usize = 4096;
const DIRECT_CLASSES: usize = 32;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct Class {
    stride: usize,
    align: usize,
    kind: u8,
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
        }
    }
    fn offset(self) -> usize {
        own_prefix(self.kind).next_multiple_of(self.align)
    }
    fn fits_slab(self) -> bool {
        self.stride <= SLAB / 2
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
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

#[derive(Default)]
struct State {
    closed: bool,
    // A direct page per small class, as in mimalloc's pages_free_direct.
    // The full key also distinguishes alignments and original payers.
    direct: [[Option<PagePointer>; DIRECT_CLASSES]; 3],
    ready: HashMap<Key, PagePointer>,
    empty_slabs: Vec<PagePointer>,
    empty_large: HashMap<Class, Vec<PagePointer>>,
}

// SAFETY: unique live blocks pin their pages and read only immutable funding.
// Queue membership, free lists and page reuse are serialized by the task heap.
unsafe impl Send for State {}

impl State {
    unsafe fn link(&mut self, page: PagePointer, key: Key) {
        // SAFETY: exclusively held heap; this page was absent from the class queue.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            slots.previous = None;
            slots.next = self.ready.insert(key, page);
            if let Some(next) = slots.next {
                (*next.as_ref().slots.get()).previous = Some(page);
            }
        }
    }

    unsafe fn unlink(&mut self, page: PagePointer, key: Key) {
        // SAFETY: exclusively held heap; this page belongs to the class queue.
        unsafe {
            let slots = &mut *page.as_ref().slots.get();
            let previous = slots.previous.take();
            if let Some(previous) = previous {
                (*previous.as_ref().slots.get()).next = slots.next;
            } else if let Some(next) = slots.next {
                *self.ready.get_mut(&key).unwrap() = next;
            } else {
                self.ready.remove(&key);
            }
            if let Some(next) = slots.next.take() {
                (*next.as_ref().slots.get()).previous = previous;
            }
        }
        if let Some(slot) = self.direct_slot(key.class)
            && *slot == Some(page)
        {
            *slot = None;
        }
    }

    fn direct_slot(&mut self, class: Class) -> Option<&mut Option<PagePointer>> {
        self.direct[usize::from(class.kind)].get_mut(class.stride / 8 - 1)
    }

    fn cached(&mut self, class: Class) -> Option<PagePointer> {
        if class.fits_slab() {
            self.empty_slabs.pop()
        } else {
            self.empty_large.get_mut(&class)?.pop()
        }
    }

    fn recycle(&mut self, page: PagePointer, class: Class) {
        if class.fits_slab() {
            self.empty_slabs.push(page);
        } else {
            self.empty_large.entry(class).or_default().push(page);
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
            let direct = state.direct_slot(class).and_then(|slot| *slot);
            let page = if let Some(page) = direct.filter(|page| Page::key(*page) == key) {
                page
            } else if let Some(&page) = state.ready.get(&key) {
                page
            } else {
                let page = if let Some(page) = state.cached(class) {
                    Page::reuse(page, payer, key);
                    page
                } else {
                    Page::new(heap, payer, key)
                };
                state.link(page, key);
                page
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
                state.unlink(page, key);
            } else if let Some(slot) = state.direct_slot(class) {
                *slot = Some(page);
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
        state.direct = Default::default();
        state.ready.clear();
        let slabs = std::mem::take(&mut state.empty_slabs);
        let large = std::mem::take(&mut state.empty_large);
        drop(state);
        for page in slabs.into_iter().chain(large.into_values().flatten()) {
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
                state.unlink(page, key);
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
                state.link(page, key);
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

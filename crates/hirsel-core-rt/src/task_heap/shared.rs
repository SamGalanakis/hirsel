//! A worker shard owns each shared page. A live region leases its address
//! until every block and in-flight return leaves. Its heap owns slot edits,
//! even after its task migrates. A region can be handed to a different payer
//! only after all of its storage has been cleared.

use super::{Class, Heap, Locked, Mapping, PageHeader, SLAB, own_prefix, page_bytes};
use crate::limit::Payer;
use std::cell::{Cell, UnsafeCell};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub(super) const MAX_STRIDE: usize = 512;
const FIRST_REGION: usize = 256;
const WORD: usize = 8;
const SHARD_COUNT: usize = 16;
const UNLISTED: usize = usize::MAX;

static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);
static SHARDS: [Mutex<Shard>; SHARD_COUNT] = [const {
    Mutex::new(Shard {
        pools: [const { Pages::new() }; 2],
    })
}; SHARD_COUNT];
static SPARES: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static SHARD: Cell<Option<usize>> = const { Cell::new(None) };
}

fn local_shard() -> usize {
    SHARD.with(|local| {
        if let Some(owner) = local.get() {
            owner
        } else {
            let owner = NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % SHARD_COUNT;
            local.set(Some(owner));
            owner
        }
    })
}

struct Live {
    payer: UnsafeCell<Payer>,
    heap: Arc<Heap>,
    kind: u8,
    slots: UnsafeCell<Slots>,
}

struct Slots {
    occupied: u64,
    words: [u8; MAX_STRIDE / WORD],
}

fn mask(start: usize, words: usize) -> u64 {
    (u64::MAX >> (u64::BITS as usize - words)) << start
}

impl Slots {
    fn take(&mut self, capacity: usize, words: usize) -> Option<usize> {
        let mut free = !self.occupied & mask(0, capacity);
        while free != 0 {
            let start = free.trailing_zeros() as usize;
            let run = (free >> start).trailing_ones() as usize;
            if run >= words {
                self.occupied |= mask(start, words);
                self.words[start] = words as u8;
                return Some(start);
            }
            if start + run == u64::BITS as usize {
                break;
            }
            free &= u64::MAX << (start + run);
        }
        None
    }
}

union Descriptor {
    live: ManuallyDrop<Live>,
    next: usize,
}

#[repr(C)]
struct SharedPage {
    header: PageHeader,
    mapping: Mapping,
    owner: usize,
    width: usize,
    regions: Box<[UnsafeCell<MaybeUninit<Descriptor>>]>,
    placement: UnsafeCell<Placement>,
}

struct Placement {
    live: usize,
    bump: usize,
    free: usize,
    /// Index in the owner's partial list. Full and spare pages are unlisted.
    partial_index: usize,
}

impl SharedPage {
    const DATA: usize = size_of::<Self>().next_multiple_of(WORD);

    fn new(owner: usize, width: usize) -> NonNull<Self> {
        let native = page_bytes();
        let (mapping, base) = Mapping::aligned(native, SLAB, 0, false);
        let regions = (0..(native - Self::DATA) / width)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();
        let page = base.cast::<Self>();
        // SAFETY: the aligned mapping has room for the header and all regions.
        // A pool cache can supply this backing, so clear the initial bytes too.
        unsafe {
            base.add(Self::DATA).write_bytes(0, native - Self::DATA);
            page.write(Self {
                header: PageHeader::Shared,
                mapping,
                owner,
                width,
                regions,
                placement: UnsafeCell::new(Placement {
                    live: 0,
                    bump: 0,
                    free: UNLISTED,
                    partial_index: UNLISTED,
                }),
            });
        }
        page
    }

    fn descriptor(&self, index: usize) -> *mut Descriptor {
        self.regions[index].get().cast()
    }

    /// # Safety
    /// The owner has unlisted the page and consumed every region lease.
    unsafe fn destroy(page: NonNull<Self>) {
        // SAFETY: move out the mapping owner before dropping it and unmapping the header.
        drop(unsafe { page.read() });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    page: NonNull<SharedPage>,
    index: usize,
}

impl Region {
    /// # Safety
    /// The page header belongs to this live shared payload.
    #[inline]
    pub(super) unsafe fn at(header: NonNull<PageHeader>, payload: NonNull<u8>) -> Self {
        let page = header.cast::<SharedPage>();
        // SAFETY: the live payload pins the region and page header.
        let width = unsafe { page.as_ref() }.width;
        let index = (payload.addr().get() - header.addr().get() - SharedPage::DATA)
            >> width.trailing_zeros();
        Self { page, index }
    }

    /// # Safety
    /// The block or heap cache keeps the region leased for the borrow.
    #[inline(always)]
    unsafe fn live<'a>(self) -> &'a Live {
        // SAFETY: the region stays assigned and its descriptor stays initialized.
        unsafe { &(*self.page.as_ref().descriptor(self.index)).live }
    }

    /// # Safety
    /// A live block pins its region and original payer for the borrow.
    #[inline(always)]
    pub(super) unsafe fn funding<'a>(self) -> &'a Payer {
        // SAFETY: the heap can install a payer only when the region has no blocks.
        unsafe { &*self.live().payer.get() }
    }

    /// # Safety
    /// A live block pins the region.
    #[inline]
    pub(super) unsafe fn kind(self) -> u8 {
        // SAFETY: the region's kind is fixed during its lease.
        unsafe { self.live() }.kind
    }

    /// # Safety
    /// The heap lock owns this assigned region.
    unsafe fn take(self, payer: Payer, class: Class) -> Option<NonNull<u8>> {
        // SAFETY: the heap lock owns the slot map and any empty region's payer.
        unsafe {
            let live = self.live();
            let slots = &mut *live.slots.get();
            let page = self.page.as_ref();
            if slots.occupied == 0 {
                live.payer.get().write(payer);
            } else if *live.payer.get() != payer {
                return None;
            }
            let word = slots.take(page.width / WORD, class.stride / WORD)?;
            Some(
                page.mapping
                    .base
                    .add(SharedPage::DATA + self.index * page.width + word * WORD + class.offset()),
            )
        }
    }

    /// # Safety
    /// The heap lock owns this assigned region.
    pub(super) unsafe fn is_empty(self) -> bool {
        // SAFETY: the heap lock owns the slot map.
        unsafe { (*self.live().slots.get()).occupied == 0 }
    }

    /// # Safety
    /// The caller transfers the unique cleaned block after its logical refund.
    pub(super) unsafe fn release(self, payload: NonNull<u8>) {
        // SAFETY: this block leases the region until the heap lock clears its
        // bit. Only a detached region with every bit clear can be returned.
        unsafe {
            let live = self.live();
            let mut heap = live.heap.lock();
            let page = self.page.as_ref();
            let start = payload.sub(own_prefix(live.kind));
            let offset = start.addr().get() - page.mapping.base.addr().get() - SharedPage::DATA;
            let word = (offset & (page.width - 1)) / WORD;
            let slots = &mut *live.slots.get();
            let words = usize::from(slots.words[word]);
            let stride = words * WORD;
            #[cfg(feature = "memcheck")]
            super::allocation_check::storage_access(start, stride, true);
            start.write_bytes(0, stride);
            #[cfg(feature = "memcheck")]
            {
                super::allocation_check::freed(payload, live.kind);
                super::allocation_check::storage_access(start, stride, false);
            }
            slots.occupied &= !mask(word, words);
            heap.release_small(stride);
            if slots.occupied != 0 {
                return;
            }
            let current = &mut heap.small().regions[usize::from(live.kind)];
            if *current == Some(self) {
                if SHARD.try_with(Cell::get).ok().flatten() == Some(page.owner) {
                    heap.0.pending.store(true, Ordering::Relaxed);
                    return;
                }
                *current = None;
            }
            // Keep the detached region's lease across unlock. A different heap
            // cannot obtain it until its page owner consumes that lease.
            drop(heap);
            self.give_back();
        }
    }

    /// # Safety
    /// This region is empty, detached from its heap cache and leased to the caller.
    pub(super) unsafe fn give_back(self) {
        // SAFETY: this unique lease keeps the descriptor alive until we
        // move it out and give the slot back under the original shard lock.
        unsafe {
            let page = self.page.as_ref();
            let live =
                ManuallyDrop::into_inner((&raw const (*page.descriptor(self.index)).live).read());
            SHARDS[page.owner].lock().unwrap().pools[width_index(page.width)].give_back(self);
            drop(live);
        }
    }
}

struct Shard {
    pools: [Pages; 2],
}

// SAFETY: a shard's mutex owns its lists and page lifetimes. Regions stay
// leased while their heap edits blocks or while a return enters the shard.
unsafe impl Send for Shard {}

struct Pages {
    partial: Vec<NonNull<SharedPage>>,
    spare: Option<NonNull<SharedPage>>,
}

impl Pages {
    const fn new() -> Self {
        Self {
            partial: Vec::new(),
            spare: None,
        }
    }

    /// # Safety
    /// The shard lock owns this unlisted page.
    unsafe fn link(&mut self, page: NonNull<SharedPage>) {
        // SAFETY: the shard lock owns the placement record.
        unsafe { (*page.as_ref().placement.get()).partial_index = self.partial.len() };
        self.partial.push(page);
    }

    fn unlink(&mut self, index: usize) {
        self.partial.swap_remove(index);
        if let Some(page) = self.partial.get(index) {
            // SAFETY: the shard lock owns the moved page's placement record.
            unsafe { (*page.as_ref().placement.get()).partial_index = index };
        }
    }

    fn claim(&mut self, owner: usize, width: usize, live: Live) -> Region {
        let pointer = if let Some(&page) = self.partial.last() {
            page
        } else {
            let page = self.spare.take().map_or_else(
                || SharedPage::new(owner, width),
                |page| {
                    SPARES.fetch_sub(1, Ordering::Relaxed);
                    page
                },
            );
            // SAFETY: the shard owns the new or spare page.
            unsafe { self.link(page) };
            page
        };
        // SAFETY: the shard owns page placement and this free descriptor.
        unsafe {
            let page = pointer.as_ref();
            let place = &mut *page.placement.get();
            let index = if place.free == UNLISTED {
                let index = place.bump;
                place.bump += 1;
                index
            } else {
                let index = place.free;
                place.free = (*page.descriptor(index)).next;
                index
            };
            page.descriptor(index).write(Descriptor {
                live: ManuallyDrop::new(live),
            });
            place.live += 1;
            if place.live == page.regions.len() {
                self.unlink(std::mem::replace(&mut place.partial_index, UNLISTED));
            }
            Region {
                page: pointer,
                index,
            }
        }
    }

    /// # Safety
    /// This pool owns the page and consumes the cleared region's unique lease.
    unsafe fn give_back(&mut self, region: Region) {
        // SAFETY: the lease pins the page, and the shard owns its placement.
        unsafe {
            let page = region.page.as_ref();
            let place = &mut *page.placement.get();
            page.descriptor(region.index)
                .write(Descriptor { next: place.free });
            place.free = region.index;
            place.live -= 1;
            if place.live == 0 {
                let index = std::mem::replace(&mut place.partial_index, UNLISTED);
                if index != UNLISTED {
                    self.unlink(index);
                }
                if self.spare.is_none() {
                    self.spare = Some(region.page);
                    SPARES.fetch_add(1, Ordering::Relaxed);
                } else {
                    SharedPage::destroy(region.page);
                }
            } else if place.partial_index == UNLISTED {
                self.link(region.page);
            }
        }
    }
}

fn width_index(width: usize) -> usize {
    usize::from(width > FIRST_REGION)
}

#[cold]
fn claim(heap: &Arc<Heap>, payer: Payer, class: Class) -> Region {
    let owner = local_shard();
    let width = if class.stride <= FIRST_REGION {
        FIRST_REGION
    } else {
        MAX_STRIDE
    };
    SHARDS[owner].lock().unwrap().pools[width_index(width)].claim(
        owner,
        width,
        Live {
            payer: UnsafeCell::new(payer),
            heap: Arc::clone(heap),
            kind: class.kind,
            slots: UnsafeCell::new(Slots {
                occupied: 0,
                words: [0; MAX_STRIDE / WORD],
            }),
        },
    )
}

pub(super) fn allocate(
    heap: &Arc<Heap>,
    state: &mut Locked<'_>,
    payer: Payer,
    class: Class,
    bytes: usize,
) -> NonNull<u8> {
    let current = &mut state.small().regions[usize::from(class.kind)];
    // SAFETY: the allocating heap owns its cached region and the newly leased
    // region until publication. Any displaced live region drains in place.
    let pointer = unsafe {
        if let Some(region) = *current
            && let Some(pointer) = region.take(payer, class)
        {
            pointer
        } else {
            if let Some(previous) = current.take()
                && previous.is_empty()
            {
                previous.give_back();
            }
            let region = claim(heap, payer, class);
            *current = Some(region);
            region.take(payer, class).unwrap()
        }
    };
    #[cfg(feature = "memcheck")]
    super::allocation_check::allocated(pointer, bytes, class.kind);
    #[cfg(not(feature = "memcheck"))]
    let _ = bytes;
    pointer
}

pub(super) fn retaining() -> bool {
    SPARES.load(Ordering::Relaxed) != 0
}

pub(super) fn purge() {
    if !retaining() {
        return;
    }
    for shard in &SHARDS {
        for pool in &mut shard.lock().unwrap().pools {
            if let Some(page) = pool.spare.take() {
                SPARES.fetch_sub(1, Ordering::Relaxed);
                // SAFETY: the shard unlinked the empty page. No lease remains.
                unsafe { SharedPage::destroy(page) };
            }
            if pool.partial.capacity() > 2 * pool.partial.len() {
                pool.partial.shrink_to_fit();
            }
        }
    }
}

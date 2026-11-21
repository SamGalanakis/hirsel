//! A shared slab uses only its first 2 KiB for small nursery regions.
//! Each region holds one nursery descriptor and its blocks. The shard
//! recycles the slab only after every region has returned its lease.

use super::{Mapping, PRIVATE_START, SLAB, page_bytes, slabs};
use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::sync::Mutex;

pub(super) const REGION_BYTES: usize = 512;
pub(super) const MAX_STRIDE: usize = 256;
const SHARDS: usize = 16;
const FREE: u8 = 0b0000_1111;
static POOLS: [Mutex<Pool>; SHARDS] = [const { Mutex::new(Pool(Vec::new())) }; SHARDS];

const _: () = assert!(
    PRIVATE_START / REGION_BYTES == 4
        && PRIVATE_START + size_of::<SharedPage>() <= super::MIN_PAGE_BYTES
        && size_of::<super::Page>() <= REGION_BYTES - MAX_STRIDE
);

struct SharedPage {
    _slab: slabs::Slab,
    shard: usize,
    placement: UnsafeCell<Placement>,
}

struct Placement {
    free: u8,
    index: usize,
}

struct Pool(Vec<NonNull<SharedPage>>);

// SAFETY: the shard mutex owns the list and all placement edits. A region
// lease keeps its shared page alive across workers.
unsafe impl Send for Pool {}

pub(super) struct Region(NonNull<u8>);

impl Region {
    pub(super) fn claim(heap: usize) -> (Self, NonNull<u8>) {
        let shard = (heap ^ (heap >> 12)) / 16 % SHARDS;
        let mut pool = POOLS[shard].lock().unwrap();
        let page = pool.0.last().copied().unwrap_or_else(|| {
            let (slab, base) = slabs::Slab::allocate(heap);
            // SAFETY: the new slab is exclusively owned. Pages after the first
            // can still be resident from the slab's earlier use.
            let page = unsafe {
                let native = page_bytes();
                if native < SLAB {
                    Mapping::discard(base.add(native), SLAB - native);
                }
                base.add(PRIVATE_START).cast::<SharedPage>()
            };
            // SAFETY: the header starts after the last region.
            unsafe {
                page.write(SharedPage {
                    _slab: slab,
                    shard,
                    placement: Placement {
                        free: FREE,
                        index: 0,
                    }
                    .into(),
                });
            }
            pool.0.push(page);
            page
        });
        // SAFETY: the shard owns placement and this region is unleased.
        unsafe {
            let placement = &mut *page.as_ref().placement.get();
            let index = placement.free.trailing_zeros();
            placement.free &= !(1 << index);
            if placement.free == 0 {
                pool.0.pop();
            }
            let region = page
                .cast::<u8>()
                .sub(PRIVATE_START - index as usize * REGION_BYTES);
            #[cfg(feature = "memcheck")]
            super::allocation_check::storage_access(region, REGION_BYTES, true);
            region.write_bytes(0, REGION_BYTES);
            (Self(region), region)
        }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        let address = self.0.addr().get();
        let offset = address & (SLAB - 1);
        // SAFETY: the still-claimed region pins the shared header and slab.
        unsafe {
            let page = self.0.add(PRIVATE_START - offset).cast::<SharedPage>();
            let mut pool = POOLS[page.as_ref().shard].lock().unwrap();
            let placement = &mut *page.as_ref().placement.get();
            let was_full = placement.free == 0;
            let bit = 1 << (offset / REGION_BYTES);
            debug_assert_eq!(placement.free & bit, 0);
            placement.free |= bit;
            if placement.free == FREE {
                if !was_full {
                    pool.0.swap_remove(placement.index);
                    if let Some(moved) = pool.0.get(placement.index) {
                        (*moved.as_ref().placement.get()).index = placement.index;
                    }
                }
                // Move the owner out before the slab can return to its pool.
                drop(page.read());
            } else if was_full {
                placement.index = pool.0.len();
                pool.0.push(page);
            }
        }
    }
}

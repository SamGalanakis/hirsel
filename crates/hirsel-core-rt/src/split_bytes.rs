//! Two independently charged byte ranges in one short task-heap slot.
use crate::{Capacity, Context, block, limit::Payer, task_heap};
use std::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicU8, Ordering},
};

const KEY_BYTES: usize = 16;
const TAIL_BYTES: usize = 160;
const TAIL_OFFSET: usize = KEY_BYTES + size_of::<usize>();
const KEY: u8 = 1;
const TAIL: u8 = 2;
const CLAIMED: u8 = 4;

#[repr(C)]
struct Header {
    tail_payer: UnsafeCell<MaybeUninit<Payer>>,
    owners: AtomicU8,
    back: usize,
}

const _: () = assert!(size_of::<Header>() == crate::abi_layout::SPLIT_BYTES_PREFIX);
const _: () =
    assert!(std::mem::offset_of!(Header, back) + size_of::<usize>() == size_of::<Header>());

pub(crate) fn key(
    context: &Context,
    bytes: usize,
    charge: Option<u64>,
) -> Result<NonNull<u8>, Capacity> {
    let payer = context.payer();
    // Checked allocations quarantine each owner independently.
    #[cfg(any(test, feature = "memcheck"))]
    let split = !crate::allocator::checked::enabled();
    #[cfg(not(any(test, feature = "memcheck")))]
    let split = true;
    if bytes > KEY_BYTES || !split {
        return block::allocate(context, payer, charge, bytes, 8, block::PLAIN);
    }
    let base = block::allocate(
        context,
        payer,
        charge,
        TAIL_OFFSET + TAIL_BYTES,
        8,
        block::SPLIT_BYTES,
    )?;
    // SAFETY: the fresh split slot reserves the header, the key and the tail.
    unsafe {
        header(base).write(Header {
            tail_payer: UnsafeCell::new(MaybeUninit::uninit()),
            owners: AtomicU8::new(KEY),
            back: 0,
        });
        base.add(KEY_BYTES).cast::<usize>().write(TAIL_OFFSET);
    }
    Ok(base)
}

/// # Safety
/// A nonnull key comes from a live, exclusively lent Bytes carrier with
/// nonzero capacity. It owns heap storage or registered lent frame storage.
pub(crate) unsafe fn tail(
    context: &Context,
    key: Option<NonNull<u8>>,
    bytes: usize,
    charge: Option<u64>,
) -> Result<NonNull<u8>, Capacity> {
    let payer = context.payer();
    block::admit_storage(payer, charge)?;
    if bytes <= TAIL_BYTES
        && let Some(key) = key
        && !context.frame_lists.contains(key)
        // SAFETY: the exclusive loan pins the backing. The frame case was excluded.
        && unsafe { task_heap::kind(key) } == block::SPLIT_BYTES
        // SAFETY: both ranges in a split slot carry their displacement.
        && unsafe { back(key) } == 0
    {
        // SAFETY: the key is the first range of a live split slot. The exclusive
        // loan keeps it live and allows one claim. The tail is private until return.
        let head = unsafe { &*header(key) };
        if head.owners.load(Ordering::Relaxed) == KEY {
            // SAFETY: an unclaimed tail has no other user of this payer field.
            unsafe { (*head.tail_payer.get()).write(payer) };
            head.owners.store(KEY | TAIL | CLAIMED, Ordering::Release);
            // SAFETY: the tail begins after the padded key and displacement word.
            let tail = unsafe { key.add(TAIL_OFFSET) };
            unsafe { task_heap::Page::own_raw(tail, charge.unwrap(), block::free) };
            return Ok(tail);
        }
    }
    Ok(block::allocate_guest(
        context,
        payer,
        charge.unwrap(),
        bytes,
        8,
        block::PLAIN,
    ))
}

unsafe fn header(base: NonNull<u8>) -> *mut Header {
    // SAFETY: the split slot has a Header immediately before its first range.
    unsafe { base.as_ptr().cast::<Header>().sub(1) }
}

unsafe fn back(range: NonNull<u8>) -> usize {
    // SAFETY: the last word before either range contains its displacement.
    unsafe { range.cast::<usize>().sub(1).read() }
}

/// # Safety
/// The caller owns a live split range and its exact logical charge. The slot
/// belongs to `page`. If supplied, `context` belongs to the dropping task.
pub(crate) unsafe fn free(
    page: NonNull<task_heap::Page>,
    range: NonNull<u8>,
    charge: u64,
    context: Option<&Context>,
) {
    // SAFETY: the range pins its displacement and the complete slot.
    unsafe { page.as_ref().forget_raw(range) };
    let offset = unsafe { back(range) };
    // SAFETY: both ranges are inside the same slot, with the header before base.
    let base = unsafe { range.sub(offset) };
    let head = unsafe { &*header(base) };
    let owner = if offset == 0 { KEY } else { TAIL };
    // SAFETY: settlement below is the first one for this owner's exact charge.
    let payer = unsafe { payer(page, range) };
    if let Some(ctx) = context {
        payer.settle_in(ctx, charge);
    } else {
        payer.settle(charge);
    }
    // The last owner releases the whole slot. Until this RMW completes, our
    // own bit keeps the header and page live even when the other task drops.
    let live = head.owners.fetch_and(!owner, Ordering::AcqRel) & (KEY | TAIL);
    if live == owner {
        // SAFETY: the last owner has exclusive custody after both settlements.
        unsafe { task_heap::release_page(page, base, context) };
    }
}

/// # Safety
/// The range belongs to this split slot's page. Its outstanding charge pins
/// the original payer.
pub(crate) unsafe fn payer(page: NonNull<task_heap::Page>, range: NonNull<u8>) -> Payer {
    // SAFETY: the range pins its displacement and the complete slot.
    let offset = unsafe { back(range) };
    if offset == 0 {
        // SAFETY: the key is the task-heap payload that carries the slot's payer.
        unsafe { task_heap::Page::funding(page, range) }
    } else {
        // SAFETY: the live tail was initialized before being transferred.
        let head = unsafe { &*header(range.sub(offset)) };
        unsafe { *(*head.tail_payer.get()).assume_init_ref() }
    }
}

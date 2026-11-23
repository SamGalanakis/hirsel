//! One native growth boundary shared by all emitted sequence sites. Physical
//! reuse changes neither full-new admission nor install-before-original-refund.
use crate::{Capacity, Context, block, task_heap};
use std::ptr::NonNull;

/// # Safety
/// Exclusive data and nonzero capacity words; length may be uninitialized.
/// Kind, alignment, moved prefix and both charges are exact FORM operands.
/// Static, fixed, resource and region storage cannot reach this. `stride` is the
/// element's native size, so the old carrier's capacity times `stride` is its
/// native payload. Small byte buffers can use spare room in their slab slot.
/// From the shared threshold upward a dedicated backing may also extend.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn grow_cold(
    context: &Context,
    carrier: *mut u8,
    bytes: usize,
    align: usize,
    stride: u64,
    charge: u64,
    capacity: u64,
    moved: usize,
    refund: u64,
    overflow: bool,
    kind: u8,
) -> Result<NonNull<u8>, Capacity> {
    // SAFETY: this is the capacity word of the caller's live carrier.
    let old_capacity = unsafe { carrier.add(16).cast::<u64>().read() };
    let reuse =
        old_capacity.saturating_mul(stride) >= crate::abi_layout::GROW_CALL_MIN_PAYLOAD_BYTES;
    // SAFETY: forwarded from the caller.
    unsafe {
        replace(
            context, carrier, bytes, align, charge, capacity, moved, refund, overflow, kind, reuse,
        )
    }
}

/// # Safety
/// As for `grow_cold`; `reuse` permits growing the backing in place.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) unsafe fn replace(
    context: &Context,
    carrier: *mut u8,
    bytes: usize,
    align: usize,
    charge: u64,
    capacity: u64,
    moved: usize,
    refund: u64,
    overflow: bool,
    kind: u8,
    reuse: bool,
) -> Result<NonNull<u8>, Capacity> {
    // SAFETY: the consuming carrier owns its data pointer and the live slot.
    let old = unsafe { NonNull::new_unchecked(carrier.cast::<*mut u8>().read()) };
    let payer = context.payer();
    block::admit_storage(payer, (!overflow).then_some(charge))?;
    // SAFETY: the caller owns the only live carrier. In-place storage remains
    // billed to the same payer, with the old charge refunded after installation.
    let heap = context.heap.get_or_init(std::sync::Arc::default);
    let frame_payer = context.frame_lists.remove(old);
    // Memcheck tracks requested extents and lacks per-slot resize metadata.
    #[cfg(not(feature = "memcheck"))]
    if kind == block::PLAIN
        && bytes <= crate::abi_layout::GROW_SLAB_MAX_PAYLOAD_BYTES as usize
        && frame_payer.is_none()
        && let Some(original) =
            unsafe { task_heap::try_reassign_slab(old, payer, bytes, align, kind) }
    {
        unsafe { carrier.add(16).cast::<u64>().write(capacity) };
        original.settle_in(context, refund);
        return Ok(old);
    }
    if reuse
        && frame_payer.is_none()
        && unsafe { task_heap::try_grow_in_place(heap, old, payer, bytes, align, kind) }
    {
        unsafe { carrier.add(16).cast::<u64>().write(capacity) };
        payer.settle_in(context, refund);
        return Ok(old);
    }
    let replacement = task_heap::Heap::allocate(context, payer, bytes, align, kind, reuse);
    // SAFETY: the new slot is disjoint and large enough for the retained
    // elements. Install the carrier before publishing the old payer's refund.
    unsafe {
        std::ptr::copy_nonoverlapping(old.as_ptr(), replacement.as_ptr(), moved);
        carrier.cast::<*mut u8>().write(replacement.as_ptr());
        carrier.add(16).cast::<u64>().write(capacity);
        if let Some(payer) = frame_payer {
            payer.settle_in(context, refund);
        } else {
            block::free_in(context, old, refund);
        }
    }
    Ok(replacement)
}

//! One native growth boundary shared by all emitted sequence sites. Physical
//! reuse changes neither full-new admission nor install-before-original-refund.
use crate::{Capacity, Context, block};
use std::ptr::NonNull;

/// # Safety
/// Exclusive live nonzero-capacity sequence carrier; kind, alignment, moved
/// prefix and both charges are the exact FORM operands. Fixed/resource/region
/// storage and static data cannot reach this operation.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn grow(
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
) -> Result<NonNull<u8>, Capacity> {
    // SAFETY: the consuming carrier owns its data pointer and the live slot.
    let old = unsafe { NonNull::new_unchecked(carrier.cast::<*mut u8>().read()) };
    let replacement = block::allocate(
        context,
        context.payer(),
        (!overflow).then_some(charge),
        bytes,
        align,
        kind,
    )?;
    // SAFETY: the new slot is disjoint and large enough for the retained
    // elements. Install the carrier before publishing the old payer's refund.
    unsafe {
        std::ptr::copy_nonoverlapping(old.as_ptr(), replacement.as_ptr(), moved);
        carrier.cast::<*mut u8>().write(replacement.as_ptr());
        carrier.add(16).cast::<u64>().write(capacity);
        block::free_in(context, old, refund);
    }
    Ok(replacement)
}

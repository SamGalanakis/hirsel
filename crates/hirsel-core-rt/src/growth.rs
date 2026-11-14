//! One native growth boundary shared by all emitted sequence sites. Physical
//! reuse changes neither full-new admission nor install-before-original-refund.
use crate::{Capacity, Context, abi_layout::*, allocator, block, limit::Payer};
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
    // SAFETY: the consuming carrier owns its data pointer and the native prefix.
    let old = unsafe { NonNull::new_unchecked(carrier.cast::<*mut u8>().read()) };
    // SAFETY: the live old block keeps its initialized payer word until consumed.
    let original = unsafe { *block::funding(old) };
    let current = context.limit_record();
    let alignment = align.max(8);
    let tag =
        (((alignment.trailing_zeros() - 3) as usize) << OWN_ALIGNMENT_SHIFT) | usize::from(kind);
    if align <= 16
        && charge != 0
        && refund != 0
        && original.native_tag() == tag
        && original.record_pointer() == current
    {
        let payer = Payer::from_current(current);
        payer.admit((!overflow).then_some(charge))?;
        let prefix = if kind == NATIVE_PLAIN {
            PLAIN_PREFIX_BYTES
        } else {
            GRAPH_PREFIX_BYTES
        };
        let offset = (prefix + alignment - 1) & !(alignment - 1);
        let payload = bytes.max(1);
        if payload > ((isize::MAX as usize) & !(alignment - 1)) - offset {
            crate::process::stop(b"HostMemory");
        }
        // SAFETY: admission is complete; the exact original tag proves the base
        // and ordinary alignment. A refusal stops without touching old/carrier.
        let base =
            unsafe { allocator::reallocate_ordinary(old.as_ptr().sub(offset), payload + offset) };
        // SAFETY: the replacement owns the complete prefix and moved elements.
        let replacement = unsafe {
            let pointer = NonNull::new_unchecked(base.as_ptr().add(offset));
            pointer
                .as_ptr()
                .sub(OWN_PAYER_BACK)
                .cast::<Payer>()
                .write(original);
            carrier.cast::<*mut u8>().write(pointer.as_ptr());
            carrier.add(16).cast::<u64>().write(capacity);
            pointer
        };
        payer.settle_in(context, refund);
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::sample();
        Ok(replacement)
    } else {
        let replacement = block::allocate_like(
            context,
            Some(&original),
            (!overflow).then_some(charge),
            bytes,
            align,
            kind,
        )?;
        // SAFETY: independently allocated storage is disjoint and holds the
        // completed prefix. Commit precedes any foreign refund/custody event.
        unsafe {
            std::ptr::copy_nonoverlapping(old.as_ptr(), replacement.as_ptr(), moved);
            carrier.cast::<*mut u8>().write(replacement.as_ptr());
            carrier.add(16).cast::<u64>().write(capacity);
            block::free(old, refund);
        }
        Ok(replacement)
    }
}

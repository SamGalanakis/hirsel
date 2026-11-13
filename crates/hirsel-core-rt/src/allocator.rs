//! Keep the allocator's native free-list path for ordinary alignments. The
//! mimalloc backend bypasses the Rust wrapper's always-aligned entry point.
use std::alloc::{Layout, handle_alloc_error};
use std::ptr::NonNull;
#[inline]
pub(crate) fn allocate(layout: Layout) -> NonNull<u8> {
    // SAFETY: positive Layout, with the corresponding backend retained until free.
    let pointer = unsafe {
        #[cfg(feature = "mimalloc")]
        {
            if layout.align() <= 16 {
                libmimalloc_sys::mi_malloc(layout.size()).cast()
            } else {
                libmimalloc_sys::mi_malloc_aligned(layout.size(), layout.align()).cast()
            }
        }
        #[cfg(not(feature = "mimalloc"))]
        {
            std::alloc::alloc(layout)
        }
    };
    NonNull::new(pointer).unwrap_or_else(|| handle_alloc_error(layout))
}
/// # Safety
/// Matching live allocation and original Layout from this backend.
#[inline]
pub(crate) unsafe fn deallocate(pointer: *mut u8, layout: Layout) {
    // SAFETY: the caller transfers the matching uniquely owned allocation.
    unsafe {
        #[cfg(feature = "mimalloc")]
        {
            let _ = layout;
            libmimalloc_sys::mi_free(pointer.cast());
        }
        #[cfg(not(feature = "mimalloc"))]
        {
            std::alloc::dealloc(pointer, layout);
        }
    }
}

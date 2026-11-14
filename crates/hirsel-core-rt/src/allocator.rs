//! Keep the allocator's native free-list path for ordinary alignments. The
//! mimalloc backend bypasses the Rust wrapper's always-aligned entry point.
use std::alloc::Layout;
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
            if layout.align() <= 16 {
                libc::malloc(layout.size()).cast()
            } else {
                let mut p = std::ptr::null_mut();
                if libc::posix_memalign(&mut p, layout.align(), layout.size()) != 0 {
                    std::ptr::null_mut()
                } else {
                    p.cast()
                }
            }
        }
    };
    let pointer = NonNull::new(pointer).unwrap_or_else(|| crate::process::stop(b"HostMemory"));
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::allocated(pointer.as_ptr(), layout.size());
    pointer
}
/// # Safety
/// Matching live allocation and original Layout from this backend.
#[inline]
pub(crate) unsafe fn deallocate(pointer: *mut u8, _layout: Layout) {
    // SAFETY: this base came from the matching native allocator.
    unsafe {
        deallocate_raw(pointer);
    }
}
/// # Safety
/// Original native base, exclusively owned, allocated by this backend.
#[inline]
pub(crate) unsafe fn deallocate_raw(pointer: *mut u8) {
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::deallocated(pointer);
    // SAFETY: malloc and posix_memalign both pair with free; mimalloc with mi_free.
    unsafe {
        #[cfg(feature = "mimalloc")]
        {
            libmimalloc_sys::mi_free(pointer.cast());
        }
        #[cfg(not(feature = "mimalloc"))]
        {
            libc::free(pointer.cast());
        }
    }
}

#[cfg(test)]
#[path = "allocator_stop_tests.rs"]
mod stop_tests;

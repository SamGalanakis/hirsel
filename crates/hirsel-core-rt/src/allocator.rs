//! Keep the allocator's native free-list path for ordinary alignments. The
//! mimalloc backend bypasses the Rust wrapper's always-aligned entry point.
use std::alloc::Layout;
use std::ptr::NonNull;
#[inline]
pub(crate) fn allocate_raw(layout: Layout) -> NonNull<u8> {
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
    #[cfg(feature = "allocation-count")]
    crate::allocation_count::allocated();
    pointer
}
/// # Safety
/// Original exclusively owned base from allocate_raw.
#[inline]
pub(crate) unsafe fn deallocate_untracked(pointer: *mut u8) {
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

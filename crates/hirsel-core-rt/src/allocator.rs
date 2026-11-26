//! Keep the allocator's native free-list path for ordinary alignments. The
//! mimalloc backend bypasses the Rust wrapper's always-aligned entry point.
use crate::run_stop::Stop;
use std::alloc::Layout;
use std::ptr::NonNull;

#[cfg(any(test, feature = "memcheck"))]
pub(crate) mod checked;

/// Fallible raw allocation that checks writable fiber stack room before
/// entering the C allocator and returns `Err(Stop)` instead of diverging
/// (so callers holding locks drop their guards before raising).
#[inline]
pub fn try_allocate_raw(layout: Layout) -> Result<NonNull<u8>, Stop> {
    crate::fiber::room(crate::fiber::RUNTIME_ROOM)?;
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
    let pointer = NonNull::new(pointer).ok_or_else(|| Stop::from_static(b"HostMemory"))?;
    #[cfg(any(test, feature = "memcheck"))]
    checked::allocated(pointer, layout.size());
    #[cfg(feature = "allocation-count")]
    crate::allocation_count::allocated();
    Ok(pointer)
}

#[inline]
pub(crate) fn allocate_raw(layout: Layout) -> NonNull<u8> {
    try_allocate_raw(layout).unwrap_or_else(|stop| crate::run_stop::raise(stop))
}

/// # Safety
/// A live pointer must be the original exclusively owned base from
/// allocate_raw. Checked mode detects an address that is no longer live.
#[inline]
pub(crate) unsafe fn deallocate_raw(pointer: NonNull<u8>) {
    if let Err(stop) = crate::fiber::room(crate::fiber::RUNTIME_ROOM) {
        crate::run_stop::raise(stop);
    }
    #[cfg(any(test, feature = "memcheck"))]
    if checked::enabled() {
        // SAFETY: the caller transfers raw allocator storage.
        unsafe { checked::retire(pointer, checked::Storage::Raw) };
        return;
    }
    // SAFETY: the caller transfers a live allocation from this backend.
    unsafe { deallocate_untracked(pointer) };
}

/// # Safety
/// Original base from allocate_raw. The caller owns the physical storage.
unsafe fn deallocate_untracked(pointer: NonNull<u8>) {
    // SAFETY: malloc and posix_memalign both pair with free; mimalloc with mi_free.
    unsafe {
        #[cfg(feature = "mimalloc")]
        {
            libmimalloc_sys::mi_free(pointer.as_ptr().cast());
        }
        #[cfg(not(feature = "mimalloc"))]
        {
            libc::free(pointer.as_ptr().cast());
        }
    }
}

#[cfg(test)]
#[path = "allocator_stop_tests.rs"]
mod stop_tests;

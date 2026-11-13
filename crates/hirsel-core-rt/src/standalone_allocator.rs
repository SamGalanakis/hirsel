//! The standalone archive owns Rust bookkeeping allocation as well as payloads.
//! Embeddings, guarded tests and allocator comparisons do not select this feature.
//! Payload malloc/mi_malloc calls bypass this wrapper and retain their fast path.
use std::alloc::{GlobalAlloc, Layout, System};

struct Standalone;

#[global_allocator]
static ALLOCATOR: Standalone = Standalone;

#[inline(always)]
fn required(pointer: *mut u8) -> *mut u8 {
    if pointer.is_null() {
        crate::process::stop(b"HostMemory");
    }
    pointer
}

// SAFETY: System owns every allocation and matching layout; null ends the run
// without allocating, unwinding, releasing owners or invoking Rust's OOM handler.
unsafe impl GlobalAlloc for Standalone {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid positive allocation layout.
        required(unsafe { System.alloc(layout) })
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: System supplies the caller's valid layout and zeroed storage.
        required(unsafe { System.alloc_zeroed(layout) })
    }

    #[inline]
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller supplies System's live base/layout and valid new size.
        required(unsafe { System.realloc(pointer, layout, size) })
    }

    #[inline]
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: matching live System allocation and original layout.
        unsafe { System.dealloc(pointer, layout) }
    }
}

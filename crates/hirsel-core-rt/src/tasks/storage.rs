use super::{Task, TaskLayout, ValueLayout};
use std::{
    alloc::{Layout, alloc, dealloc},
    ptr::NonNull,
};

// Keep one small block per scheduler. Large or unusually aligned tasks must
// release their backing if the combined layout exceeds this bound.
pub(super) const MAX_SPARE_TASK_BYTES: usize = 4096;

/// Host bookkeeping storage, whose logical charge was already admitted whole.
pub(crate) struct Storage {
    pub(super) pointer: NonNull<u8>,
    pub(super) layout: Layout,
}
impl Storage {
    pub(crate) fn new(bytes: usize, align: usize) -> Self {
        let layout = Layout::from_size_align(bytes, align)
            .unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
        Self::with_layout(layout)
    }
    fn with_layout(layout: Layout) -> Self {
        let bytes = layout.size();
        let pointer = if bytes == 0 {
            layout.dangling_ptr()
        } else {
            // SAFETY: alloc receives a valid nonzero layout.
            unsafe { NonNull::new(alloc(layout)) }
                .unwrap_or_else(|| crate::process::stop(b"HostMemory"))
        };
        #[cfg(any(test, feature = "charge-observation", feature = "prof"))]
        if bytes != 0 {
            crate::backing_observation::observe!(allocate, pointer.as_ptr(), bytes, TaskStorage);
        }
        Self { pointer, layout }
    }
    pub(crate) fn as_ptr(&self) -> *mut u8 {
        self.pointer.as_ptr()
    }

    pub(super) fn for_task(
        spare: &mut Option<Self>,
        payload: TaskLayout,
    ) -> (Self, *mut u8, *mut u8) {
        fn extend(base: Layout, part: ValueLayout) -> (Layout, Option<usize>) {
            let part = Layout::from_size_align(part.bytes, part.align)
                .unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
            if part.size() == 0 {
                return (base, None);
            }
            let (combined, offset) = base
                .extend(part)
                .unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
            (combined, Some(offset))
        }

        let (layout, input) = extend(Layout::new::<Task>(), payload.input);
        let (layout, result) = extend(layout, payload.result);
        let storage = if spare.as_ref().is_some_and(|slot| slot.layout == layout) {
            spare.take().unwrap()
        } else {
            Self::with_layout(layout)
        };
        let at = |offset: Option<usize>, part: ValueLayout| match offset {
            // SAFETY: extend placed this nonempty payload inside the allocation.
            Some(offset) => unsafe { storage.as_ptr().add(offset) },
            None => std::ptr::without_provenance_mut(part.align),
        };
        let input = at(input, payload.input);
        let result = at(result, payload.result);
        (storage, input, result)
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        // SAFETY: sole owner of this host storage; payload is already moved/cleaned.
        unsafe {
            if self.layout.size() != 0 {
                crate::backing_observation::observe!(
                    free,
                    self.pointer.as_ptr(),
                    self.layout.size(),
                );
                dealloc(self.pointer.as_ptr(), self.layout);
            }
        }
    }
}

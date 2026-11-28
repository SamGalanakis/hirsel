use super::{Body, Context, Task};
use crate::code::ExecutionPin;
use std::{
    alloc::{Layout, alloc, dealloc},
    ptr::NonNull,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ValueLayout {
    pub bytes: usize,
    pub align: usize,
    pub logical: u64,
    /// The compiler's resumable cleanup of one value in place, run on the
    /// active drain (counted::Recipe). None for a task input, which the child
    /// body always cleans itself.
    pub clean: Option<crate::counted::Recipe>,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TaskLayout {
    pub input: ValueLayout,
    pub result: ValueLayout,
}

impl TaskLayout {
    pub(super) fn pin_recipes(&self, context: &Context, body: Body) -> Box<[ExecutionPin]> {
        [
            ExecutionPin::pin_pc(Some(context), body as usize),
            ExecutionPin::recipe(context, self.input.clean),
            ExecutionPin::recipe(context, self.result.clean),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

// Keep one small block per scheduler. Large or unusually aligned tasks must
// release their backing if the combined layout exceeds this bound.
pub(super) const MAX_SPARE_TASK_BYTES: usize = 4096;

/// Host bookkeeping storage, whose logical charge was already admitted whole.
pub(crate) struct Storage {
    pub(super) pointer: NonNull<u8>,
    pub(super) layout: Layout,
}
impl Storage {
    pub(crate) fn try_new(bytes: usize, align: usize) -> Result<Self, crate::run_stop::Stop> {
        let layout = Layout::from_size_align(bytes, align)
            .map_err(|_| crate::run_stop::Stop::from_static(b"HostMemory"))?;
        Self::try_with_layout(layout)
    }
    pub(crate) fn new(bytes: usize, align: usize) -> Self {
        Self::try_new(bytes, align).unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }
    fn try_with_layout(layout: Layout) -> Result<Self, crate::run_stop::Stop> {
        crate::fiber::room(crate::fiber::RUNTIME_ROOM)?;
        let bytes = layout.size();
        let pointer = if bytes == 0 {
            layout.dangling_ptr()
        } else {
            // SAFETY: alloc receives a valid nonzero layout.
            unsafe { NonNull::new(alloc(layout)) }
                .ok_or_else(|| crate::run_stop::Stop::from_static(b"HostMemory"))?
        };
        #[cfg(any(test, feature = "charge-observation", feature = "prof"))]
        if bytes != 0 {
            crate::backing_observation::observe!(allocate, pointer.as_ptr(), bytes, TaskStorage);
        }
        Ok(Self { pointer, layout })
    }
    fn with_layout(layout: Layout) -> Self {
        Self::try_with_layout(layout).unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }
    pub(crate) fn as_ptr(&self) -> *mut u8 {
        self.pointer.as_ptr()
    }

    pub(super) fn for_task(
        spare: &mut Option<Self>,
        payload: TaskLayout,
    ) -> (Self, *mut u8, *mut u8) {
        fn extend(base: Layout, part: ValueLayout) -> (Layout, Option<usize>) {
            let part = Layout::from_size_align(part.bytes, part.align).unwrap_or_else(|_| {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostMemory"))
            });
            if part.size() == 0 {
                return (base, None);
            }
            let (combined, offset) = base.extend(part).unwrap_or_else(|_| {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostMemory"))
            });
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

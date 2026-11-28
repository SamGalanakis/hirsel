//! Guarded unit-stack and root-task-stack pair backing for untied pool units.
use crate::fiber::Stack;
use std::ptr::NonNull;

pub(crate) struct UnitStack {
    base: NonNull<u8>,
    pub(super) len: usize,
}

// SAFETY: ownership of the mapping moves with the stack. A worker gives it
// back only after switching to its host stack for the last time.
unsafe impl Send for UnitStack {}

impl UnitStack {
    pub(super) fn new(usable_bytes: usize) -> Self {
        let guard = crate::task_heap::page_bytes();
        let len = usable_bytes + guard;
        // SAFETY: allocate a private stack and make its lowest page inaccessible.
        unsafe {
            let raw = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if raw == libc::MAP_FAILED || libc::mprotect(raw, guard, libc::PROT_NONE) != 0 {
                if raw != libc::MAP_FAILED {
                    libc::munmap(raw, len);
                }
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitStack"));
            }
            Self {
                base: NonNull::new_unchecked(raw.cast()),
                len,
            }
        }
    }

    pub(super) fn new_batch(count: usize, usable_bytes: usize) -> Vec<Self> {
        if count <= 1 {
            return (0..count).map(|_| Self::new(usable_bytes)).collect();
        }
        let guard = crate::task_heap::page_bytes();
        let len = usable_bytes + guard;
        let Some(total) = len.checked_mul(count) else {
            return (0..count).map(|_| Self::new(usable_bytes)).collect();
        };
        // SAFETY: one contiguous private mapping partitioned into `count`
        // independent unit stacks, each with its lowest page protected
        // PROT_NONE, unmapped individually on Drop.
        unsafe {
            let raw = libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if raw == libc::MAP_FAILED {
                return (0..count).map(|_| Self::new(usable_bytes)).collect();
            }
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let base = raw.cast::<u8>().add(i * len);
                if libc::mprotect(base.cast(), guard, libc::PROT_NONE) != 0 {
                    libc::munmap(base.cast(), (count - i) * len);
                    crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitStack"));
                }
                out.push(Self {
                    base: NonNull::new_unchecked(base),
                    len,
                });
            }
            out
        }
    }

    pub(super) fn top(&self) -> *mut u8 {
        // SAFETY: the end address is within one mapping, one past its last byte.
        unsafe { self.base.as_ptr().add(self.len) }
    }

    pub(crate) fn sanitizer_bounds(&self) -> hirsel_stack::sanitizer::StackBounds {
        let guard = crate::task_heap::page_bytes();
        // SAFETY: `guard` lies within this unit stack's mapping.
        let bottom = unsafe { self.base.as_ptr().add(guard) };
        hirsel_stack::sanitizer::StackBounds::from_raw(bottom, self.len - guard - 16)
    }

    /// Write an initial entry frame at the top of this unit stack and return its `sp`.
    /// # Safety
    /// The caller exclusively owns this idle stack.
    pub(crate) unsafe fn prepare_frame(&self, entry: hirsel_stack::Entry, arg: *mut u8) -> *mut u8 {
        let words = hirsel_stack::initial_frame(entry, arg, std::ptr::null_mut());
        // SAFETY: the top of the unit stack is 16-aligned and writable.
        unsafe {
            let (bottom, size) = self.sanitizer_bounds().into_raw();
            hirsel_stack::sanitizer::asan_unpoison(bottom, size);
            let start = self.top().sub(16 + size_of_val(&words));
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), start, size_of_val(&words));
            start
        }
    }

    #[cfg(test)]
    pub(super) fn guard_bounds(&self) -> (usize, usize) {
        let base = self.base.as_ptr().addr();
        (base, base + crate::task_heap::page_bytes())
    }
}

impl Drop for UnitStack {
    fn drop(&mut self) {
        // SAFETY: the stack owns this mapping and nobody is running on it.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }
}

/// A guarded unit stack paired with a guarded, trimmed root task stack.
pub(crate) struct StackPair {
    pub(crate) unit: UnitStack,
    pub(crate) root: Stack,
}

impl StackPair {
    pub(crate) fn new(unit_stack_bytes: usize) -> Self {
        Self {
            unit: UnitStack::new(unit_stack_bytes),
            root: Stack::new(),
        }
    }

    pub(super) fn new_batch(count: usize, unit_stack_bytes: usize) -> Vec<Self> {
        if count == 0 {
            return Vec::new();
        }
        let units = UnitStack::new_batch(count, unit_stack_bytes);
        let roots = Stack::new_batch(count);
        units
            .into_iter()
            .zip(roots)
            .map(|(unit, root)| Self { unit, root })
            .collect()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.unit.len + self.root.committed_bytes()
    }
}

/// The guarded stacks admitted to a live unit, charged to that unit until it
/// ends and returns or releases them.
pub(crate) struct AdmittedStacks {
    pub(crate) unit: Option<UnitStack>,
    pub(crate) root: Option<Stack>,
    pub(crate) charged_bytes: usize,
}

impl AdmittedStacks {
    pub(crate) const EMPTY: Self = Self {
        unit: None,
        root: None,
        charged_bytes: 0,
    };

    pub(crate) fn take(&mut self) -> Self {
        std::mem::replace(self, Self::EMPTY)
    }

    pub(crate) unsafe fn switch_in(
        &self,
        sanitizer: &hirsel_stack::sanitizer::FiberState,
        host: *mut *mut u8,
        sp: *mut u8,
    ) {
        // SAFETY: the unit stack is mounted and pinned while the worker runs it.
        unsafe {
            sanitizer.switch_in(host, sp, self.unit.as_ref().unwrap().sanitizer_bounds());
        }
    }
}

//! AddressSanitizer and ThreadSanitizer annotations for stackful fiber switches.
//!
//! Normal builds compile every type here to a zero-sized value and every hook
//! to an inline no-op. Sanitizer builds enable `sanitizer-asan` or
//! `sanitizer-tsan` so ASan tracks active stack bounds/fake stacks and TSan
//! tracks per-fiber happens-before clocks across worker migrations.

use std::cell::Cell;
#[cfg(any(feature = "sanitizer-asan", feature = "sanitizer-tsan"))]
use std::ffi::c_void;

#[cfg(feature = "sanitizer-asan")]
unsafe extern "C" {
    fn __sanitizer_start_switch_fiber(
        fake_stack_save: *mut *mut c_void,
        bottom: *const c_void,
        size: usize,
    );
    fn __sanitizer_finish_switch_fiber(
        fake_stack_save: *mut c_void,
        bottom_old: *mut *const c_void,
        size_old: *mut usize,
    );
    fn __asan_unpoison_memory_region(addr: *const c_void, size: usize);
}

#[cfg(feature = "sanitizer-tsan")]
unsafe extern "C" {
    fn __tsan_get_current_fiber() -> *mut c_void;
    fn __tsan_create_fiber(flags: u32) -> *mut c_void;
    fn __tsan_destroy_fiber(fiber: *mut c_void);
    fn __tsan_switch_to_fiber(fiber: *mut c_void, flags: u32);
}

/// Stack `[bottom, bottom + size)` reported to ASan across a fiber switch.
#[derive(Clone, Copy)]
pub struct StackBounds {
    #[cfg(feature = "sanitizer-asan")]
    bottom: *const c_void,
    #[cfg(feature = "sanitizer-asan")]
    size: usize,
}

// SAFETY: StackBounds is an immutable address/size descriptor transferred with fiber custody.
unsafe impl Send for StackBounds {}
unsafe impl Sync for StackBounds {}

impl StackBounds {
    pub const EMPTY: Self = Self {
        #[cfg(feature = "sanitizer-asan")]
        bottom: std::ptr::null(),
        #[cfg(feature = "sanitizer-asan")]
        size: 0,
    };

    #[inline(always)]
    pub const fn from_raw(bottom: *const u8, size: usize) -> Self {
        #[cfg(feature = "sanitizer-asan")]
        {
            Self {
                bottom: bottom.cast(),
                size,
            }
        }
        #[cfg(not(feature = "sanitizer-asan"))]
        {
            let _ = (bottom, size);
            Self {}
        }
    }

    #[inline(always)]
    pub fn into_raw(self) -> (*const u8, usize) {
        #[cfg(feature = "sanitizer-asan")]
        {
            (self.bottom.cast(), self.size)
        }
        #[cfg(not(feature = "sanitizer-asan"))]
        {
            (std::ptr::null(), 0)
        }
    }
}

/// Opaque ASan fake-stack token saved across a suspended fiber switch.
#[derive(Clone, Copy)]
pub struct AsanSave {
    #[cfg(feature = "sanitizer-asan")]
    ptr: *mut c_void,
}

// SAFETY: opaque ASan runtime handle moved with a suspended fiber.
unsafe impl Send for AsanSave {}
unsafe impl Sync for AsanSave {}

impl AsanSave {
    pub const NULL: Self = Self {
        #[cfg(feature = "sanitizer-asan")]
        ptr: std::ptr::null_mut(),
    };
}

#[cfg(feature = "sanitizer-asan")]
thread_local! {
    static ASAN_SWITCH_PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Begin switching from the current stack to `target`, saving the current
/// stack's ASan fake-stack token for resumption.
///
/// # Safety
/// Must be called immediately before [`crate::switch`] to `target`.
#[inline(always)]
pub unsafe fn asan_start_switch(target: StackBounds) -> AsanSave {
    #[cfg(feature = "sanitizer-asan")]
    {
        let mut save = std::ptr::null_mut();
        if !target.bottom.is_null() && target.size != 0 {
            ASAN_SWITCH_PENDING.set(true);
            // SAFETY: caller switches to `target` immediately after this call.
            unsafe { __sanitizer_start_switch_fiber(&mut save, target.bottom, target.size) };
        }
        AsanSave { ptr: save }
    }
    #[cfg(not(feature = "sanitizer-asan"))]
    {
        let _ = target;
        AsanSave::NULL
    }
}

/// Begin a terminal switch away from a fiber that will not be resumed again,
/// allowing ASan to release its fake stack immediately.
///
/// # Safety
/// Must be called immediately before [`crate::switch`] out of a dying fiber.
#[inline(always)]
pub unsafe fn asan_start_switch_exit(target: StackBounds) {
    #[cfg(feature = "sanitizer-asan")]
    {
        if !target.bottom.is_null() && target.size != 0 {
            ASAN_SWITCH_PENDING.set(true);
            // SAFETY: null `fake_stack_save` tells ASan this fiber is exiting permanently.
            unsafe {
                __sanitizer_start_switch_fiber(std::ptr::null_mut(), target.bottom, target.size)
            };
        }
    }
    #[cfg(not(feature = "sanitizer-asan"))]
    {
        let _ = target;
    }
}

/// Complete a fiber switch on the destination stack and return the previous
/// stack's bounds (needed so the fiber can switch back to whichever OS worker
/// resumed it).
///
/// # Safety
/// Must be called immediately after [`crate::switch`] resumes or at fiber entry.
#[inline(always)]
pub unsafe fn asan_finish_switch(save: AsanSave) -> StackBounds {
    #[cfg(feature = "sanitizer-asan")]
    {
        let mut bottom = std::ptr::null();
        let mut size = 0usize;
        if ASAN_SWITCH_PENDING.replace(false) {
            // SAFETY: paired with `asan_start_switch` / `asan_start_switch_exit` on the source stack.
            unsafe { __sanitizer_finish_switch_fiber(save.ptr, &mut bottom, &mut size) };
        }
        StackBounds { bottom, size }
    }
    #[cfg(not(feature = "sanitizer-asan"))]
    {
        let _ = save;
        StackBounds::EMPTY
    }
}

/// Complete a return switch back to the caller/host stack. If the fiber jumped
/// back asynchronously through the signal handler (`resume_saved`) without
/// calling `asan_start_switch_exit`, performs the start+finish pair now on the
/// host stack so ASan restores the host thread's stack bounds cleanly.
///
/// # Safety
/// Must be called on the caller/host stack immediately after [`crate::switch`] returns.
#[inline(always)]
pub unsafe fn asan_finish_switch_caller(save: AsanSave, caller_bounds: StackBounds) {
    #[cfg(feature = "sanitizer-asan")]
    {
        if !ASAN_SWITCH_PENDING.replace(false)
            && !caller_bounds.bottom.is_null()
            && caller_bounds.size != 0
        {
            // SAFETY: we are back on the host stack after a signal-handler `resume_saved`;
            // pair a terminal start with finish to restore ASan's host stack bounds.
            unsafe {
                __sanitizer_start_switch_fiber(
                    std::ptr::null_mut(),
                    caller_bounds.bottom,
                    caller_bounds.size,
                );
                __sanitizer_finish_switch_fiber(
                    save.ptr,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
            return;
        }
        // SAFETY: paired with fiber-side `asan_start_switch*`.
        unsafe {
            __sanitizer_finish_switch_fiber(save.ptr, std::ptr::null_mut(), std::ptr::null_mut());
        }
    }
    #[cfg(not(feature = "sanitizer-asan"))]
    {
        let _ = (save, caller_bounds);
    }
}

/// Unpoison an idle/recycled stack region in ASan shadow memory so redzones
/// left by an abandoned or terminal fiber frame do not cause false positives.
///
/// # Safety
/// `[addr, addr + size)` must be an idle fiber stack region with no live frames.
#[inline(always)]
pub unsafe fn asan_unpoison(addr: *const u8, size: usize) {
    #[cfg(feature = "sanitizer-asan")]
    {
        if !addr.is_null() && size != 0 {
            // SAFETY: caller guarantees the stack range is idle.
            unsafe { __asan_unpoison_memory_region(addr.cast(), size) };
        }
    }
    #[cfg(not(feature = "sanitizer-asan"))]
    {
        let _ = (addr, size);
    }
}

/// Copyable handle to a TSan fiber (either an OS thread's implicit fiber or
/// one created by [`TsanFiber::create`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TsanFiber {
    #[cfg(feature = "sanitizer-tsan")]
    ptr: *mut c_void,
}

// SAFETY: TSan fibers are explicitly designed to migrate between OS worker threads.
unsafe impl Send for TsanFiber {}
unsafe impl Sync for TsanFiber {}

impl TsanFiber {
    pub const NULL: Self = Self {
        #[cfg(feature = "sanitizer-tsan")]
        ptr: std::ptr::null_mut(),
    };

    #[inline(always)]
    pub fn from_raw(ptr: *mut u8) -> Self {
        #[cfg(feature = "sanitizer-tsan")]
        {
            Self { ptr: ptr.cast() }
        }
        #[cfg(not(feature = "sanitizer-tsan"))]
        {
            let _ = ptr;
            Self::NULL
        }
    }

    #[inline(always)]
    pub fn into_raw(self) -> *mut u8 {
        #[cfg(feature = "sanitizer-tsan")]
        {
            self.ptr.cast()
        }
        #[cfg(not(feature = "sanitizer-tsan"))]
        {
            std::ptr::null_mut()
        }
    }

    #[inline(always)]
    pub fn create() -> Self {
        #[cfg(feature = "sanitizer-tsan")]
        {
            // SAFETY: TSan runtime is linked when `sanitizer-tsan` is enabled.
            Self {
                ptr: unsafe { __tsan_create_fiber(0) },
            }
        }
        #[cfg(not(feature = "sanitizer-tsan"))]
        {
            Self::NULL
        }
    }

    #[inline(always)]
    pub fn current() -> Self {
        #[cfg(feature = "sanitizer-tsan")]
        {
            // SAFETY: TSan runtime is linked when `sanitizer-tsan` is enabled.
            Self {
                ptr: unsafe { __tsan_get_current_fiber() },
            }
        }
        #[cfg(not(feature = "sanitizer-tsan"))]
        {
            Self::NULL
        }
    }

    /// Synchronize and switch TSan's active fiber to `self`.
    ///
    /// # Safety
    /// `self` must be a live TSan fiber handle, called immediately before [`crate::switch`].
    #[inline(always)]
    pub unsafe fn switch_to(self) {
        #[cfg(feature = "sanitizer-tsan")]
        {
            if !self.ptr.is_null() {
                // SAFETY: caller owns exclusive transition to `self`.
                unsafe { __tsan_switch_to_fiber(self.ptr, 0) };
            }
        }
    }

    /// Ensure `self` is the active TSan fiber (restores host fiber after a
    /// signal-handler `resume_saved` jump that bypassed the fiber-side switch).
    ///
    /// # Safety
    /// `self` must be the live host TSan fiber for the current stack.
    #[inline(always)]
    pub unsafe fn ensure_current(self) {
        #[cfg(feature = "sanitizer-tsan")]
        {
            if !self.ptr.is_null() && Self::current() != self {
                // SAFETY: caller is already executing on `self`'s stack.
                unsafe { __tsan_switch_to_fiber(self.ptr, 0) };
            }
        }
    }

    /// Destroy a TSan fiber created by [`Self::create`].
    ///
    /// # Safety
    /// Must not be called while `self` is the currently active fiber.
    #[inline(always)]
    pub unsafe fn destroy(self) {
        #[cfg(feature = "sanitizer-tsan")]
        {
            if !self.ptr.is_null() {
                // SAFETY: caller guarantees `self` is no longer active.
                unsafe { __tsan_destroy_fiber(self.ptr) };
            }
        }
    }
}

/// RAII owner of a TSan fiber created via [`TsanFiber::create`].
pub struct OwnedTsanFiber(TsanFiber);

impl OwnedTsanFiber {
    pub const NULL: Self = Self(TsanFiber::NULL);

    #[inline(always)]
    pub fn new() -> Self {
        Self(TsanFiber::create())
    }

    #[inline(always)]
    pub fn handle(&self) -> TsanFiber {
        self.0
    }

    /// Replace with a fresh TSan fiber, destroying any previous one.
    ///
    /// # Safety
    /// Any previously held fiber must not be currently active.
    #[inline(always)]
    pub unsafe fn reset(&mut self) {
        let old = std::mem::replace(&mut self.0, TsanFiber::create());
        // SAFETY: caller guarantees `old` is not active.
        unsafe { old.destroy() };
    }

    /// Destroy any held TSan fiber and reset to null.
    ///
    /// # Safety
    /// Any held fiber must not be currently active.
    #[inline(always)]
    pub unsafe fn clear(&mut self) {
        let old = std::mem::replace(&mut self.0, TsanFiber::NULL);
        // SAFETY: caller guarantees `old` is not active.
        unsafe { old.destroy() };
    }
}

impl Default for OwnedTsanFiber {
    fn default() -> Self {
        Self::NULL
    }
}

impl Drop for OwnedTsanFiber {
    fn drop(&mut self) {
        // SAFETY: an owned TSan fiber is only dropped off-fiber.
        unsafe { self.0.destroy() };
    }
}

/// Per-fiber ASan + TSan annotation state (zero-sized in normal builds).
pub struct FiberState {
    asan_caller: Cell<StackBounds>,
    tsan_fiber: Cell<TsanFiber>,
    tsan_caller: Cell<TsanFiber>,
}

// SAFETY: custody of a fiber moves exclusively between OS workers one at a time.
unsafe impl Send for FiberState {}

impl FiberState {
    pub const fn new() -> Self {
        Self {
            asan_caller: Cell::new(StackBounds::EMPTY),
            tsan_fiber: Cell::new(TsanFiber::NULL),
            tsan_caller: Cell::new(TsanFiber::NULL),
        }
    }

    /// Create a fresh TSan fiber for a newly mounted/prepared stack.
    ///
    /// # Safety
    /// Any previously held TSan fiber must not be active.
    #[inline(always)]
    pub unsafe fn reset(&self) {
        let old = self.tsan_fiber.replace(TsanFiber::create());
        // SAFETY: caller guarantees `old` is off-fiber.
        unsafe { old.destroy() };
    }

    /// Destroy any held TSan fiber after the fiber exited permanently or unmounted.
    ///
    /// # Safety
    /// Must be called off-fiber (after returning to the caller/host stack).
    #[inline(always)]
    pub unsafe fn clear(&self) {
        let old = self.tsan_fiber.replace(TsanFiber::NULL);
        // SAFETY: caller guarantees `old` is off-fiber.
        unsafe { old.destroy() };
    }

    /// Switch from the caller/host stack into this fiber and wait until it
    /// yields, exits, or aborts back.
    ///
    /// # Safety
    /// Same preconditions as [`crate::switch`]; `target_bounds` must describe
    /// this fiber's usable stack range.
    #[inline(always)]
    pub unsafe fn switch_in(&self, save: *mut *mut u8, to: *mut u8, target_bounds: StackBounds) {
        let caller_tsan = TsanFiber::current();
        self.tsan_caller.set(caller_tsan);
        // SAFETY: paired with `entered`/`switch_out`/`switch_exit` on the fiber.
        unsafe {
            let host_asan = asan_start_switch(target_bounds);
            self.tsan_fiber.get().switch_to();
            crate::switch(save, to);
            asan_finish_switch_caller(host_asan, self.asan_caller.get());
            caller_tsan.ensure_current();
        }
    }

    /// Complete the initial switch into a freshly started fiber entry function.
    /// Returns the caller's `(asan_bounds, tsan_fiber)`.
    ///
    /// # Safety
    /// Must be called at the start of a newly started fiber entered via [`Self::switch_in`].
    #[inline(always)]
    pub unsafe fn entered(&self) -> (StackBounds, TsanFiber) {
        // SAFETY: paired with `switch_in` on the caller stack.
        let caller_asan = unsafe { asan_finish_switch(AsanSave::NULL) };
        self.asan_caller.set(caller_asan);
        (caller_asan, self.tsan_caller.get())
    }

    /// Suspend this fiber back to the caller/host stack and resume when a
    /// worker (possibly a different OS thread) switches back in.
    /// Returns the resuming caller's `(asan_bounds, tsan_fiber)`.
    ///
    /// # Safety
    /// Same preconditions as [`crate::switch`].
    #[inline(always)]
    pub unsafe fn switch_out(&self, save: *mut *mut u8, to: *mut u8) -> (StackBounds, TsanFiber) {
        // SAFETY: paired with `switch_in` on the caller stack.
        unsafe {
            let fiber_asan = asan_start_switch(self.asan_caller.get());
            self.tsan_caller.get().switch_to();
            crate::switch(save, to);
            let caller_asan = asan_finish_switch(fiber_asan);
            self.asan_caller.set(caller_asan);
            (caller_asan, self.tsan_caller.get())
        }
    }

    /// Terminal switch from a fiber that will not be resumed again back to its
    /// caller/host stack.
    ///
    /// # Safety
    /// Same preconditions as [`crate::switch`]; the fiber must never be resumed.
    #[inline(always)]
    pub unsafe fn switch_exit(&self, save: *mut *mut u8, to: *mut u8) {
        let asan = self.asan_caller.get();
        let tsan = self.tsan_caller.get();
        // SAFETY: terminal exit frees the fiber's ASan fake stack cleanly;
        // `save` must point outside the dying fiber's stack (e.g. heap Task/Unit).
        unsafe {
            asan_start_switch_exit(asan);
            tsan.switch_to();
            crate::switch(save, to);
        }
    }

    /// Terminal switch abandoning the current fiber without saving its SP.
    ///
    /// # Safety
    /// Same preconditions as [`crate::switch_abandon`].
    #[inline(always)]
    pub unsafe fn switch_abandon(&self, to: *mut u8) -> ! {
        let asan = self.asan_caller.get();
        let tsan = self.tsan_caller.get();
        // SAFETY: terminal exit frees the fiber's ASan fake stack cleanly.
        unsafe {
            asan_start_switch_exit(asan);
            tsan.switch_to();
            crate::switch_abandon(to)
        }
    }
}

impl Default for FiberState {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FiberState {
    fn drop(&mut self) {
        // SAFETY: fiber state is dropped only after the fiber is no longer active.
        unsafe { self.clear() };
    }
}

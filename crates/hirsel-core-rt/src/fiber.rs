//! Guarded task stacks. Both native emitters and Rust probe large frames.
//! Non-suspending foreign OS calls run on a separate guarded host stack.
//! Language code never runs on the launcher's stack.
//!
//! Every stack reserves `RESERVE` bytes above one guard page, like a Rust main
//! thread, but only its top `COMMIT` bytes start writable. A fault below the
//! writable part makes the next `GROW` bytes writable (Windows' commit-on-touch
//! stack, adapted: the fault handler commits instead of a guard-page chain), so
//! a shallow task costs what a 64 KiB stack did in RSS, the data limit and
//! retained stock. Recycling returns a stack that grew to its first `COMMIT`.
//! The top 16 bytes hold two words: the low-water address (`top - 8`) and the
//! async-signal-safe retired flag (`top - 16`).
use crate::{
    retention::{Budget, RETENTION, Retained},
    run_stop::{
        StackKind, Stop, current_slot, current_slot_here, current_slot_or_acquire,
        maybe_release_slot,
    },
};
use std::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release},
};

#[path = "fiber_fault.rs"]
mod fault;
pub(crate) use fault::arm_code;
#[cfg(test)]
pub(self) use fault::fault;
pub use fault::{
    Alternate, ArmError, arm_embedded, growth_faults, guard_hits, reset_fault_counters,
};

const RESERVE: usize = hirsel_core_layout::stack::RESERVATION as usize;
pub(crate) const COMMIT: usize = 64 << 10;
pub(crate) fn commit_for_page(page: usize) -> usize {
    COMMIT.max(4 * page)
}

const GROW: usize = 256 << 10;
/// Bytes kept at the top for the low-water and retired words; a multiple of 16.
const HEADER: usize = 16;
/// Stack room checked before taking a lock or entering libc/allocator on a
/// fiber stack (EMB-3 "The guard rule").
pub const RUNTIME_ROOM: usize = 16 << 10;

crate::units::moved_tls! { static STOP_FD: i32 = 2; }

#[cfg(test)]
#[path = "fiber_stop_tests.rs"]
mod stop_tests;

pub(crate) fn stop_fd() -> i32 {
    STOP_FD.get()
}
pub(crate) fn set_stop_fd(fd: i32) -> i32 {
    STOP_FD.replace(fd)
}
/// `set_stop_fd` on a stack that does not move threads (a worker's host stack).
#[inline(always)]
pub(crate) fn set_stop_fd_here(fd: i32) -> i32 {
    STOP_FD.replace_here(fd)
}

#[cfg(feature = "prof")]
pub(crate) fn profile_stack_top(sp: usize) -> Option<usize> {
    let (_, guard_end, top) = guarded();
    if guard_end == 0 {
        return None;
    }
    if sp < guard_end || sp >= top - HEADER {
        return None;
    }
    // SAFETY: the active guard pins this stack's committed header. The
    // current thread owns its low-water word until the next stack switch.
    let committed = unsafe { low_word(top).read() };
    (sp >= committed).then_some(top - HEADER)
}

pub(crate) fn guard(bounds: (usize, usize, usize)) {
    let kind = if bounds.1 == 0 {
        StackKind::None
    } else {
        StackKind::Recoverable
    };
    guard_full((bounds, kind));
}

pub(crate) fn guard_full((bounds, kind): ((usize, usize, usize), StackKind)) {
    if kind == StackKind::None {
        if let Some(slot) = current_slot() {
            slot.set_guard(0, 0, 0, StackKind::None);
            maybe_release_slot();
        }
        return;
    }
    let slot = current_slot_or_acquire();
    slot.set_guard(bounds.0, bounds.1, bounds.2, kind);
}

/// The bounds `guard` last set: `(base, guard_end, top)`.
pub fn guarded() -> (usize, usize, usize) {
    let (b, g, t, _) = guarded_full_raw();
    (b, g, t)
}

/// Current committed low-water address of the active guarded stack, or 0 if none.
pub fn current_low_water() -> usize {
    let (_, guard_end, top) = guarded();
    if guard_end == 0 {
        return 0;
    }
    // SAFETY: active guarded stack's top header page is always writable.
    unsafe { *low_word(top) }
}

pub(crate) fn guarded_full() -> ((usize, usize, usize), StackKind) {
    let (b, g, t, k) = guarded_full_raw();
    ((b, g, t), k)
}

fn guarded_full_raw() -> (usize, usize, usize, StackKind) {
    match current_slot() {
        Some(slot) => slot.get_guard(),
        None => (0, 0, 0, StackKind::None),
    }
}

/// `guard` from code that does not cross a stack switch before returning.
#[inline(always)]
pub(crate) fn guard_here(bounds: (usize, usize, usize)) {
    let kind = if bounds.1 == 0 {
        StackKind::None
    } else {
        StackKind::Recoverable
    };
    if kind == StackKind::None {
        if let Some(slot) = current_slot_here() {
            slot.set_guard(0, 0, 0, StackKind::None);
            maybe_release_slot();
        }
        return;
    }
    let slot = current_slot_or_acquire();
    slot.set_guard(bounds.0, bounds.1, bounds.2, kind);
}

/// Ensure at least `bytes` of **already-writable** stack below the current
/// stack pointer (EMB-3 "The guard rule").
///
/// - Outside a guarded stack: returns `Ok(())`.
/// - If `bytes` fits above `*low_word(top)`: fast-path `Ok(())` (zero syscalls).
/// - If `bytes` would cross the hard guard page: returns `Err(Stop("Stack"))`
///   without hitting the guard page.
/// - If `bytes` fits above the hard guard but crosses `low_word`: pre-commits
///   the intervening pages with `mprotect` before any lock/libc entry; on
///   `mprotect` failure, marks the stack retired and returns `Err(Stop("HostMemory"))`.
#[inline(always)]
pub fn room(bytes: usize) -> Result<(), Stop> {
    let Some(slot) = current_slot() else {
        return Ok(());
    };
    let (base, guard_end, top, kind) = slot.get_guard();
    if kind == StackKind::None || guard_end == 0 {
        return Ok(());
    }
    let here = 0u8;
    let sp = std::hint::black_box(&here as *const u8).addr();
    // SAFETY: active guarded stack's top header page is always writable.
    let low = unsafe { *low_word(top) };
    if sp >= low && sp - low >= bytes {
        return Ok(());
    }
    room_slow(base, guard_end, top, sp, low, bytes)
}

#[cold]
#[inline(never)]
fn room_slow(
    base: usize,
    guard_end: usize,
    top: usize,
    sp: usize,
    low: usize,
    bytes: usize,
) -> Result<(), Stop> {
    if sp < guard_end || sp - guard_end < bytes {
        return Err(Stop::from_static(b"Stack"));
    }
    let page_bytes = guard_end - base;
    let target = (sp - bytes) & !(page_bytes - 1);
    let from = target.max(guard_end);
    if from < low {
        // SAFETY: `[from, low)` lies strictly inside this stack's private
        // mapping above its guard page and below its current low-water mark.
        let rc = unsafe {
            libc::mprotect(
                std::ptr::with_exposed_provenance_mut(from),
                low - from,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if rc != 0 {
            // Fail closed: do not advance low_word; retire this stack whole.
            // SAFETY: header page is always writable.
            unsafe { (*retired_word(top)).store(1, Release) };
            return Err(Stop::from_static(b"HostMemory"));
        }
        // SAFETY: `mprotect` succeeded, so `[from, top)` is writable.
        unsafe { *low_word(top) = from };
    }
    Ok(())
}

/// Install the process fault handler if not yet installed (never touches the
/// calling thread's alternate signal stack).
pub(crate) fn arm() {
    if let Err(stop) = fault::arm() {
        crate::run_stop::raise(stop);
    }
}

pub(crate) fn try_page() -> Result<usize, Stop> {
    // SAFETY: sysconf reads the kernel's immutable page size.
    try_page_bytes(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
}

#[cfg(test)]
fn page() -> usize {
    try_page().unwrap_or_else(|stop| crate::run_stop::raise(stop))
}

pub(crate) fn try_page_bytes(value: libc::c_long) -> Result<usize, Stop> {
    match usize::try_from(value) {
        Ok(bytes) if bytes != 0 => Ok(bytes),
        _ => Err(Stop::from_static(b"PageSize")),
    }
}

#[cfg(test)]
fn page_bytes(value: libc::c_long) -> usize {
    try_page_bytes(value).unwrap_or_else(|stop| crate::run_stop::raise(stop))
}

pub(crate) struct Stack {
    base: NonNull<u8>,
    len: usize,
    guard_end: usize,
    #[cfg(any(test, feature = "memcheck"))]
    valgrind: u64,
}

impl Stack {
    pub(crate) fn new() -> Self {
        Self::try_new().unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }

    pub(crate) fn try_new() -> Result<Self, Stop> {
        Self::try_new_with_reserve(RESERVE)
    }

    pub(crate) fn new_with_reserve(reserve: usize) -> Self {
        Self::try_new_with_reserve(reserve).unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }

    pub(crate) fn try_new_with_reserve(reserve: usize) -> Result<Self, Stop> {
        let page_bytes = try_page()?;
        let commit = commit_for_page(page_bytes);
        let len = page_bytes + reserve;
        // SAFETY: fresh inaccessible private mapping, then enable only its top pages.
        unsafe {
            let raw = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if raw == libc::MAP_FAILED {
                return Err(Stop::from_static(b"HostMemory"));
            }
            let top = raw.addr() + len;
            if libc::mprotect(
                raw.cast::<u8>().add(len - commit).cast(),
                commit,
                libc::PROT_READ | libc::PROT_WRITE,
            ) != 0
            {
                libc::munmap(raw, len);
                return Err(Stop::from_static(b"HostMemory"));
            }
            low_word(top).write(top - commit);
            (*retired_word(top)).store(0, Relaxed);
            Ok(Self {
                base: NonNull::new_unchecked(raw.cast()),
                len,
                guard_end: raw.addr() + page_bytes,
                #[cfg(any(test, feature = "memcheck"))]
                valgrind: hirsel_stack::stack_register(
                    raw.cast::<u8>().add(page_bytes),
                    raw.cast::<u8>().add(len),
                ),
            })
        }
    }

    pub(crate) fn try_new_batch_with_reserve(
        count: usize,
        reserve: usize,
    ) -> Result<Vec<Self>, Stop> {
        if count <= 1 {
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
                out.push(Self::try_new_with_reserve(reserve)?);
            }
            return Ok(out);
        }
        let page_bytes = try_page()?;
        let commit = commit_for_page(page_bytes);
        let len = page_bytes + reserve;
        let Some(total) = len.checked_mul(count) else {
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
                out.push(Self::try_new_with_reserve(reserve)?);
            }
            return Ok(out);
        };
        // SAFETY: one contiguous inaccessible mapping partitioned into `count`
        // independent guarded stacks, each with its own guard page and top
        // writable band, unmapped individually on Drop.
        unsafe {
            let raw = libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if raw == libc::MAP_FAILED {
                let mut out = Vec::with_capacity(count);
                for _ in 0..count {
                    out.push(Self::try_new_with_reserve(reserve)?);
                }
                return Ok(out);
            }
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let base = raw.cast::<u8>().add(i * len);
                let top = base.addr() + len;
                if libc::mprotect(
                    base.add(len - commit).cast(),
                    commit,
                    libc::PROT_READ | libc::PROT_WRITE,
                ) != 0
                {
                    // Unmap remaining unpushed segments; pushed `out` drops cleanly.
                    libc::munmap(base.cast(), (count - i) * len);
                    return Err(Stop::from_static(b"HostMemory"));
                }
                low_word(top).write(top - commit);
                (*retired_word(top)).store(0, Relaxed);
                out.push(Self {
                    base: NonNull::new_unchecked(base),
                    len,
                    guard_end: base.addr() + page_bytes,
                    #[cfg(any(test, feature = "memcheck"))]
                    valgrind: hirsel_stack::stack_register(base.add(page_bytes), base.add(len)),
                });
            }
            Ok(out)
        }
    }

    pub(crate) fn new_batch_with_reserve(count: usize, reserve: usize) -> Vec<Self> {
        Self::try_new_batch_with_reserve(count, reserve)
            .unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }

    pub(crate) fn bounds(&self) -> (usize, usize, usize) {
        (self.base.as_ptr().addr(), self.guard_end, self.top())
    }

    pub(crate) fn sanitizer_bounds(&self) -> hirsel_stack::sanitizer::StackBounds {
        let guard = self.guard_bytes();
        // SAFETY: `guard` lies within this stack's mapping.
        let bottom = unsafe { self.base.as_ptr().add(guard) };
        hirsel_stack::sanitizer::StackBounds::from_raw(bottom, self.len - guard - HEADER)
    }

    /// True when an `mprotect` growth failure retired this stack (EMB-3). A
    /// retired stack is never trimmed or recycled; it is unmapped whole on drop.
    pub(crate) fn is_retired(&self) -> bool {
        // SAFETY: top header page is always writable while `self` is live.
        unsafe { (*retired_word(self.top())).load(Acquire) != 0 }
    }

    fn guard_bytes(&self) -> usize {
        self.guard_end - self.base.as_ptr().addr()
    }

    /// Run a synchronous native foreign or host call on this stack, publishing
    /// it as [`StackKind::Native`] so guard hits and refused growth forward to
    /// the host's previous fault action instead of abandoning native frames.
    ///
    /// # Safety
    /// The caller exclusively owns the idle stack. The callback must finish
    /// synchronously and must not switch or unwind across the stack boundary.
    pub(crate) unsafe fn call<T, F: FnOnce() -> T>(&self, call: F) -> T {
        struct Frame<T, F> {
            call: Option<F>,
            previous: *mut u8,
            bounds: (usize, usize, usize),
            previous_guard: ((usize, usize, usize), StackKind),
            sanitizer: hirsel_stack::sanitizer::FiberState,
            result: std::mem::MaybeUninit<T>,
        }
        unsafe extern "C" fn invoke<T, F: FnOnce() -> T>(first: *mut u8, _: *mut u8) -> ! {
            // SAFETY: the original stack pins the frame and is idle while we
            // own it. The single invocation initializes the result.
            unsafe {
                let frame = first.cast::<Frame<T, F>>();
                let _ = (*frame).sanitizer.entered();
                // This stack is current from its first frame on, marked Native
                // so a foreign overflow or growth refusal is never recovered.
                guard_full(((*frame).bounds, StackKind::Native));
                let call = (*frame).call.take().unwrap();
                (*frame).result.write(call());
                // Restore the caller's guard while still on this committed top
                // page, so returning onto a caller near its commit edge cannot
                // fault while `callee` is still the active guard.
                let previous = (*frame).previous;
                guard_full((*frame).previous_guard);
                (*frame).sanitizer.switch_abandon(previous)
            }
        }
        // The switch saves registers on the caller's stack, which may cross
        // its commit edge: keep the caller's guard until `invoke` runs.
        let previous_guard = guarded_full();
        let sanitizer = hirsel_stack::sanitizer::FiberState::new();
        // SAFETY: freshly created fiber state has no active TSan fiber.
        unsafe { sanitizer.reset() };
        let mut frame = Frame {
            call: Some(call),
            previous: std::ptr::null_mut(),
            bounds: self.bounds(),
            previous_guard,
            sanitizer,
            result: std::mem::MaybeUninit::uninit(),
        };
        // SAFETY: the borrowed stack is idle. Only invoke can switch back,
        // after writing the result, restoring `previous_guard`, and ceasing
        // access to the frame.
        unsafe {
            let start = self.prepare(
                invoke::<T, F>,
                (&raw mut frame).cast(),
                std::ptr::null_mut(),
            );
            frame
                .sanitizer
                .switch_in(&raw mut frame.previous, start, self.sanitizer_bounds());
            frame.sanitizer.clear();
            frame.result.assume_init()
        }
    }

    /// # Safety
    /// Stack is idle, entry never returns, and its operands remain live until completion.
    pub(crate) unsafe fn prepare(
        &self,
        entry: hirsel_stack::Entry,
        first: *mut u8,
        second: *mut u8,
    ) -> *mut u8 {
        let words = hirsel_stack::initial_frame(entry, first, second);
        // SAFETY: 16-aligned top and sufficient space for this architecture's frame.
        unsafe {
            let (bottom, size) = self.sanitizer_bounds().into_raw();
            hirsel_stack::sanitizer::asan_unpoison(bottom, size);
            let start = self
                .base
                .as_ptr()
                .add(self.len - HEADER - size_of_val(&words));
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), start, size_of_val(&words));
            start
        }
    }

    /// Overwrite what a fiber that returned its stack left above `sp`, so a
    /// pointer into the released stack reads garbage in debug and checker
    /// builds instead of the stale frame (the returnable form keeps none).
    #[cfg(any(test, debug_assertions, feature = "testing"))]
    pub(crate) fn poison_above(&self, sp: *mut u8) {
        if self.is_retired() {
            return;
        }
        let top = self.top();
        // SAFETY: the stack is idle; [low, top - HEADER) is writable, and the
        // red zone below `sp` is inside it.
        unsafe {
            let low = (*low_word(top)).max(sp.addr().saturating_sub(128));
            let ptr = std::ptr::with_exposed_provenance_mut::<u8>(low);
            let len = top - HEADER - low;
            hirsel_stack::sanitizer::asan_unpoison(ptr, len);
            std::ptr::write_bytes(ptr, 0xA5, len);
        }
    }

    /// The end of the reservation, where the low-water word sits.
    fn top(&self) -> usize {
        self.base.as_ptr().addr() + self.len
    }

    /// Bytes an idle, trimmed stack keeps writable: its guard page and first `COMMIT`.
    fn retained(&self) -> usize {
        self.guard_bytes() + commit_for_page(self.guard_bytes())
    }

    /// Upper bound on resident bytes while this idle mapping is cached.
    pub(crate) fn committed_bytes(&self) -> usize {
        let top = self.top();
        // SAFETY: this stack is idle and its low-water word is writable.
        top - unsafe { *low_word(top) } + self.guard_bytes()
    }

    /// Return a stack that grew to its first `COMMIT` bytes. A stack that never
    /// left them costs one load and compare, inlined into recycling.
    pub(crate) fn try_trim(&self) -> Result<(), Stop> {
        if self.is_retired() {
            return Ok(());
        }
        let top = self.top();
        // SAFETY: the stack is idle and its top word is always writable.
        let committed = top - commit_for_page(self.guard_bytes());
        if unsafe { *low_word(top) } < committed {
            release(self.guard_end, top, committed)?;
        }
        Ok(())
    }

    pub(crate) fn trim(&self) {
        if let Err(stop) = self.try_trim() {
            crate::run_stop::raise(stop);
        }
    }

    /// # Safety
    /// The caller exclusively owns this off-CPU stack and every page below
    /// the page containing `sp` has no live frame.
    pub(crate) unsafe fn discard_below(&self, sp: usize) {
        if self.is_retired() {
            return;
        }
        let top = self.top();
        let page = self.guard_bytes();
        // SAFETY: the stack is pinned and the low-water word remains writable.
        let low = unsafe { low_word(top).read() };
        debug_assert!((low..top).contains(&sp));
        let end = sp & !(page - 1);
        if low < end {
            // SAFETY: the caller owns these complete writable pages, none of
            // which contains the cut point or the stack header.
            unsafe { crate::host::sys::discard(low, end - low) };
        }
    }
}

/// Drop the grown pages of the idle stack whose writable range starts at
/// `guard_end` and whose reservation ends at `top`, with one fresh inaccessible
/// mapping. Out of line, and given plain addresses, so recycling a shallow
/// stack stays inlined and keeps the stack in registers.
#[cold]
#[inline(never)]
fn release(guard_end: usize, top: usize, committed: usize) -> Result<(), Stop> {
    // SAFETY: the stack is idle; the range is inside its own reservation.
    unsafe {
        let raw = libc::mmap(
            guard_end as *mut libc::c_void,
            committed - guard_end,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_FIXED,
            -1,
            0,
        );
        if raw == libc::MAP_FAILED {
            (*retired_word(top)).store(1, Release);
            return Err(Stop::from_static(b"HostMemory"));
        }
        *low_word(top) = committed;
        Ok(())
    }
}

/// The low-water word of the stack whose reservation ends at `top`.
fn low_word(top: usize) -> *mut usize {
    std::ptr::with_exposed_provenance_mut::<usize>(top - size_of::<usize>())
}

/// Async-signal-safe retired flag stored in the unused half of `HEADER`.
fn retired_word(top: usize) -> *const AtomicUsize {
    std::ptr::with_exposed_provenance::<AtomicUsize>(top - 2 * size_of::<usize>())
}

impl Drop for Stack {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "memcheck"))]
        hirsel_stack::stack_deregister(self.valgrind);
        // SAFETY: the scheduler unmaps only a stack whose fiber ended permanently.
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), self.len);
        }
    }
}

/// Idle stacks a worker's or run's spawns reuse.
pub(crate) struct Stock {
    budget: &'static Budget,
    kept: Vec<(Stack, Retained<'static>)>,
    // The prefix whose stack pages have already been returned during idle.
    cooled: usize,
    surplus: Vec<Stack>,
}

impl Default for Stock {
    fn default() -> Self {
        Self::with_budget(&RETENTION)
    }
}

/// Surplus stacks held by every stock, so an idle worker with none skips the release.
static SURPLUS: AtomicUsize = AtomicUsize::new(0);

/// Whether some stock holds surplus stacks for an idle worker to release.
pub(crate) fn surplus() -> bool {
    SURPLUS.load(Relaxed) != 0
}

// SAFETY: an idle stack holds no live frame or thread-local address; only its
// mapping moves between workers.
unsafe impl Send for Stack {}

impl Stock {
    fn with_budget(budget: &'static Budget) -> Self {
        Self {
            budget,
            kept: Vec::new(),
            cooled: 0,
            surplus: Vec::new(),
        }
    }

    pub(crate) fn try_take(&mut self) -> Result<Stack, Stop> {
        if let Some(memory) = crate::units::memory() {
            return Ok(memory.take_task_stack());
        }
        if let Some(stack) = self.surplus.pop() {
            SURPLUS.fetch_sub(1, Relaxed);
            return Ok(stack);
        }
        match self.kept.pop() {
            Some((stack, _)) => Ok(stack),
            None => Stack::try_new(),
        }
    }

    pub(crate) fn take(&mut self) -> Stack {
        self.try_take()
            .unwrap_or_else(|stop| crate::run_stop::raise(stop))
    }

    pub(crate) fn recycle(&mut self, stack: Stack) {
        if stack.is_retired() || crate::run_stop::is_ended() {
            // Retired stacks or ended-generation stacks are never trimmed or
            // recycled: Drop unmaps the entire reservation whole with munmap.
            drop(stack);
            return;
        }
        if let Err(stop) = stack.try_trim() {
            drop(stack);
            crate::run_stop::raise(stop);
        }
        self.recycle_trimmed(stack);
    }

    /// Recycle a finished task's stack. A root task returns its trimmed stack
    /// to its owning unit so the unit can recycle the guarded pair together.
    pub(crate) fn recycle_ended(&mut self, is_root: bool, stack: Stack) {
        if stack.is_retired() || crate::run_stop::is_ended() {
            drop(stack);
            return;
        }
        if let Err(stop) = stack.try_trim() {
            drop(stack);
            crate::run_stop::raise(stop);
        }
        let stack = if is_root {
            match crate::units::return_root_stack(stack) {
                Ok(()) => return,
                Err(stack) => stack,
            }
        } else {
            stack
        };
        self.recycle_trimmed(stack);
    }

    fn recycle_trimmed(&mut self, stack: Stack) {
        if let Some(memory) = crate::units::memory() {
            memory.put_task_stack(stack);
            return;
        }
        match self.budget.retain(stack.retained()) {
            Some(held) => {
                self.cooled = self.cooled.min(self.kept.len());
                self.kept.push((stack, held));
            }
            None => {
                SURPLUS.fetch_add(1, Relaxed);
                self.surplus.push(stack);
            }
        }
    }

    /// Unmap the surplus; kept stacks stay for the next spawns.
    pub(crate) fn release(&mut self) {
        SURPLUS.fetch_sub(self.surplus.len(), Relaxed);
        self.surplus.clear();
    }

    /// Drop every kept and surplus stack whole (used at generation teardown).
    pub(crate) fn clear_all(&mut self) {
        self.release();
        self.kept.clear();
        self.cooled = 0;
    }

    pub(crate) fn has_warm_kept(&self) -> bool {
        self.kept.len() > self.cooled
    }

    /// Keep each reservation and its low-water word. The ended task owns no
    /// frames, so the rest of its writable pages can return to the host.
    pub(crate) fn cool_kept(&mut self) {
        for (stack, _) in self.kept.iter().skip(self.cooled) {
            let page = stack.guard_bytes();
            // SAFETY: the stock owns this idle stack. Recycling trimmed
            // it to COMMIT, and the last native page holds its low word.
            unsafe { stack.discard_below(stack.top() - page) };
        }
        self.cooled = self.kept.len();
    }
}

impl Drop for Stock {
    fn drop(&mut self) {
        self.release();
    }
}

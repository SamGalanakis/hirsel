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
//! The word at the stack's top records its lowest writable address.
use crate::retention::{Budget, RETENTION, Retained};
use std::{
    cell::Cell,
    ptr::NonNull,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
};
const RESERVE: usize = 8 << 20;
pub(crate) const COMMIT: usize = 64 << 10;
const GROW: usize = 256 << 10;
/// Bytes kept at the top for the low-water word; a multiple of 16.
const HEADER: usize = 16;
thread_local! {
    static ALTERNATE: Alternate = Alternate::new();
}
crate::units::moved_tls! { static GUARD: (usize, usize) = (0, 0); }
crate::units::moved_tls! { static STOP_FD: i32 = 2; }
static PREVIOUS: OnceLock<[libc::sigaction; FAULTS.len()]> = OnceLock::new();
/// The signals a stack fault raises: macOS reports a touch of a mapped but
/// inaccessible page as SIGBUS where Linux reports SIGSEGV.
#[cfg(target_os = "macos")]
const FAULTS: [i32; 2] = [libc::SIGSEGV, libc::SIGBUS];
#[cfg(not(target_os = "macos"))]
const FAULTS: [i32; 1] = [libc::SIGSEGV];
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
    let (_, guard_end) = GUARD.get();
    if guard_end == 0 {
        return None;
    }
    let top = guard_end + RESERVE;
    if sp < guard_end || sp >= top - HEADER {
        return None;
    }
    // SAFETY: the active guard pins this stack's committed header. The
    // current thread owns its low-water word until the next stack switch.
    let committed = unsafe { low_word(top).read() };
    (sp >= committed).then_some(top - HEADER)
}

pub(crate) fn guard(bounds: (usize, usize)) {
    GUARD.set(bounds);
}
/// `guard` from code that does not cross a stack switch before returning.
#[inline(always)]
pub(crate) fn guard_here(bounds: (usize, usize)) {
    GUARD.set_here(bounds);
}
/// Whether the current stack has `bytes` above its guard page. A thread with
/// no guarded stack current (a launcher or worker thread's own stack) has room.
#[inline(always)]
pub(crate) fn room(bytes: usize) -> bool {
    let (_, end) = GUARD.get();
    let here = 0u8;
    std::hint::black_box(&here as *const u8)
        .addr()
        .wrapping_sub(end)
        >= bytes
        || end == 0
}
pub(crate) fn arm() {
    PREVIOUS.get_or_init(|| {
        // SAFETY: initialized sigaction structures and a signal-compatible handler.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let mut previous: [libc::sigaction; FAULTS.len()] = std::mem::zeroed();
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            action.sa_sigaction = fault as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            for (signal, previous) in FAULTS.iter().zip(&mut previous) {
                if libc::sigaction(*signal, &action, previous) != 0 {
                    crate::process::stop(b"SignalStack");
                }
            }
            previous
        }
    });
    ALTERNATE.with(|_| ());
}
fn page() -> usize {
    // SAFETY: sysconf reads the kernel's immutable page size.
    page_bytes(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
}
fn page_bytes(value: libc::c_long) -> usize {
    match usize::try_from(value) {
        Ok(bytes) if bytes != 0 => bytes,
        _ => crate::process::stop(b"PageSize"),
    }
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
        let page_bytes = page();
        let len = page_bytes + RESERVE;
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
                crate::process::stop(b"HostMemory");
            }
            let top = raw.addr() + len;
            if libc::mprotect(
                raw.cast::<u8>().add(len - COMMIT).cast(),
                COMMIT,
                libc::PROT_READ | libc::PROT_WRITE,
            ) != 0
            {
                crate::process::stop(b"HostMemory");
            }
            low_word(top).write(top - COMMIT);
            Self {
                base: NonNull::new_unchecked(raw.cast()),
                len,
                guard_end: raw.addr() + page_bytes,
                #[cfg(any(test, feature = "memcheck"))]
                valgrind: hirsel_stack::stack_register(
                    raw.cast::<u8>().add(page_bytes),
                    raw.cast::<u8>().add(len),
                ),
            }
        }
    }
    pub(crate) fn bounds(&self) -> (usize, usize) {
        (self.base.as_ptr().addr(), self.guard_end)
    }

    /// # Safety
    /// The caller exclusively owns the idle stack. The callback must finish
    /// synchronously and must not switch or unwind across the stack boundary.
    pub(crate) unsafe fn call<T, F: FnOnce() -> T>(&self, call: F) -> T {
        struct Frame<T, F> {
            call: Option<F>,
            previous: *mut u8,
            result: std::mem::MaybeUninit<T>,
        }
        unsafe extern "C" fn invoke<T, F: FnOnce() -> T>(first: *mut u8, _: *mut u8) -> ! {
            // SAFETY: the original stack pins the frame and is idle while we
            // own it. The single invocation initializes the result.
            unsafe {
                let frame = first.cast::<Frame<T, F>>();
                let call = (*frame).call.take().unwrap();
                (*frame).result.write(call());
                let mut discard = std::ptr::null_mut();
                hirsel_stack::switch(&mut discard, (*frame).previous);
                std::hint::unreachable_unchecked();
            }
        }
        let mut frame = Frame {
            call: Some(call),
            previous: std::ptr::null_mut(),
            result: std::mem::MaybeUninit::uninit(),
        };
        // SAFETY: the borrowed stack is idle. Only invoke can switch back,
        // after writing the result and ceasing access to the frame.
        unsafe {
            let start = self.prepare(
                invoke::<T, F>,
                (&raw mut frame).cast(),
                std::ptr::null_mut(),
            );
            let previous = GUARD.replace(self.bounds());
            hirsel_stack::switch(&raw mut frame.previous, start);
            GUARD.set(previous);
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
        let top = self.top();
        // SAFETY: the stack is idle; [low, top - HEADER) is writable, and the
        // red zone below `sp` is inside it.
        unsafe {
            let low = (*low_word(top)).max(sp.addr().saturating_sub(128));
            std::ptr::write_bytes(
                std::ptr::with_exposed_provenance_mut::<u8>(low),
                0xA5,
                top - HEADER - low,
            );
        }
    }
    /// The end of the reservation, where the low-water word sits.
    fn top(&self) -> usize {
        self.base.as_ptr().addr() + self.len
    }
    /// Bytes an idle, trimmed stack keeps writable: its guard page and first `COMMIT`.
    fn retained(&self) -> usize {
        self.len - RESERVE + COMMIT
    }
    /// Upper bound on resident bytes while this idle mapping is cached.
    pub(crate) fn committed_bytes(&self) -> usize {
        let top = self.top();
        // SAFETY: this stack is idle and its low-water word is writable.
        top - unsafe { *low_word(top) } + self.len - RESERVE
    }
    /// Return a stack that grew to its first `COMMIT` bytes. A stack that never
    /// left them costs one load and compare, inlined into recycling.
    fn trim(&self) {
        let top = self.top();
        // SAFETY: the stack is idle and its top word is always writable.
        if unsafe { *low_word(top) } < top - COMMIT {
            release(self.guard_end, top);
        }
    }

    /// # Safety
    /// The caller exclusively owns this off-CPU stack and every page below
    /// the page containing `sp` has no live frame.
    #[cfg(target_os = "linux")]
    pub(crate) unsafe fn discard_below(&self, sp: usize) {
        let top = self.top();
        let page = self.len - RESERVE;
        // SAFETY: the stack is pinned and the low-water word remains writable.
        let low = unsafe { low_word(top).read() };
        debug_assert!((low..top).contains(&sp));
        let end = sp & !(page - 1);
        if low < end {
            // SAFETY: the caller owns these complete writable pages, none of
            // which contains the cut point or the stack header.
            unsafe {
                libc::madvise(
                    std::ptr::without_provenance_mut(low),
                    end - low,
                    libc::MADV_DONTNEED,
                );
            }
        }
    }
}
/// Drop the grown pages of the idle stack whose writable range starts at
/// `guard_end` and whose reservation ends at `top`, with one fresh inaccessible
/// mapping. Out of line, and given plain addresses, so recycling a shallow
/// stack stays inlined and keeps the stack in registers.
#[cold]
#[inline(never)]
fn release(guard_end: usize, top: usize) {
    let committed = top - COMMIT;
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
            crate::process::stop(b"HostMemory");
        }
        *low_word(top) = committed;
    }
}
/// The low-water word of the stack whose reservation ends at `top`.
fn low_word(top: usize) -> *mut usize {
    std::ptr::with_exposed_provenance_mut::<usize>(top - size_of::<usize>())
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
/// Idle stacks a worker's or run's spawns reuse. A recycled stack is trimmed,
/// then kept, holding its share of the process retention allowance, or, once
/// that is spent, held as surplus. Recycling never unmaps: the surplus goes back
/// to the host when its thread has waited `RELEASE_DELAY_MS` with nothing to run
/// (`release`), the task heap's rule for emptied slabs. Spawns reuse surplus
/// first, so the stacks mapped never exceed the most that were live at once,
/// plus each worker's private batch.
pub(crate) struct Stock {
    budget: &'static Budget,
    kept: Vec<(Stack, Retained<'static>)>,
    // The prefix whose stack pages have already been returned during idle.
    cooled: usize,
    surplus: Vec<Stack>,
    // Repeated calls from one live unit reuse its last stack without locking
    // the pool. The unit gives it back with the rest of its task storage.
    recent: Option<Stack>,
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

/// Worker stock moves this many idle stacks to or from the pool's spare stock.
const SPARE_BATCH: usize = 16;

/// Idle stacks shared by a pool's workers. A fiber that ends on a thief
/// returns its stack here, so the spawning worker refills instead of mapping.
#[derive(Default)]
pub(crate) struct Spare(std::sync::Mutex<Stock>);

impl Spare {
    pub(crate) fn release(&self) {
        self.0.lock().unwrap().release();
    }
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
            recent: None,
        }
    }

    fn len(&self) -> usize {
        self.kept.len() + self.surplus.len()
    }

    pub(crate) fn take(&mut self) -> Stack {
        if let Some(memory) = crate::units::memory() {
            return self
                .recent
                .take()
                .unwrap_or_else(|| memory.take_task_stack());
        }
        if let Some(stack) = self.surplus.pop() {
            SURPLUS.fetch_sub(1, Relaxed);
            return stack;
        }
        self.kept.pop().map_or_else(Stack::new, |(stack, _)| stack)
    }

    pub(crate) fn recycle(&mut self, stack: Stack) {
        if let Some(memory) = crate::units::memory() {
            if let Some(previous) = self.recent.replace(stack) {
                memory.put_task_stack(previous);
            }
            return;
        }
        stack.trim();
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

    pub(crate) fn has_warm_kept(&self) -> bool {
        cfg!(target_os = "linux") && self.kept.len() > self.cooled
    }

    /// Keep each reservation and its low-water word. The ended task owns no
    /// frames, so the rest of its writable pages can return to the host.
    pub(crate) fn cool_kept(&mut self) {
        #[cfg(target_os = "linux")]
        {
            for (stack, _) in self.kept.iter().skip(self.cooled) {
                let page = stack.len - RESERVE;
                // SAFETY: the stock owns this idle stack. Recycling trimmed
                // it to COMMIT, and the last native page holds its low word.
                unsafe { stack.discard_below(stack.top() - page) };
            }
            self.cooled = self.kept.len();
        }
    }

    /// Move up to `count` idle stacks, surplus first, to `to`.
    fn hand(&mut self, to: &mut Self, count: usize) {
        let surplus = count.min(self.surplus.len());
        to.surplus
            .extend(self.surplus.drain(self.surplus.len() - surplus..));
        let kept = (count - surplus).min(self.kept.len());
        to.cooled = to.cooled.min(to.kept.len());
        to.kept.extend(self.kept.drain(self.kept.len() - kept..));
    }

    pub(crate) fn take_shared(&mut self, spare: &Spare) -> Stack {
        if self.len() == 0 {
            spare.0.lock().unwrap().hand(self, SPARE_BATCH);
        }
        self.take()
    }

    pub(crate) fn recycle_shared(&mut self, stack: Stack, spare: &Spare) {
        self.recycle(stack);
        if self.len() > 2 * SPARE_BATCH {
            self.hand(&mut spare.0.lock().unwrap(), SPARE_BATCH);
        }
    }
}

impl Drop for Stock {
    fn drop(&mut self) {
        if let Some(stack) = self.recent.take()
            && let Some(memory) = crate::units::memory()
        {
            memory.put_task_stack(stack);
        }
        self.release();
    }
}

struct Alternate {
    stack: Stack,
    previous: libc::stack_t,
}
impl Alternate {
    fn new() -> Self {
        let stack = crate::units::signal_stack_memory()
            .map_or_else(Stack::new, |memory| memory.signal_stack());
        // SAFETY: install owned usable pages as the current thread's signal stack.
        unsafe {
            let mut previous = std::mem::zeroed();
            let new = libc::stack_t {
                ss_sp: std::ptr::with_exposed_provenance_mut(stack.top() - COMMIT),
                ss_flags: 0,
                ss_size: COMMIT - HEADER,
            };
            if libc::sigaltstack(&new, &mut previous) != 0 {
                crate::process::stop(b"SignalStack");
            }
            Self { stack, previous }
        }
    }
}
impl Drop for Alternate {
    fn drop(&mut self) {
        // SAFETY: thread-local teardown runs with no task/signal handler active.
        unsafe {
            libc::sigaltstack(&self.previous, std::ptr::null_mut());
        }
        let _ = &self.stack;
    }
}
unsafe extern "C" fn fault(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    // SAFETY: SA_SIGINFO receives a live kernel siginfo for this fault.
    let address = unsafe { (*info).si_addr() as usize };
    let (lo, hi) = GUARD.get();
    if lo <= address && address < hi {
        crate::process::stop(b"Stack");
    }
    // A touch below the current stack's writable part commits the next GROW
    // bytes (or up to the guard page) and resumes the faulting instruction.
    if hi != 0 && hi <= address && address < hi + RESERVE {
        let top = hi + RESERVE;
        // SAFETY: the current stack's top word is always writable.
        unsafe {
            let low = low_word(top);
            if address < *low {
                let page = hi - lo;
                let from = (address & !(page - 1)).min(*low - GROW).max(hi);
                if libc::mprotect(
                    std::ptr::with_exposed_provenance_mut(from),
                    *low - from,
                    libc::PROT_READ | libc::PROT_WRITE,
                ) != 0
                {
                    crate::process::stop(b"HostMemory");
                }
                *low = from;
                return;
            }
        }
    }
    // SAFETY: chain to the process handler saved before installation; it has the
    // corresponding POSIX signature. Unrelated memory faults remain fatal.
    unsafe {
        let previous = PREVIOUS
            .get()
            .and_then(|previous| {
                FAULTS
                    .iter()
                    .position(|&fault| fault == signal)
                    .map(|index| &previous[index])
            })
            .unwrap_or_else(|| crate::process::stop(b"Signal"));
        if previous.sa_sigaction == libc::SIG_DFL || previous.sa_sigaction == libc::SIG_IGN {
            libc::sigaction(signal, previous, std::ptr::null_mut());
        } else if previous.sa_flags & libc::SA_SIGINFO != 0 {
            let next: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void) =
                std::mem::transmute(previous.sa_sigaction);
            next(signal, info, context);
        } else {
            let next: unsafe extern "C" fn(i32) = std::mem::transmute(previous.sa_sigaction);
            next(signal);
        }
    }
}

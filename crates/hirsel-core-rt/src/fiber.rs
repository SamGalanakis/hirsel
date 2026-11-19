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
use crate::retention::{RETENTION, Retained};
use std::{cell::Cell, ptr::NonNull, sync::OnceLock};
const RESERVE: usize = 8 << 20;
const COMMIT: usize = 64 << 10;
const GROW: usize = 256 << 10;
/// Bytes kept at the top for the low-water word; a multiple of 16.
const HEADER: usize = 16;
thread_local! {
    static GUARD: Cell<(usize,usize)> = const { Cell::new((0,0)) };
    static STOP_FD: Cell<i32> = const { Cell::new(2) };
    static ALTERNATE: Alternate = Alternate::new();
}
static PREVIOUS: OnceLock<libc::sigaction> = OnceLock::new();
#[cfg(test)]
#[path = "fiber_stop_tests.rs"]
mod stop_tests;
pub(crate) fn stop_fd() -> i32 {
    STOP_FD.get()
}
pub(crate) fn set_stop_fd(fd: i32) -> i32 {
    STOP_FD.replace(fd)
}
pub(crate) fn guard(bounds: (usize, usize)) {
    GUARD.set(bounds);
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
            let mut previous: libc::sigaction = std::mem::zeroed();
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            action.sa_sigaction = fault as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGSEGV, &action, &mut previous) != 0 {
                crate::process::stop(b"SignalStack");
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
    /// The end of the reservation, where the low-water word sits.
    fn top(&self) -> usize {
        self.base.as_ptr().addr() + self.len
    }
    /// Bytes an idle, trimmed stack keeps writable: its guard page and first `COMMIT`.
    fn retained(&self) -> usize {
        self.len - RESERVE + COMMIT
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
#[derive(Default)]
pub(crate) struct Stock {
    idle: Vec<(Stack, Retained<'static>)>,
}

/// Worker stock moves this many idle stacks to or from the pool's spare list.
const SPARE_BATCH: usize = 16;

/// Idle stacks shared by a pool's workers. A fiber that ends on a thief
/// returns its stack here, so the spawning worker refills instead of mapping.
#[derive(Default)]
pub(crate) struct Spare(std::sync::Mutex<Vec<(Stack, Retained<'static>)>>);

// SAFETY: an idle stack holds no live frame or thread-local address; only its
// mapping moves between workers.
unsafe impl Send for Stack {}

impl Stock {
    pub(crate) fn take(&mut self) -> Stack {
        self.idle.pop().map_or_else(Stack::new, |(stack, _)| stack)
    }

    pub(crate) fn recycle(&mut self, stack: Stack) {
        stack.trim();
        if let Some(held) = RETENTION.retain(stack.retained()) {
            self.idle.push((stack, held));
        }
    }

    pub(crate) fn take_shared(&mut self, spare: &Spare) -> Stack {
        if self.idle.is_empty() {
            let mut shared = spare.0.lock().unwrap();
            let start = shared.len().saturating_sub(SPARE_BATCH);
            self.idle.extend(shared.drain(start..));
        }
        self.take()
    }

    pub(crate) fn recycle_shared(&mut self, stack: Stack, spare: &Spare) {
        self.recycle(stack);
        if self.idle.len() > 2 * SPARE_BATCH {
            spare
                .0
                .lock()
                .unwrap()
                .extend(self.idle.drain(..SPARE_BATCH));
        }
    }
}

struct Alternate {
    stack: Stack,
    previous: libc::stack_t,
}
impl Alternate {
    fn new() -> Self {
        let stack = Stack::new();
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

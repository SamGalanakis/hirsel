//! Guarded task stacks. Both native emitters and Rust probe large frames.
//! Non-suspending foreign OS calls run on a separate guarded host stack.
//! Language code never runs on the launcher's stack.
use std::{cell::Cell, ptr::NonNull, sync::OnceLock};
const USABLE: usize = 64 << 10;
thread_local! {
    static GUARD: Cell<(usize,usize)> = const { Cell::new((0,0)) };
    static STOP_FD: Cell<i32> = const { Cell::new(2) };
    static ALTERNATE: Alternate = Alternate::new();
}
static PREVIOUS: OnceLock<libc::sigaction> = OnceLock::new();
pub(crate) fn stop_fd() -> i32 {
    STOP_FD.get()
}
pub(crate) fn set_stop_fd(fd: i32) -> i32 {
    STOP_FD.replace(fd)
}
pub(crate) fn guard(bounds: (usize, usize)) {
    GUARD.set(bounds);
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
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap()
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
        Self::with_bytes(USABLE)
    }
    pub(crate) fn with_bytes(bytes: usize) -> Self {
        let page_bytes = page();
        let len = page_bytes + bytes;
        // SAFETY: fresh inaccessible private mapping, then enable only its usable pages.
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
            if libc::mprotect(
                raw.cast::<u8>().add(page_bytes).cast(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
            ) != 0
            {
                crate::process::stop(b"HostMemory");
            }
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
            let start = self.base.as_ptr().add(self.len - size_of_val(&words));
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), start, size_of_val(&words));
            start
        }
    }
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
                ss_sp: stack.base.as_ptr().add(page()).cast(),
                ss_flags: 0,
                ss_size: USABLE,
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
    // SAFETY: chain to the process handler saved before installation; it has the
    // corresponding POSIX signature. Unrelated memory faults remain fatal.
    unsafe {
        let previous = PREVIOUS.get().unwrap();
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

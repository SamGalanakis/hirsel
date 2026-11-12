//! Native stack exhaustion is a run stop (`BudgetKind::Stack`), not a language failure: it
//! cannot be caught, and no finalizer runs after it.
//!
//! Every task runs on a fiber whose mapping keeps an inaccessible guard of
//! [`super::GUARD_BYTES`] below its usable bytes, and both engines probe any frame larger than
//! a page (Cranelift `enable_probestack`, LLVM `probe-stack`), as Rust does for the runtime. So
//! running out of stack always faults at a permission fault within a page of the interrupted
//! stack pointer, never past the guard. A thread that runs tasks gets an alternate signal
//! stack ([`arm`]); the SIGSEGV handler runs there, recognises that fault and ends the run
//! from it, exactly as a spent memory budget does ([`crate::stop::raise`]): the fiber's frames
//! are abandoned and never resumed. Any other fault goes to the handler installed before
//! this one (Rust's own overflow report for thread stacks, or the default action).
use std::sync::OnceLock;

use crate::environmental;

/// Bytes of each thread's alternate signal stack: room for the handler and the stop it
/// raises, which records the stop and switches to the launcher or the worker's loop.
const ALTERNATE_BYTES: usize = 256 << 10;
/// An access this far from the stack pointer is the stack growing: a push, a frame's
/// first store or a page probe.
const WINDOW: usize = 4096;
/// Linux's `si_code` for an access to a mapped page it may not touch (the guard).
const SEGV_ACCERR: libc::c_int = 2;

static PREVIOUS: OnceLock<libc::sigaction> = OnceLock::new();

thread_local! {
    static ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALTERNATE: Alternate = Alternate::new();
}

/// This thread is about to run tasks: install the handler (once per process) and this
/// thread's alternate stack (once per thread).
#[inline]
pub(crate) fn arm() {
    if !ARMED.with(std::cell::Cell::get) {
        arm_slow();
    }
}

#[cold]
fn arm_slow() {
    PREVIOUS.get_or_init(install);
    ALTERNATE.with(|_| ());
    ARMED.with(|armed| armed.set(true));
}

fn install() -> libc::sigaction {
    // SAFETY: plain sigaction calls with initialized arguments.
    unsafe {
        let mut previous: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut previous) != 0 {
            environmental("the stack-overflow handler could not read SIGSEGV's action");
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        let handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
            on_fault;
        action.sa_sigaction = handler as usize;
        // SA_NODEFER: the stop never returns from the handler, so SIGSEGV must not stay
        // blocked on the thread afterwards.
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGSEGV, &action, std::ptr::null_mut()) != 0 {
            environmental("the stack-overflow handler could not be installed");
        }
        previous
    }
}

/// One thread's alternate signal stack, with an inaccessible page below it.
struct Alternate {
    base: *mut u8,
}

impl Alternate {
    fn new() -> Alternate {
        let page = page_bytes();
        // SAFETY: a fresh private mapping whose lowest page is then made inaccessible; the
        // rest becomes this thread's alternate stack.
        unsafe {
            let base = libc::mmap(
                std::ptr::null_mut(),
                page + ALTERNATE_BYTES,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if base == libc::MAP_FAILED {
                environmental("a signal stack could not be mapped");
            }
            if libc::mprotect(base, page, libc::PROT_NONE) != 0 {
                environmental("a signal stack guard could not be protected");
            }
            let stack = libc::stack_t {
                ss_sp: base.cast::<u8>().add(page).cast(),
                ss_flags: 0,
                ss_size: ALTERNATE_BYTES,
            };
            if libc::sigaltstack(&stack, std::ptr::null_mut()) != 0 {
                environmental("a signal stack could not be installed");
            }
            Alternate { base: base.cast() }
        }
    }
}

impl Drop for Alternate {
    fn drop(&mut self) {
        // SAFETY: the thread is exiting; the mapping is this thread's and no handler runs.
        unsafe {
            let disable = libc::stack_t {
                ss_sp: std::ptr::null_mut(),
                ss_flags: libc::SS_DISABLE,
                ss_size: 0,
            };
            libc::sigaltstack(&disable, std::ptr::null_mut());
            libc::munmap(self.base.cast(), page_bytes() + ALTERNATE_BYTES);
        }
    }
}

fn page_bytes() -> usize {
    // SAFETY: sysconf has no preconditions.
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096)
}

unsafe extern "C" fn on_fault(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    // SAFETY: the kernel passes a valid siginfo and ucontext for an SA_SIGINFO handler.
    let (address, code, sp) = unsafe {
        (
            (*info).si_addr() as usize,
            (*info).si_code,
            stack_pointer(context.cast()),
        )
    };
    let ctx = crate::stop::running();
    if !ctx.is_null()
        && code == SEGV_ACCERR
        && address < sp.saturating_add(WINDOW)
        && sp.saturating_sub(WINDOW) <= address
    {
        // SAFETY: the faulting task's frames are never resumed: the stop leaves its fiber
        // for good, as any stop does.
        unsafe {
            crate::stop::raise(
                ctx,
                crate::stop::Stop::BudgetExceeded(hirsel_run_schema::BudgetKind::Stack),
            )
        }
    }
    // SAFETY: the previous action was read from the kernel before this handler replaced it.
    unsafe { chain(signal, info, context) }
}

/// Hand a fault that is not a task's overflow to the action installed before ours.
unsafe fn chain(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let previous = PREVIOUS.get();
    // SAFETY: as the caller; resetting to the default action makes the returning fault
    // fatal, as an uncaught SIGSEGV is.
    unsafe {
        match previous {
            Some(previous)
                if previous.sa_sigaction != libc::SIG_DFL
                    && previous.sa_sigaction != libc::SIG_IGN =>
            {
                if previous.sa_flags & libc::SA_SIGINFO != 0 {
                    let handler: unsafe extern "C" fn(
                        libc::c_int,
                        *mut libc::siginfo_t,
                        *mut libc::c_void,
                    ) = std::mem::transmute(previous.sa_sigaction);
                    handler(signal, info, context);
                } else {
                    let handler: unsafe extern "C" fn(libc::c_int) =
                        std::mem::transmute(previous.sa_sigaction);
                    handler(signal);
                }
            }
            _ => {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = libc::SIG_DFL;
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn stack_pointer(context: *const libc::ucontext_t) -> usize {
    // SAFETY: caller contract.
    unsafe { (*context).uc_mcontext.gregs[libc::REG_RSP as usize] as usize }
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
unsafe fn stack_pointer(context: *const libc::ucontext_t) -> usize {
    // SAFETY: caller contract.
    unsafe { (*context).uc_mcontext.sp as usize }
}

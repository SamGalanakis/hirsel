//! Fault handler installation, per-worker alternate signal stacks, signal
//! chaining with flags, and async-signal-safe `ucontext` redirection (EMB-3).
use super::{GROW, HEADER, Stack, commit_for_page, low_word, retired_word, try_page};
use crate::run_stop::{StackKind, Stop, find_current_slot_signal_safe};
use std::{
    cell::UnsafeCell,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    },
};

/// The signals a touch of an inaccessible stack page raises, delegated to the
/// runtime's host seam (`host::sys::FAULT_SIGNALS`).
const FAULTS: &[libc::c_int] = &crate::host::sys::FAULT_SIGNALS;
const FAULT_COUNT: usize = crate::host::sys::FAULT_SIGNALS.len();

const _: () = assert!(libc::SIG_DFL == 0);

struct ActionSlot(UnsafeCell<libc::sigaction>);
// SAFETY: each ActionSlot is written once before its address is published via
// an atomic Release store in PREVIOUS_SLOT, and never mutated afterwards.
unsafe impl Sync for ActionSlot {}

impl ActionSlot {
    const fn zeroed() -> Self {
        // SAFETY: an all-zero sigaction is valid (SIG_DFL, empty mask, zero flags).
        Self(UnsafeCell::new(unsafe { std::mem::zeroed() }))
    }
}

/// Published before `sigaction` installs Hirsel's handler.
static SAVED_INITIAL: [ActionSlot; FAULT_COUNT] = [const { ActionSlot::zeroed() }; FAULT_COUNT];
/// Published after `sigaction` only if the replaced action differed from initial.
static SAVED_REPLACED: [ActionSlot; FAULT_COUNT] = [const { ActionSlot::zeroed() }; FAULT_COUNT];
/// Constant `SIG_DFL` action installed into `PREVIOUS_SLOT` by `SA_RESETHAND`.
static SAVED_DFL: ActionSlot = ActionSlot::zeroed();
/// Atomic pointer to the active previous action for each signal in `FAULTS`.
static PREVIOUS_SLOT: [AtomicPtr<libc::sigaction>; FAULT_COUNT] =
    [const { AtomicPtr::new(ptr::null_mut()) }; FAULT_COUNT];

static ARM_LOCK: Mutex<()> = Mutex::new(());
static ARMED: AtomicBool = AtomicBool::new(false);

/// Diagnostic counters for the room-check laws (EMB-3).
static GUARD_HITS: AtomicUsize = AtomicUsize::new(0);
static GROWTH_FAULTS: AtomicUsize = AtomicUsize::new(0);

pub fn guard_hits() -> usize {
    GUARD_HITS.load(Ordering::Relaxed)
}

pub fn growth_faults() -> usize {
    GROWTH_FAULTS.load(Ordering::Relaxed)
}

pub fn reset_fault_counters() {
    GUARD_HITS.store(0, Ordering::Relaxed);
    GROWTH_FAULTS.store(0, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmError {
    SignalStack,
    FaultHandlerReplaced,
}

impl From<ArmError> for Stop {
    fn from(err: ArmError) -> Self {
        match err {
            ArmError::SignalStack => Stop::from_static(b"SignalStack"),
            ArmError::FaultHandlerReplaced => Stop::from_static(b"FaultHandlerReplaced"),
        }
    }
}

fn sigactions_equal(a: &libc::sigaction, b: &libc::sigaction) -> bool {
    if a.sa_sigaction != b.sa_sigaction || a.sa_flags != b.sa_flags {
        return false;
    }
    // SAFETY: `sigset_t` is plain POD bitmask bytes on Linux and Darwin.
    unsafe {
        let len = size_of::<libc::sigset_t>();
        let a_bytes = std::slice::from_raw_parts((&raw const a.sa_mask).cast::<u8>(), len);
        let b_bytes = std::slice::from_raw_parts((&raw const b.sa_mask).cast::<u8>(), len);
        a_bytes == b_bytes
    }
}

fn is_hirsel_handler(action: &libc::sigaction) -> bool {
    action.sa_sigaction == fault as *const () as usize && (action.sa_flags & libc::SA_SIGINFO) != 0
}

/// Install Hirsel's fault handler once per process, publishing the previous
/// action BEFORE calling `sigaction`. On subsequent calls, verify Hirsel's
/// handler is still installed and refuse with `FaultHandlerReplaced` otherwise,
/// without reinstalling or modifying any signal disposition.
///
/// Never touches the calling thread's alternate signal stack (EMB-3).
pub fn arm_embedded() -> Result<(), ArmError> {
    let _guard = ARM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if ARMED.load(Ordering::Acquire) {
        for &signal in FAULTS {
            // SAFETY: querying current disposition without modifying it.
            let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(signal, ptr::null(), &mut current) } != 0 {
                return Err(ArmError::SignalStack);
            }
            if !is_hirsel_handler(&current) {
                return Err(ArmError::FaultHandlerReplaced);
            }
        }
        return Ok(());
    }

    // SAFETY: initialized sigaction structures and a signal-compatible handler.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        action.sa_sigaction = fault as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);

        for (i, &signal) in FAULTS.iter().enumerate() {
            let mut current: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, ptr::null(), &mut current) != 0 {
                return Err(ArmError::SignalStack);
            }
            // Publish the observed previous action with Release BEFORE installing
            // Hirsel's handler so a concurrent fault on another thread never sees
            // an unpublished slot.
            SAVED_INITIAL[i].0.get().write(current);
            PREVIOUS_SLOT[i].store(SAVED_INITIAL[i].0.get(), Ordering::Release);

            let mut replaced: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, &action, &mut replaced) != 0 {
                PREVIOUS_SLOT[i].store(ptr::null_mut(), Ordering::Release);
                return Err(ArmError::SignalStack);
            }
            if !sigactions_equal(&current, &replaced) {
                SAVED_REPLACED[i].0.get().write(replaced);
                PREVIOUS_SLOT[i].store(SAVED_REPLACED[i].0.get(), Ordering::Release);
            }
        }
    }
    ARMED.store(true, Ordering::Release);
    Ok(())
}

/// Standalone / internal `arm`: installs the process fault handler, and outside
/// a contained embedded run also ensures a 64 KiB alternate signal stack on the
/// calling thread (matching standalone pre-embed behavior for direct tests and
/// standalone main threads). Embedded host entry calls [`arm_embedded`] directly
/// and never touches the host thread's alternate signal stack.
pub(crate) fn arm() -> Result<(), Stop> {
    arm_embedded().map_err(Stop::from)?;
    if !crate::run_stop::is_contained() && !HAS_ALTERNATE.get() {
        thread_local! {
            static STANDALONE_ALTERNATE: std::cell::RefCell<Option<Alternate>> =
                const { std::cell::RefCell::new(None) };
        }
        STANDALONE_ALTERNATE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                match Alternate::install() {
                    Ok(alt) => *slot = Some(alt),
                    Err(stop) => return Err(stop),
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Per-worker alternate signal stack RAII guard. Installs an owned signal stack
/// when a runtime worker starts and restores the previous one when it exits.
pub struct Alternate {
    _stack: Stack,
    previous: libc::stack_t,
}

thread_local! {
    static HAS_ALTERNATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Alternate {
    /// Install an owned alternate signal stack on the current thread and
    /// restore the previous one on drop.
    pub fn install() -> Result<Self, Stop> {
        let commit = commit_for_page(try_page()?);
        let stack = Stack::try_new_with_reserve(commit)?;
        // SAFETY: install owned usable pages as the current thread's signal stack.
        unsafe {
            let mut previous: libc::stack_t = std::mem::zeroed();
            let new = libc::stack_t {
                ss_sp: ptr::with_exposed_provenance_mut(stack.top() - commit),
                ss_flags: 0,
                ss_size: commit - HEADER,
            };
            if libc::sigaltstack(&new, &mut previous) != 0 {
                return Err(Stop::from_static(b"SignalStack"));
            }
            HAS_ALTERNATE.set(true);
            Ok(Self {
                _stack: stack,
                previous,
            })
        }
    }

    /// Install an alternate signal stack only if this thread does not already
    /// have one installed by its worker entry.
    pub(crate) fn install_if_needed() -> Result<Option<Self>, Stop> {
        if HAS_ALTERNATE.get() {
            Ok(None)
        } else {
            Self::install().map(Some)
        }
    }
}

impl Drop for Alternate {
    fn drop(&mut self) {
        HAS_ALTERNATE.set(false);
        // SAFETY: worker exit runs with no task or signal handler active.
        // On Linux/Darwin, if the previous stack was disabled (SS_DISABLE),
        // passing it back restores disabled state cleanly.
        unsafe {
            let mut prev = self.previous;
            if (prev.ss_flags & libc::SS_DISABLE) != 0 {
                prev.ss_sp = ptr::null_mut();
                prev.ss_size = 0;
            }
            libc::sigaltstack(&prev, ptr::null_mut());
        }
    }
}

fn fault_index(signal: libc::c_int) -> Option<usize> {
    FAULTS.iter().position(|&armed| armed == signal)
}

/// Distinguish synchronous hardware faults from software-raised signals
/// (`kill`, `raise`, `pthread_kill`, `sigqueue`).
fn is_hardware_fault(signal: libc::c_int, info: *const libc::siginfo_t) -> bool {
    crate::host::sys::is_hardware_fault(signal, info)
}

/// Rewrite the interrupted `ucontext` so returning from the signal handler
/// resumes at `hirsel_stack::resume_saved` on the worker's own host stack.
///
/// # Safety
/// `context` is the live `ucontext_t` passed by the kernel, and `resume_sp`
/// points at a live frame saved by `hirsel_stack::switch`.
unsafe fn rewrite_ucontext(context: *mut libc::c_void, resume_sp: usize) -> bool {
    let resume_pc = hirsel_stack::resume_saved as *const () as usize;
    // SAFETY: delegated to the host-seam ucontext helper.
    unsafe { crate::host::sys::rewrite_ucontext(context, resume_sp, resume_pc) }
}

/// Redirect a recoverable fiber fault to the worker's stop entry via `ucontext`
/// rewrite, or fall back to standalone uncontained stop if no worker stop entry
/// is active.
#[cold]
#[inline(never)]
unsafe fn redirect_recoverable_stop(
    slot: &crate::run_stop::WorkerFaultSlot,
    context: *mut libc::c_void,
    name: &'static [u8],
) {
    let generation = slot.generation.load(Ordering::Acquire);
    let idle_ptr = slot.idle_ptr.load(Ordering::Relaxed);
    if !generation.is_null() && !idle_ptr.is_null() {
        // SAFETY: ExecutionScope pins `generation` and `dispatch_serial` pins `idle_ptr`.
        unsafe {
            let resume_sp = (*idle_ptr).addr();
            if resume_sp != 0 && rewrite_ucontext(context, resume_sp) {
                (*generation).record_stop_bytes(name);
                slot.set_guard(0, 0, 0, StackKind::None);
                return;
            }
        }
    }
    // No active worker stop entry (e.g. standalone direct unit test):
    // standalone process stop via process.rs.
    crate::process::uncontained_stop(Stop::from_static(name));
}

#[cold]
#[inline(never)]
unsafe fn chain_previous(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
    hardware_fault: bool,
) {
    let Some(idx) = fault_index(signal) else {
        crate::process::uncontained_stop(Stop::from_static(b"Signal"));
    };
    let prev_slot = &PREVIOUS_SLOT[idx];
    let mut prev_ptr = prev_slot.load(Ordering::Acquire);
    if prev_ptr.is_null() {
        crate::process::uncontained_stop(Stop::from_static(b"Signal"));
    }

    // SAFETY: every published pointer in PREVIOUS_SLOT points to a static
    // ActionSlot that is never mutated after publication.
    unsafe {
        // Loop at most once if a concurrent SA_RESETHAND CAS loses the race.
        loop {
            let previous = *prev_ptr;
            if previous.sa_sigaction == libc::SIG_IGN {
                if !hardware_fault {
                    // Software-raised signal with SIG_IGN: return immediately
                    // and leave Hirsel's handler installed (EMB-3).
                    return;
                }
                // Ignoring a synchronous hardware fault is UB in POSIX; treat
                // as SIG_DFL below.
            }
            if previous.sa_sigaction == libc::SIG_DFL || previous.sa_sigaction == libc::SIG_IGN {
                let dfl = SAVED_DFL.0.get();
                libc::sigaction(signal, dfl, ptr::null_mut());
                if !hardware_fault {
                    // Software-raised signal has no faulting instruction to
                    // repeat: unblock and re-raise on the current thread.
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigaddset(&mut set, signal);
                    libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, ptr::null_mut());
                    libc::pthread_kill(libc::pthread_self(), signal);
                }
                return;
            }

            // Custom handler: honor SA_RESETHAND atomically while keeping
            // Hirsel's handler installed.
            if (previous.sa_flags & libc::SA_RESETHAND) != 0 {
                let dfl = SAVED_DFL.0.get();
                if let Err(newer) =
                    prev_slot.compare_exchange(prev_ptr, dfl, Ordering::AcqRel, Ordering::Acquire)
                {
                    prev_ptr = newer;
                    continue;
                }
            }

            // Apply previous.sa_mask (plus `signal` unless SA_NODEFER), then
            // restore the prior thread mask on return.
            let mut block_mask = previous.sa_mask;
            if (previous.sa_flags & libc::SA_NODEFER) == 0 {
                libc::sigaddset(&mut block_mask, signal);
            }
            let mut saved_mask: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, &block_mask, &mut saved_mask);
            if (previous.sa_flags & libc::SA_NODEFER) != 0
                && libc::sigismember(&previous.sa_mask, signal) == 0
            {
                let mut unblock_self: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut unblock_self);
                libc::sigaddset(&mut unblock_self, signal);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblock_self, ptr::null_mut());
            }

            if (previous.sa_flags & libc::SA_SIGINFO) != 0 {
                let next: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void) =
                    std::mem::transmute(previous.sa_sigaction);
                next(signal, info, context);
            } else {
                let next: unsafe extern "C" fn(i32) = std::mem::transmute(previous.sa_sigaction);
                next(signal);
            }

            libc::pthread_sigmask(libc::SIG_SETMASK, &saved_mask, ptr::null_mut());
            return;
        }
    }
}

pub(super) unsafe extern "C" fn fault(
    signal: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let hardware_fault = is_hardware_fault(signal, info);
    if hardware_fault && let Some(slot) = find_current_slot_signal_safe() {
        let (lo, hi, top, kind) = slot.get_guard();
        if kind != StackKind::None && hi != 0 {
            // SAFETY: SA_SIGINFO receives a live kernel siginfo for this fault.
            let address = unsafe { (*info).si_addr() as usize };
            // Hard guard page hit: [lo, hi).
            if lo <= address && address < hi {
                if kind == StackKind::Recoverable {
                    GUARD_HITS.fetch_add(1, Ordering::Relaxed);
                    // SAFETY: redirects recoverable Hirsel fiber to worker stop entry.
                    unsafe { redirect_recoverable_stop(slot, context, b"Stack") };
                    return;
                }
                // Non-recoverable native foreign stack: never rewrite ucontext;
                // forward to the host's previous fault action (EMB-3 item 3).
                // SAFETY: forwarded to saved host action.
                unsafe { chain_previous(signal, info, context, true) };
                return;
            }
            // Uncommitted growth band: [hi, low).
            if hi <= address && address < top {
                // SAFETY: the active stack's top header page is always writable.
                unsafe {
                    let low = low_word(top);
                    if address < *low {
                        GROWTH_FAULTS.fetch_add(1, Ordering::Relaxed);
                        let page_bytes = hi - lo;
                        let from = (address & !(page_bytes - 1))
                            .min((*low).saturating_sub(GROW))
                            .max(hi);
                        if libc::mprotect(
                            ptr::with_exposed_provenance_mut(from),
                            *low - from,
                            libc::PROT_READ | libc::PROT_WRITE,
                        ) == 0
                        {
                            *low = from;
                            return;
                        }
                        // Refused stack growth: never advance *low, never retry.
                        if kind == StackKind::Recoverable {
                            // Retire this stack whole (never trim or recycle).
                            (*retired_word(top)).store(1, Ordering::Release);
                            redirect_recoverable_stop(slot, context, b"HostMemory");
                            return;
                        }
                        // Non-recoverable native foreign stack: forward to host action.
                        chain_previous(signal, info, context, true);
                        return;
                    }
                }
            }
        }
    }
    // SAFETY: unrelated fault or software-raised signal chains to saved action.
    unsafe { chain_previous(signal, info, context, hardware_fault) };
}

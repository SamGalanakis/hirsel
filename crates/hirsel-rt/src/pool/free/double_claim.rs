//! Native TSan mutation of Queued<Image>'s exclusive claim. A second scoped
//! worker also claims the first job's saved mapper image and runs it as an
//! ordinary owner: on its own stack, saved to its own idle, through the
//! annotated switch. The owners share only the job's context and data, never a
//! stack or a return path. No duplicate reaches cleanup: the campaign requires
//! TSan exit 66.
use super::{Image, Queued};
use crate::TaskCtx;
use crate::tasks::ordered::ahead::native::Physical;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Barrier, Mutex, OnceLock};

#[derive(Default)]
struct State {
    ctx: usize,
    stolen: bool,
    sp: usize,
    ordinary: bool,
}
static STATE: Mutex<State> = Mutex::new(State {
    ctx: 0,
    stolen: false,
    sp: 0,
    ordinary: false,
});
/// Both owners hold the image: the duplicate has copied it, the victim not yet resumed it.
static ENTER: Barrier = Barrier::new(2);
/// The victim is resuming. The duplicate starts on this event, inside the
/// victim's first mapper quantum rather than a futex wake later.
static GO: AtomicBool = AtomicBool::new(false);
/// The points of one mapper quantum, as the victim's quota-only return grants.
const QUANTUM: i64 = 1024;

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("HIRSEL_TSAN_DOUBLE_CLAIM").is_some())
}

/// # Safety
/// A queued CPU job, on a scoped worker. Only the sanitizer campaign enables
/// this intentionally invalid second owner; TSan must terminate before return.
pub(super) unsafe fn dispatch(queued: &Queued<Image>) {
    if !enabled() {
        return;
    }
    let ctx = unsafe { (*queued.tsan_unclaimed()).ctx };
    if !unsafe { super::super::cpu_scoped::tsan_job(ctx) } {
        return;
    }
    let mut state = STATE.lock().unwrap();
    if state.ctx == 0 {
        // SAFETY: the queue token has not been started. Keep only its context
        // address, not a borrow of the image, across the real owner's start.
        state.ctx = ctx as usize;
    }
}

/// An idle scoped worker may duplicate only an already saved mapper. Blocking
/// before its first dispatch would prevent the canonical startup rendezvous.
/// # Safety
/// Called on a scoped worker's idle stack, without a scheduler role or borrow.
pub(crate) unsafe fn idle() {
    if !enabled() {
        return;
    }
    let mut state = STATE.lock().unwrap();
    if state.sp == 0 || state.stolen {
        return;
    }
    state.stolen = true;
    let ctx = state.ctx as *mut TaskCtx;
    let sp = state.sp as *mut u8;
    drop(state);
    // An Ordered image returns through its Physical. The duplicate gets its
    // own, so its quanta end on this idle stack rather than the victim's.
    let shared = unsafe { (*ctx).ahead_physical };
    let own = (!shared.is_null()).then(|| {
        Box::into_raw(Box::new(Physical {
            points_left: QUANTUM,
            saved_sp: std::ptr::null_mut(),
            idle_sp: std::ptr::null_mut(),
            reason: 0,
            reserved_zero: 0,
        }))
    });
    // SAFETY: deliberately violated by this mutant. A real scoped worker takes
    // the victim's saved image without the Queued CAS or Option::take; the
    // victim waits at ENTER, so the image is intact until it is copied.
    let mut sp = unsafe { duplicate(ctx.cast(), sp, shared, own.unwrap_or(shared)) };
    ENTER.wait();
    while !GO.load(Ordering::Acquire) {
        std::hint::spin_loop();
    }
    let Some(own) = own else {
        let mut idle = std::ptr::null_mut();
        // SAFETY: the copy is a suspended fiber on a stack no other switch resumes.
        unsafe { crate::fiber::switch(&raw mut idle, sp) };
        crate::fault("double-claimed mapper returned without a TSan report");
    };
    // Each ended quantum is refilled, as the victim's quota-only return is,
    // until TSan reports the owners' overlapping writes.
    loop {
        // SAFETY: as above; the copy's handoffs save to and resume `own`.
        unsafe { crate::fiber::switch(&raw mut (*own).idle_sp, sp) };
        unsafe {
            sp = (*own).saved_sp;
            (*own).points_left = QUANTUM;
        }
    }
}

/// The saved image `[sp, ctx)` of the victim's stack, copied to the same
/// offset below a fresh stack's context slot. Words that address the copied
/// range move with it, so frame chains and local addresses stay on the copy,
/// and words naming the victim's `shared` return state name `own` instead.
/// The context and everything else it reaches stay shared.
/// # Safety
/// `sp` is a suspended fiber of the task stack whose context slot is `ctx`.
unsafe fn duplicate(
    ctx: *mut u8,
    sp: *mut u8,
    shared: *mut Physical,
    own: *mut Physical,
) -> *mut u8 {
    let length = (ctx as usize).wrapping_sub(sp as usize);
    if sp >= ctx || length > crate::fiber::STACK_BYTES {
        crate::fault("a double-claimed image is not below its task context");
    }
    // Never dropped: no duplicate returns, and its fiber stays registered.
    let stack = std::mem::ManuallyDrop::new(crate::fiber::Stack::new());
    let copy = stack.context().cast::<u8>().wrapping_sub(length);
    let delta = (copy as usize).wrapping_sub(sp as usize);
    // SAFETY: both ranges are `length` mapped bytes of distinct stacks.
    unsafe { copy.copy_from_nonoverlapping(sp, length) };
    let words = copy.cast::<usize>();
    for i in 0..length / 8 {
        // SAFETY: within the copy, which `sp` aligned to 8 bytes.
        let word = unsafe { &mut *words.add(i) };
        if (sp as usize..ctx as usize).contains(word) {
            *word = word.wrapping_add(delta);
        } else if *word == shared as usize {
            *word = own as usize;
        }
    }
    copy
}

/// Publish the victim's first saved scalar mapper quantum. Deferring the
/// duplicate until here avoids mistaking a scheduler-metadata race for emitted
/// mapper coverage. No job contents, stack bytes or mapper PCs are fabricated.
/// # Safety
/// The legitimate idle owner holds this saved mapper stack, before resumption.
pub(crate) unsafe fn mapper(ctx: *mut TaskCtx, sp: *mut u8) {
    if !enabled() {
        return;
    }
    let mut state = STATE.lock().unwrap();
    if state.ctx != ctx as usize || state.sp != 0 {
        return;
    }
    state.sp = sp as usize;
    drop(state);
    super::super::cpu_scoped::tsan_wake(super::super::execs(ctx));
    ENTER.wait();
    GO.store(true, Ordering::Release);
}

#[repr(C)]
pub(crate) struct Switch {
    save: *mut *mut u8,
    to: *mut u8,
}

/// Save the ordinary CPU worker at its first mapper fuel refill. Free has no
/// Ordered claim receipt; the emitted-PC oracle verifies the resumed mapper.
/// # Safety
/// The running context owns this worker. The naked caller must tail-jump to
/// fiber_switch after refill returns, without retaining any Rust frame.
pub(crate) unsafe extern "C" fn ordinary_prepare(ctx: *mut TaskCtx) -> Switch {
    let none = Switch {
        save: std::ptr::null_mut(),
        to: std::ptr::null_mut(),
    };
    if !enabled() || !unsafe { (*ctx).ahead_physical.is_null() } {
        return none;
    }
    let mut state = STATE.lock().unwrap();
    if state.ctx != ctx as usize || state.sp != 0 || state.ordinary {
        return none;
    }
    state.ordinary = true;
    Switch {
        save: unsafe { &raw mut (*ctx).task_sp },
        to: unsafe { (*super::super::own_exec()).idle.get() },
    }
}

/// Resume the ordinary save without publishing a wake or relinquishing Running.
/// # Safety
/// The dispatcher is on idle and still exclusively owns this context's image.
pub(super) unsafe fn ordinary_saved(ctx: *mut TaskCtx) -> Option<*mut u8> {
    if !enabled() {
        return None;
    }
    {
        let state = STATE.lock().unwrap();
        if state.ctx != ctx as usize || !state.ordinary || state.sp != 0 {
            return None;
        }
    }
    let sp = unsafe { (*ctx).task_sp };
    unsafe { mapper(ctx, sp) };
    Some(sp)
}

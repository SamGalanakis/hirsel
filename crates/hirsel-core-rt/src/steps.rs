//! MEM-30 step budgets and slice counters belong to one task.
use crate::{Context, causes};
use std::sync::atomic::{AtomicU64, Ordering};

pub const UNBOUNDED: i64 = 1 << 62;
pub const SLICE: u64 = crate::abi_layout::STEP_SLICE as u64;
static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Bound {
    id: u64,
    left: u64,
}

fn bound(left: u64) -> Bound {
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .unwrap_or_else(|_| crate::process::stop(b"StepIds"));
    Bound { id, left }
}

// The active bound and slice hold the tails beyond the emitter's common window.
// Switching tasks leaves all three words intact.
fn window_left(context: &Context) -> u64 {
    context.steps.max(0) as u64
}

fn install_window(context: &mut Context, slice: u64) {
    if let Some(current) = context.step_bounds.last_mut() {
        let window = current.left.min(slice);
        current.left -= window;
        context.step_slice_tail = slice - window;
        context.steps = window as i64;
    } else {
        context.step_slice_tail = slice;
        context.steps = UNBOUNDED;
    }
}

pub(crate) fn inherit(child: &mut Context, parent: &Context) {
    if let Some(current) = parent.step_bounds.last() {
        child
            .step_bounds
            .push(bound(current.left + window_left(parent)));
        install_window(child, SLICE);
    }
}

pub(crate) fn is_active(context: &Context, id: u64) -> bool {
    context.step_bounds.iter().any(|bound| bound.id == id)
}

fn leave(context: &mut Context) {
    let window = window_left(context);
    let left = context.step_bounds.pop().expect("an open step scope").left + window;
    if let Some(parent) = context.step_bounds.last_mut() {
        parent.left += left;
    }
    let slice = context.step_slice_tail + window;
    install_window(context, slice);
}

/// # Safety
/// The current task owns context exclusively; its native counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_enter(context: *mut Context, steps: u64) -> bool {
    // SAFETY: the caller provides its live exclusive task context.
    let context = unsafe { &mut *context };
    let window = window_left(context);
    let mut slice = context.step_slice_tail;
    if let Some(current) = context.step_bounds.last_mut() {
        let left = current.left + window;
        if left < steps {
            return false;
        }
        current.left = left - steps;
        slice += window;
    }
    context.step_bounds.push(bound(steps));
    install_window(context, slice);
    true
}

/// # Safety
/// The current task is closing its innermost lexical step scope.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_leave(context: *mut Context) {
    // SAFETY: the caller owns this task and an entered scope.
    leave(unsafe { &mut *context });
}

/// # Safety
/// Cleanup finished under the current exhausted scope; its counter is flushed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_catch(context: *mut Context) -> bool {
    // SAFETY: the caller owns this task and an entered scope.
    let context = unsafe { &mut *context };
    let id = context.step_bounds.last().expect("an open step scope").id;
    let caught = context.causes.iter().next().is_some()
        && context
            .causes
            .iter()
            .all(|c| c.kind() == causes::OUT_OF_TIME && c.message.data.addr() as u64 == id);
    if caught {
        let mut chain = std::mem::take(&mut context.causes);
        let obligation = if context.active_resource.is_null() {
            &mut context.primary
        } else {
            // SAFETY: an active resource is the live current finalizer's obligation.
            unsafe { &mut (*context.active_resource).cell }
        };
        if obligation.is_none() {
            let mut cell = chain.take_first().expect("a caught chain's primary cell");
            cell.cause = causes::Cause::default();
            *obligation = Some(cell);
        }
        drop(chain);
    }
    leave(context);
    caught
}

/// # Safety
/// Called immediately after a source Step decremented the flushed window to <= 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_step_out(context: *mut Context, site: u64) -> u8 {
    crate::sequencer::end_receive_run();
    // SAFETY: the engine supplies the exclusively running task's context.
    let ctx = unsafe { &mut *context };
    if ctx.steps < 0
        && let Some(current) = ctx.step_bounds.last()
    {
        ctx.steps = 0;
        let mut cause = causes::Cause::new(causes::OUT_OF_TIME, site);
        cause.message.data = std::ptr::without_provenance_mut(current.id as usize);
        // SAFETY: the failed step owns the active obligation's primary cell.
        unsafe { causes::record(ctx, cause) };
        return 1;
    }
    let slice_end = ctx.step_slice_tail == 0;
    let slice = if slice_end {
        SLICE
    } else {
        ctx.step_slice_tail
    };
    install_window(ctx, slice);
    if slice_end {
        let _commit = crate::tasks::before_commit();
        crate::tasks::event(hirsel_core_layout::event::STEP_SLICE, 0, SLICE);
        // SAFETY: the step window is installed and no borrow spans the switch.
        unsafe { crate::tasks::checkpoint() };
    }
    0
}

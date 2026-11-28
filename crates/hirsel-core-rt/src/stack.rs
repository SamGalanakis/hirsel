//! FAIL-17: the task stack's limit word and the `Stack` refusal.
//!
//! Every activation of user code starts with `stack_check`: when the stack
//! pointer, the activation's frame already made, lies below
//! [`Context::stack_limit`], the engine calls [`hirsel_core_stack_out`] and the
//! activation fails at its entry. The numbers are in
//! [`hirsel_core_layout::stack`].
use crate::{Context, causes};
use hirsel_core_layout::stack::{ALLOWANCE, MARGIN};

/// Install the build's allowance before a task runs on a newly mounted stack.
pub(crate) fn commit(context: &mut Context, top: usize) {
    let limit = top - ALLOWANCE as usize;
    context.stack_limit = limit;
    context.stack_floor = limit;
}

/// Narrow the limit to at least `limit` and return the limit to restore.
/// An `isolate` saves the limit at entry with `limit` 0 and restores it when
/// it returns or catches, which takes back a cleanup margin its body's
/// refusal released.
#[inline]
pub fn tighten(context: &mut Context, limit: usize) -> usize {
    let saved = context.stack_limit;
    context.stack_limit = saved.max(limit);
    saved
}

/// Restore a limit [`tighten`] returned.
#[inline]
pub fn restore(context: &mut Context, saved: usize) {
    context.stack_limit = saved;
}

/// Record the refusal of an activation's entry: a `Stack` cause at `site`.
/// The first refusal releases the cleanup margin below the limit, so the
/// finalizers of the failure's cleanup can run; a finalizer that needs more
/// is refused against the lowered limit and adds its own `Stack` cause.
/// Returns 1, the failed status.
///
/// # Safety
/// `context` is the running task's context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_stack_out(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the engine supplies the exclusively running task's context.
    let ctx = unsafe { &mut *context };
    if crate::run_stop::is_ended() {
        crate::run_stop::leave_on_ended();
    }
    if ctx.stack_limit != 0 && ctx.stack_limit >= ctx.stack_floor {
        ctx.stack_limit = ctx.stack_floor - MARGIN as usize;
    }
    // SAFETY: the refused entry owns the active obligation's primary cell.
    unsafe { causes::record(ctx, causes::Cause::new(causes::STACK, site)) };
    1
}

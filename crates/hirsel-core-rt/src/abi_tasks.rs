//! Native task/channel ABI and the shared memory lane's custody hooks.
//! Typed FORM rows bind the concrete layouts/drop callbacks declared here.
use crate::Context;

/// The one cross-task memory path. Spawn has admitted record + cell + result +
/// reservation as one charge. Neither attaching nor hand-over admits again.
pub(crate) fn task_child(
    parent: *const Context,
    record: &mut crate::block::Block,
    bytes: u64,
    task: u64,
) -> Context {
    let reservation = record.partition(bytes).prepaid(bytes);
    reservation.task_owner(task);
    let primary = record.partition(hirsel_core_layout::charge::CAUSE_CELL);
    let mut context = Context::with_funding(reservation);
    context.primary = Some(crate::causes::Cell::prepaid(primary));
    // SAFETY: spawner and child share the same run-owned provider configuration.
    context.run = unsafe { (*parent).run };
    context
}
/// # Safety
/// Child body/cleanup completed, all bound scopes exited, and this is its only end.
pub(crate) unsafe fn task_handover(context: *mut Context) {
    // SAFETY: child's remaining funding forwards to its live spawner without a value walk.
    unsafe {
        (*context).current.end();
    }
}
/// Task-local representations drain pending foreign refunds here before pure code
/// can attempt another admission. The current synchronized forest settles them
/// directly at refund; no pending queue is needed in this representation.
pub(crate) fn task_resume(_: *mut Context) {}

/// One slow path for cross-task refunds, after updating the live payer. Local
/// refund does not invoke a provider. No allocation, no suspended callback.
pub(crate) fn cross_task_refund(owner: u64, bytes: u64) {
    if bytes != 0 && owner != crate::tasks::current_id() {
        crate::tasks::event(crate::tasks::REFUND, owner, bytes);
    }
}

pub use crate::channel::{
    hirsel_core_channel_new, hirsel_core_channel_sender, hirsel_core_receive,
    hirsel_core_receiver_drop, hirsel_core_send, hirsel_core_sender_drop, hirsel_core_try_send,
};
pub use crate::tasks::{
    hirsel_core_cancel, hirsel_core_check_cancel, hirsel_core_join, hirsel_core_spawn,
    hirsel_core_task_drop,
};

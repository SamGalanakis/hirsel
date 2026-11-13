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
    // SAFETY: every task borrows the root run's provider-file registry.
    context.host_files = unsafe { (*parent).host_files };
    context
}
/// # Safety
/// Child body/cleanup completed, all bound scopes exited, and this is its only end.
pub(crate) unsafe fn task_handover(context: *mut Context) {
    // SAFETY: child's remaining funding forwards to its live spawner without a value walk.
    unsafe {
        (*context).current.handover();
    }
}
/// Drain completed remote refunds before this task resumes pure code.
pub(crate) fn task_resume(context: *mut Context) {
    // SAFETY: the cooperative scheduler owns this task's exclusive execution.
    unsafe {
        (*context).current.bind();
        (*context).current.drain();
        (*context).current.normalize();
    }
}

/// One slow path for cross-task refunds, before publishing to the payer mailbox. Local
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

/// Dispatch the provider event after an emitted Arc increment.
/// # Safety
/// Node is a live initialized Arc under the provider's operation order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_arc_shared(node: *const u8) {
    // SAFETY: caller just completed the increment on this live Arc.
    let count = unsafe {
        (&*node.cast::<std::sync::atomic::AtomicU64>()).load(std::sync::atomic::Ordering::Relaxed)
    };
    crate::tasks::event(crate::tasks::ARC_SHARE, 0, count);
}
/// Dispatch the provider event after an emitted Arc decrement. Admits nothing
/// and has no failure status; last-release traversal has not begun yet.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_arc_released(last: bool) {
    crate::tasks::event(crate::tasks::ARC_RELEASE, 0, u64::from(last));
}
/// Dispatch the provider event after an emitted Arc uniqueness test.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_arc_unwrapped(unique: bool) {
    crate::tasks::event(crate::tasks::ARC_UNWRAP, 0, u64::from(unique));
}

/// Exclusive execution of the payer's entire table, including queued refunds.
/// The current scheduler has one OS worker and callbacks here cannot suspend,
/// so its running turn already holds the lease. A multiworker scheduler must
/// acquire the payer domain before invoking this closure, quiesce every task
/// sharing that domain, and retain the lease until the closure returns. It must
/// also hold it for a received finalizer's full body, even across suspension.
#[inline]
pub(crate) fn payer_lease<R>(operation: impl FnOnce() -> R) -> R {
    operation()
}

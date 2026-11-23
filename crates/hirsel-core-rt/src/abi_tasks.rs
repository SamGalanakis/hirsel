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
    share_heap: bool,
) -> Context {
    let reservation = record.partition(bytes).prepaid(bytes);
    reservation.task_owner(task);
    let primary = record.partition(hirsel_core_layout::charge::CAUSE_CELL);
    let mut context = Context::with_funding(reservation);
    if share_heap {
        // SAFETY: the serial dispatcher runs both tasks, and the spawn
        // record has already initialized their heap in the parent.
        let heap = unsafe { (*parent).heap.get().unwrap() };
        context.share_heap(heap.clone());
    }
    context.primary = Some(crate::causes::Cell::prepaid(primary));
    // SAFETY: the running parent owns its flushed step counter.
    crate::steps::inherit(&mut context, unsafe { &*parent });
    // SAFETY: spawner and child share the same run-owned provider configuration.
    context.run = unsafe { (*parent).run };
    // SAFETY: the immutable key was set before the parent started.
    context.set_map_keys(unsafe { (*parent).map_keys() });
    // SAFETY: child inherits recording; spawn replaces the pointer before publication.
    context.events = unsafe { (*parent).events };
    // SAFETY: every task borrows the root run's provider-file registry.
    context.host_files = unsafe { (*parent).host_files };
    // SAFETY: the same run pins its foreign state until the last child ends.
    context.foreign = unsafe { (*parent).foreign };
    context
}
/// # Safety
/// Child body/cleanup completed, all bound scopes exited, and this is its only end.
pub(crate) unsafe fn task_handover(context: *mut Context) {
    // SAFETY: child's remaining funding forwards to its live spawner without a value walk.
    unsafe {
        (*context).current.handover(crate::tasks::current_id());
    }
}
/// Rebind task custody without making foreign refunds visible. Only the
/// owning task's semantic events drain its prepaid refund mailboxes (MEM-20).
#[inline(always)]
pub(crate) fn task_resume(context: *mut Context) {
    // SAFETY: the cooperative scheduler owns this task's exclusive execution.
    unsafe {
        (*context).current.bind();
        (*context).current.normalize();
    }
}

pub use crate::channel::{
    hirsel_core_channel_new, hirsel_core_channel_sender, hirsel_core_receive,
    hirsel_core_receiver_drop, hirsel_core_send, hirsel_core_sender_drop, hirsel_core_try_receive,
    hirsel_core_try_send,
};
pub use crate::latest::{
    hirsel_core_latest_drop, hirsel_core_latest_handle, hirsel_core_latest_new,
    hirsel_core_latest_publish, hirsel_core_latest_read,
};
pub use crate::tasks::{
    hirsel_core_cancel, hirsel_core_check_cancel, hirsel_core_join, hirsel_core_spawn,
    hirsel_core_task_drop,
};

/// Poll events: CHECK_CANCEL and CAPACITY. A Capacity
/// snapshot is recorded before the scheduling turn, admits nothing and observes
/// no cancellation. The cold hook is shared by normal and scope admission.
/// Owning-task events alone publish foreign refunds (MEM-20); task_resume only
/// rebinds custody. abi_layout.rs and the context's frozen offsets are unchanged.
pub use crate::tasks::{CAPACITY, CHECK_CANCEL};

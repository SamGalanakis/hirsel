//! Generation-end wakeup and host-resource teardown for `tasks.rs` (EMB-3).
use super::{Body, CURRENT, Scheduler, SchedulerProvider, State, Task, UnitGroup, WAITERS};
use crate::{
    Context,
    run_stop::{EndReason, Stop},
};

/// Execute a root on the same guarded stack discipline as its children.
/// # Safety
/// Context/body/argument/destination remain pinned and task-owned until completion.
pub unsafe fn run_root(
    context: *mut Context,
    body: Body,
    argument: *mut u8,
    destination: *mut u8,
    input_bytes: usize,
    result_bytes: usize,
    provider: SchedulerProvider,
) -> u8 {
    // SAFETY: the caller pins all root operands for the run.
    match unsafe {
        run_root_contained(
            context,
            body,
            argument,
            destination,
            input_bytes,
            result_bytes,
            provider,
        )
    } {
        Ok(failed) => failed,
        Err(EndReason::RunStop(stop)) => crate::run_stop::raise(stop),
        Err(EndReason::HostStop) => 0,
    }
}

/// # Safety
/// As for [`run_root`].
pub(crate) unsafe fn run_root_contained(
    context: *mut Context,
    body: Body,
    argument: *mut u8,
    destination: *mut u8,
    input_bytes: usize,
    result_bytes: usize,
    provider: SchedulerProvider,
) -> Result<u8, EndReason> {
    // SAFETY: the caller pins all root operands for the run.
    unsafe {
        super::run_root_with_unit_contained(
            context,
            body,
            argument,
            destination,
            input_bytes,
            result_bytes,
            provider,
            None,
            Default::default(),
        )
    }
}

/// # Safety
/// As for [`run_root`].
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn run_root_with_unit(
    context: *mut Context,
    body: Body,
    argument: *mut u8,
    destination: *mut u8,
    input_bytes: usize,
    result_bytes: usize,
    provider: SchedulerProvider,
    unit: Option<&mut crate::units_lang::LangUnit>,
    group: UnitGroup,
) -> u8 {
    // SAFETY: forwarded caller contract.
    match unsafe {
        super::run_root_with_unit_contained(
            context,
            body,
            argument,
            destination,
            input_bytes,
            result_bytes,
            provider,
            unit,
            group,
        )
    } {
        Ok(failed) => failed,
        Err(EndReason::RunStop(stop)) => crate::run_stop::raise(stop),
        Err(EndReason::HostStop) => 0,
    }
}

/// Run non-suspending OS code off the language fiber.
/// # Safety
/// Operation does not suspend/re-enter Hirsel; state/request stay pinned.
pub(crate) unsafe fn host_operation(
    state: *mut u8,
    request: *mut crate::provider::Request,
    operation: unsafe extern "C" fn(
        *mut u8,
        *mut crate::provider::Request,
    ) -> crate::provider::Outcome,
) -> crate::provider::Outcome {
    // SAFETY: the provider's operands remain live and its callback cannot suspend.
    unsafe {
        on_host_stack(
            false,
            #[cfg(feature = "prof")]
            (*request).site,
            || operation(state, request),
        )
    }
}

/// # Safety
/// The callback cannot re-enter the scheduler, suspend or unwind across its
/// host-stack frame. All borrowed operands stay live until the call returns.
pub(crate) unsafe fn on_host_stack<T>(
    require_stack: bool,
    #[cfg(feature = "prof")] site: u64,
    call: impl FnOnce() -> T,
) -> T {
    let task = CURRENT.get();
    if task.is_null() {
        #[cfg(feature = "prof")]
        let _interval = crate::prof::HostInterval::begin(super::prof_current_task(), site);
        if !require_stack {
            return call();
        }
        crate::fiber::arm();
        // SAFETY: this fresh stack belongs to this call alone.
        return unsafe { crate::fiber::Stack::new().call(call) };
    }
    if let Err(stop) = crate::fiber::room(crate::fiber::RUNTIME_ROOM) {
        crate::run_stop::raise(stop);
    }
    // SAFETY: the current task pins the worker's stock and no other task can
    // take this stack until the synchronous call returns.
    unsafe {
        let stock = &raw mut (*(*task).run).stacks;
        let host = (*stock).take();
        #[cfg(feature = "prof")]
        let interval = crate::prof::HostInterval::begin(super::prof_current_task(), site);
        let result = host.call(call);
        #[cfg(feature = "prof")]
        drop(interval);
        (*stock).recycle(host);
        if crate::run_stop::is_ended() {
            crate::run_stop::leave_on_ended();
        }
        result
    }
}

/// Wake every registered scheduler/alone waiter so peer workers observe
/// generation end immediately without waiting for timers or network I/O.
pub(crate) fn wake_all_waiters() {
    let waiters: Vec<_> = WAITERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for waiter in waiters {
        waiter.notify();
    }
}

/// Drop every stack held by tasks and stock in an ended scheduler without
/// trimming or recycling any stack (every mapping is unmapped whole via
/// `munmap`, including retired stacks).
///
/// # Safety
/// The dispatcher has exited and no task of `run` is running.
pub(super) unsafe fn teardown_ended(run: &mut Scheduler, root: *mut Task) -> EndReason {
    wake_all_waiters();
    crate::units::wake_all_for_stop();
    // SAFETY: exclusive ownership after dispatch_serial exited.
    unsafe {
        let mut task = run.all;
        while !task.is_null() {
            let next = (*task).all;
            // Drop each task's stack whole (munmap) without trimming/recycling.
            drop((*task).stack.take());
            if task != root {
                if (*task).state != State::Ended {
                    (*task).state = State::Ended;
                }
                // Release child task heap context/records without running Hirsel cleanup.
                let storage_layout = (*task).storage_layout;
                std::ptr::drop_in_place(task);
                if storage_layout.size() != 0 {
                    crate::backing_observation::observe!(free, task.cast(), storage_layout.size());
                    std::alloc::dealloc(task.cast(), storage_layout);
                }
            }
            task = next;
        }
        run.all = std::ptr::null_mut();
    }
    run.stacks.clear_all();
    crate::run_stop::current_generation()
        .and_then(|g| g.end_reason())
        .unwrap_or(EndReason::HostStop)
}

#[inline(always)]
pub(super) fn stop_static(name: &'static [u8]) -> ! {
    crate::run_stop::raise(Stop::from_static(name))
}

#[inline(always)]
pub(super) fn record_static(name: &'static [u8]) {
    crate::run_stop::record(Stop::from_static(name))
}

//! C executable fixtures inspect the real runtime, with no shadow implementation.
/// # Safety
/// Context is live and borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_available(
    context: *const hirsel_core_rt::Context,
) -> u64 {
    // SAFETY: native fixture passes its live task context.
    unsafe { (&*context).available() }
}
// Keep the actual native runtime entry points in this staticlib.
#[used]
static CONTEXT: extern "C" fn(u64) -> *mut hirsel_core_rt::Context =
    hirsel_core_rt::abi::hirsel_core_context_new;

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_test_context_new(bytes: u64) -> *mut hirsel_core_rt::Context {
    let mut context = hirsel_core_rt::Context::new(288);
    hirsel_core_rt::causes::task_start(&mut context).expect("fixture startup allowance");
    context.reset_test_limit(bytes);
    Box::into_raw(Box::new(context))
}
/// # Safety
/// Context is live and exclusively borrowed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_inject(
    context: *mut hirsel_core_rt::Context,
    n: usize,
    later: bool,
) {
    // SAFETY: fixture exclusively owns its live context.
    unsafe { (&mut *context).inject_admission(n, later) }
}
/// # Safety
/// Context is live and borrowed for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_attempts(
    context: *const hirsel_core_rt::Context,
) -> usize {
    // SAFETY: fixture exclusively owns its live context.
    unsafe { (&*context).admission_attempts() }
}

/// Advance the real test injector once, restoring the empty reservation before
/// emitted admission mutates the balances itself.
/// # Safety
/// The fixture exclusively borrows its live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_probe_admission(
    context: *mut hirsel_core_rt::Context,
    charge: u64,
) -> bool {
    // SAFETY: fixture owns this live context for the duration of the call.
    let context = unsafe { &mut *context };
    if context.limit_enter(charge).is_err() {
        return false;
    }
    context.limit_exit();
    true
}

/// Execute native fixtures on the real cooperative root and child stacks.
/// # Safety
/// The context, body, argument and result storage stay pinned through completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_run(
    context: *mut hirsel_core_rt::Context,
    body: hirsel_core_rt::tasks::Body,
    input: *mut u8,
    output: *mut u8,
    input_bytes: usize,
    output_bytes: usize,
) -> u8 {
    // SAFETY: the fixture supplies the exact initialized input/result carriers.
    unsafe {
        hirsel_core_rt::tasks::run_root(
            context,
            body,
            input,
            output,
            input_bytes,
            output_bytes,
            Default::default(),
        )
    }
}

/// Execute a fixture with an explicit event recorder on the cooperative root.
/// # Safety
/// All operands and provider callback state stay pinned through completion;
/// callbacks neither suspend nor re-enter the runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_run_events(
    context: *mut hirsel_core_rt::Context,
    body: hirsel_core_rt::tasks::Body,
    input: *mut u8,
    output: *mut u8,
    input_bytes: usize,
    output_bytes: usize,
    provider: hirsel_core_rt::tasks::SchedulerProvider,
) -> u8 {
    // SAFETY: the caller supplies initialized carriers and a bounded recorder.
    unsafe {
        hirsel_core_rt::tasks::run_root(
            context,
            body,
            input,
            output,
            input_bytes,
            output_bytes,
            provider,
        )
    }
}

/// Reset a process fixture's allowance after real task startup.
/// # Safety
/// Context is live and exclusively owned by the fixture entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_reset_limit(
    context: *mut hirsel_core_rt::Context,
    bytes: u64,
) {
    // SAFETY: the process entry exclusively borrows this live context.
    unsafe {
        (&mut *context).reset_test_limit(bytes);
    }
}

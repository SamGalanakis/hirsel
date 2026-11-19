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
    // Keep the payer domain registered by task_start while exposing the
    // requested fixture allowance, including u64::MAX rows.
    context.set_test_available(bytes);
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

/// Set a process fixture's allowance after real task startup, preserving the
/// payer domain and its owner inbox.
/// # Safety
/// Context is live and exclusively owned by the fixture entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_reset_limit(
    context: *mut hirsel_core_rt::Context,
    bytes: u64,
) {
    // SAFETY: the process entry exclusively borrows this live context.
    unsafe {
        let context = &mut *context;
        context.set_test_available(bytes);
        // Admission indices in the sample exclude process startup. Index zero
        // never fires, and an injected case replaces it before its first admit.
        context.inject_admission(0, false);
    }
}

/// # Safety
/// Context is this fixture's empty root, exclusively owned by the worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_observe_begin(
    context: *const hirsel_core_rt::Context,
) -> *mut hirsel_core_rt::peak::Run {
    // SAFETY: fixture pins its root for the whole observation.
    Box::into_raw(Box::new(hirsel_core_rt::peak::Run::start_test_interval(
        unsafe { &*context },
    )))
}
/// # Safety
/// Observation is the live unique result of test_observe_begin on this worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_observe_end(
    observation: *mut hirsel_core_rt::peak::Run,
) -> u64 {
    // SAFETY: fixture ends and consumes exactly its own guard.
    drop(unsafe { Box::from_raw(observation) });
    hirsel_core_rt::abi::hirsel_core_peak_charge()
}

/// # Safety
/// A fixture transfers a fresh owned fd while executing inside the run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_socket_adopt(
    context: *mut hirsel_core_rt::Context,
    fd: i32,
) -> u64 {
    use std::os::fd::FromRawFd;
    unsafe { (*context).adopt_fixture_socket(std::os::fd::OwnedFd::from_raw_fd(fd)) }
}

/// Starts one native source law with the same socket and clock model as the evaluator.
/// The linker substitutes this entry only for model-network law executables.
///
/// # Safety
/// The generated executable supplies the OS entry's configuration, complete
/// source table, and matching entry thunk. All are valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_hirsel_core_os_run_with_sources(
    input: *const hirsel_core_rt::entry::OsConfig,
    _names: *const hirsel_core_rt::process::View,
    sources: *const hirsel_core_rt::abi_sites::SourceTable,
    entry: hirsel_core_rt::process::Entry,
) -> u8 {
    use hirsel_core_rt::{abi_sites, entry::*, model_net::ModelProvider, process};

    // SAFETY: the generated configuration and its borrowed tables live through startup.
    let opened = unsafe { hirsel_core_os_process_new(input) };
    if opened.process.is_null() {
        return process::NOT_STARTED;
    }
    // SAFETY: we own the process; the generated source table outlives it.
    unsafe { abi_sites::hirsel_core_os_process_set_sources(opened.process, sources) };
    // SAFETY: the process pins its configuration, grant records and fallback provider.
    let mut cfg = unsafe { *hirsel_core_os_process_config(opened.process) };
    let mut model = ModelProvider::new(Default::default(), unsafe { *cfg.provider });
    let provider = model.provider();
    // SAFETY: the model laws grant at least Net, so the pointer names a live grant table.
    let mut grants =
        unsafe { std::slice::from_raw_parts(cfg.capabilities, cfg.capabilities_len) }.to_vec();
    for grant in &mut grants {
        if matches!(grant.kind, NET | CLOCK) {
            grant.provider = &provider;
        }
    }
    cfg.capabilities = grants.as_ptr();
    cfg.provider = &provider;
    // SAFETY: the model, grant table and provider pointers remain pinned through
    // the generated entry, structured task cleanup, and grant closure.
    let status = unsafe { process::hirsel_core_run(&cfg, entry) };
    // SAFETY: the process is still owned here and the run has ended.
    unsafe { hirsel_core_os_process_free(opened.process) };
    status
}

/// Run a linked native fixture with T1's last-runnable schedule.
/// # Safety
/// The runner and entry obey the process ABI. The configuration stays live
/// through their return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_run_last_with_runner(
    run: unsafe extern "C" fn(
        *const hirsel_core_rt::process::Config,
        hirsel_core_rt::process::Entry,
    ) -> u8,
    config: *const hirsel_core_rt::process::Config,
    entry: hirsel_core_rt::process::Entry,
) -> u8 {
    let script = std::sync::Arc::new(hirsel_core_rt::schedule_script::ScheduleScript::last());
    // SAFETY: the link wrapper supplies the real process runner and its live operands.
    script.install(|| unsafe { run(config, entry) })
}

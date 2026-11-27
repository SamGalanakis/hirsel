//! Process entry. The generated entry thunk runs main and all its cleanup before
//! returning a MainResult. Only this boundary turns an outcome into an exit code.
use crate::{
    Context,
    causes::{self, Cause},
    provider::Provider,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct View {
    pub data: *const u8,
    pub len: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MainResult {
    /// 0 Ok, 1 Error(E) for E != Exit, 2 Error(Exit).
    pub kind: u8,
    pub exit: u8,
    /// PROG-3: the text of `e` for kind 1, a host block from
    /// [`hirsel_core_main_error`] that the run prints and frees; null when absent.
    pub text: *mut u8,
    pub text_len: usize,
}
impl Default for MainResult {
    fn default() -> Self {
        Self {
            kind: 0,
            exit: 0,
            text: std::ptr::null_mut(),
            text_len: 0,
        }
    }
}
impl MainResult {
    /// Take the error text, if any, leaving none.
    fn take_text(&mut self) -> Option<Box<[u8]>> {
        if self.text.is_null() {
            return None;
        }
        let text = std::ptr::slice_from_raw_parts_mut(self.text, self.text_len);
        self.text = std::ptr::null_mut();
        // SAFETY: `text` is the block hirsel_core_main_error leaked, taken once.
        Some(unsafe { Box::from_raw(text) })
    }
}

/// PROG-3: called by the generated entry when `main` returned `Error(e)` with
/// `E` not `Exit`, before the result is cleaned. Renders `e` from the entry's
/// text program (`hirsel_core_layout::show`) into `out`.
///
/// # Safety
/// `value` is the entry's live `Result`, `program` the `len` bytes the build
/// encoded for its `E`, and `out` the run's MainResult.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_main_error(
    _context: *mut Context,
    value: *const u8,
    program: *const u8,
    len: usize,
    out: *mut MainResult,
) {
    struct Host;
    impl hirsel_core_layout::show::Memory for Host {
        fn read(&self, at: u64, out: &mut [u8]) {
            // SAFETY: the program reads only the live value's own bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(at as *const u8, out.as_mut_ptr(), out.len());
            }
        }
    }
    // SAFETY: the build's program bytes are static and live for the run.
    let program = unsafe { std::slice::from_raw_parts(program, len) };
    let show = hirsel_core_layout::show::Show::decode(program)
        .expect("the build encodes the entry's text program");
    let text = show
        .render(value as u64, &Host)
        .into_bytes()
        .into_boxed_slice();
    // SAFETY: the run's MainResult outlives the entry; an earlier text is freed.
    unsafe {
        drop((*out).take_text());
        (*out).text_len = text.len();
        (*out).text = Box::into_raw(text).cast();
    }
}
pub type Entry = unsafe extern "C" fn(*mut Context, *const Config, *mut MainResult) -> u8;
/// Host configuration and grant operands, all pinned for the run. Missing
/// required grants are refused by the named OS entry before calling run (CAP-4).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Config {
    pub root_limit: u64,
    pub args: *const crate::provider::Buffer,
    pub args_len: usize,
    pub grants: *const u64,
    pub grants_len: usize,
    pub provider: *const Provider,
    pub state: *mut u8,
    pub diagnostic: unsafe extern "C" fn(*mut u8, *const u8, usize),
    /// Source binder formats the cause and rebound site as one line, without a
    /// newline. Its immutable view lives through diagnostic. Null uses tag/site.
    pub describe: Option<unsafe extern "C" fn(*mut u8, *const Cause) -> View>,
    pub scheduler: crate::tasks::SchedulerProvider,
    pub stop_fd: i32,
    pub capabilities: *const crate::entry::Grant,
    pub capabilities_len: usize,
    pub close_grants: Option<unsafe extern "C" fn(*mut u8)>,
    /// Immutable metadata pinned by the launch owner, never a per-session lookup.
    pub launch: *const hirsel_core_ids::launch::Metadata,
    pub describe_output: Option<unsafe extern "C" fn(*mut u8, *const u8, usize) -> bool>,
}

pub const NOT_STARTED: u8 = 103;

/// Take the place control socket once (field 0) or read the scratch grant (1).
/// # Safety
/// The context and its configuration are pinned for the current run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_launch_handle(context: *mut Context, field: u64) -> u64 {
    #[cfg(feature = "record")]
    {
        // SAFETY: the run pins Files, and its one worker serializes handle transfers.
        let managed = unsafe { &mut (*(*context).host_files).managed_control };
        if managed.1 != 0 {
            return match field {
                0 => std::mem::take(&mut managed.0),
                1 => managed.1,
                _ => 0,
            };
        }
    }
    let Some(host) = (unsafe { crate::entry::launch_host((*context).run) }) else {
        return 0;
    };
    match field {
        0 => {
            let control = host.control.lock().unwrap().take();
            control
                .and_then(|fd| unsafe {
                    crate::os_net::adopt_socket(&raw mut (*(*context).host_files).sockets, fd).ok()
                })
                .unwrap_or(0)
        }
        1 => host.scratch,
        _ => 0,
    }
}

/// Private runner metadata transport. Missing metadata is source-only identity.
/// # Safety
/// The context and its configuration are pinned for the current run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_launch_word(
    context: *const Context,
    field: u64,
    index: u64,
) -> u64 {
    // SAFETY: the launch owner pins Config and its immutable metadata.
    unsafe {
        let run = (*context).run;
        if run.is_null() || (*run).launch.is_null() {
            return if field == 0 {
                hirsel_core_ids::launch::DEFAULT_N_PLACE
            } else {
                0
            };
        }
        (*(*run).launch).word(field, index)
    }
}

/// # Safety
/// The context pins launch metadata; output is a fresh three-word String view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_launch_target(
    context: *const Context,
    output: *mut crate::provider::Buffer,
) {
    // SAFETY: the caller owns output; the run pins the target's immutable bytes.
    unsafe {
        let run = (*context).run;
        let target = if run.is_null() || (*run).launch.is_null() {
            ""
        } else {
            (*(*run).launch).target()
        };
        *output = crate::provider::Buffer {
            data: target.as_ptr().cast_mut(),
            len: target.len(),
            capacity: 0,
        };
    }
}

/// # Safety
/// Context/config remain pinned for the run.
#[inline(always)]
pub(crate) unsafe fn deny_describe_capability(context: *const Context) {
    // SAFETY: the launch owner pins Config and its immutable metadata.
    unsafe {
        let run = (*context).run;
        if !run.is_null() && !(*run).launch.is_null() && (*(*run).launch).describe() {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"DescribeCapability"));
        }
    }
}

/// # Safety
/// Buffer is a borrowed Bytes carrier; the callback consumes neither it nor data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_describe_emit(
    context: *const Context,
    buffer: *const crate::provider::Buffer,
) -> bool {
    // SAFETY: the launcher pins Config and callback, and the caller pins buffer.
    unsafe {
        if hirsel_core_launch_word(context, 5, 0) == 0
            && hirsel_core_launch_word(context, 12, 0) == 0
            && hirsel_core_launch_word(context, 15, 0) == 0
            && hirsel_core_launch_word(context, 20, 0) == 0
            && hirsel_core_launch_word(context, 21, 0) == 0
            && hirsel_core_launch_word(context, 28, 0) == 0
        {
            return false;
        }
        let config = &*(*context).run;
        let Some(write) = config.describe_output else {
            return false;
        };
        write(config.state, (*buffer).data, (*buffer).len)
    }
}

/// Run a native fixture under a seeded ScheduleScript.
///
/// # Safety
/// The configuration, entry, and callback state remain live through the run.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_run_scripted(
    config: *const Config,
    entry: Entry,
    seed: u64,
) -> u8 {
    use crate::schedule_script::ScheduleScript;
    let script = std::sync::Arc::new(ScheduleScript::new(seed));
    // SAFETY: the native fixture pins the configuration and entry for this call.
    script.install(|| unsafe { hirsel_core_run(config, entry) })
}

/// Run a native fixture that always prefers the last runnable task.
///
/// # Safety
/// The configuration, entry, and callback state remain live through the run.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_run_last(config: *const Config, entry: Entry) -> u8 {
    let script = std::sync::Arc::new(crate::schedule_script::ScheduleScript::last());
    // SAFETY: the native fixture pins the configuration and entry for this call.
    script.install(|| unsafe { hirsel_core_run(config, entry) })
}

/// A test process's replacement for an OS launch's `hirsel_core_run`.
#[cfg(feature = "testing")]
type Launch = unsafe extern "C" fn(*const Config, Entry) -> u8;
#[cfg(feature = "testing")]
static LAUNCH: std::sync::OnceLock<Launch> = std::sync::OnceLock::new();

/// Route a test process's OS launches (an ordinary run without a checked
/// foreign interface, and a recording) through `launch`, which receives the
/// launch's Config and entry and runs them itself, with its own providers,
/// observers or chooser, through `hirsel_core_run`. A C test object registers
/// it from a constructor: `--wrap=hirsel_core_run` cannot reach these calls,
/// which the runtime archive resolves internally. Set once; a second call
/// returns false.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_test_set_launch(launch: Launch) -> bool {
    LAUNCH.set(launch).is_ok()
}

/// An OS launch's run: the registered test launch, else `hirsel_core_run`.
/// # Safety
/// As hirsel_core_run.
pub(crate) unsafe fn launch(config: &Config, entry: Entry) -> u8 {
    #[cfg(feature = "testing")]
    if let Some(launch) = LAUNCH.get() {
        // SAFETY: the launch takes this call's pinned Config and entry.
        return unsafe { launch(config, entry) };
    }
    // SAFETY: forwarded caller contract.
    unsafe { hirsel_core_run(config, entry) }
}

/// [`launch`] for a recording run. A recording build reaches no native code
/// (FFI-8) and contained code never waits (FFI-12), so the run carries the
/// foreign state its contained calls use, with no helper pool.
///
/// # Safety
/// As [`launch`].
pub(crate) unsafe fn launch_taped(config: &Config, entry: Entry) -> u8 {
    #[cfg(feature = "testing")]
    if let Some(launch) = LAUNCH.get() {
        // SAFETY: the launch takes this call's pinned Config and entry.
        return unsafe { launch(config, entry) };
    }
    // SAFETY: forwarded caller contract.
    unsafe { run_taped(config, entry) }
}

/// [`hirsel_core_run`] for a replayed run, as [`launch_taped`].
///
/// # Safety
/// As [`hirsel_core_run`].
pub(crate) unsafe fn run_taped(config: &Config, entry: Entry) -> u8 {
    // SAFETY: forwarded caller contract.
    unsafe {
        run_config(config, entry, |context, main| {
            with_foreign(context, main, false)
        })
    }
}

/// # Safety
/// Config, entry and all borrowed grants/arguments remain live through the run.
/// The generated entry thunk returns only after cleanup on every exit edge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_run(config: *const Config, entry: Entry) -> u8 {
    // SAFETY: caller pins the configuration and entry through the run.
    unsafe { run_config(&*config, entry, run_plain) }
}

type RunMain<'a> = dyn FnMut(&mut Context) -> Result<(u8, i32), crate::run_stop::EndReason> + 'a;

fn run_plain(
    context: &mut Context,
    main: &mut RunMain<'_>,
) -> std::io::Result<Result<(u8, i32), crate::run_stop::EndReason>> {
    Ok(main(context))
}

/// Run a program with a checked foreign interface. A reachable waits entry
/// requires the helper pool.
///
/// # Safety
/// As hirsel_core_run. The entry may use the pool until its last task ends.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_run_with_foreign(
    config: *const Config,
    entry: Entry,
    waits: bool,
) -> u8 {
    // SAFETY: caller pins the configuration and entry through the run.
    unsafe {
        run_config(&*config, entry, |context, main| {
            with_foreign(context, main, waits)
        })
    }
}

unsafe fn with_foreign(
    context: &mut Context,
    main: &mut RunMain<'_>,
    waits: bool,
) -> std::io::Result<Result<(u8, i32), crate::run_stop::EndReason>> {
    let state = crate::foreign::Run::new(waits)?;
    context.foreign = &raw const state;
    let result = main(context);
    if result.is_ok() {
        // SAFETY: the main runner has joined the tasks and delivered their calls.
        unsafe {
            crate::foreign::contained::end_task(context);
            state.finish(context);
        }
    }
    context.foreign = std::ptr::null();
    Ok(result)
}

fn map_hash_key() -> std::io::Result<[u8; 16]> {
    let mut key = [0; 16];
    let mut remaining = &mut key[..];
    while !remaining.is_empty() {
        // SAFETY: remaining is a writable slice passed for exactly its length.
        let n = unsafe { crate::host::sys::random(remaining.as_mut_ptr(), remaining.len(), 0) };
        if n < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if n == 0 {
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        remaining = &mut remaining[n as usize..];
    }
    Ok(key)
}

// Instantiate startup for each runner so the linker can discard unused
// foreign setup from executables that use the ordinary entry.
unsafe fn run_config(
    config: &Config,
    entry: Entry,
    run: impl FnOnce(
        &mut Context,
        &mut RunMain<'_>,
    ) -> std::io::Result<Result<(u8, i32), crate::run_stop::EndReason>>,
) -> u8 {
    let standalone_gen = crate::run_stop::GenerationEnd::standalone();
    // An untied pool attaches its run's generation to every worker thread
    // before any unit runs; a scope entered here would be bound to whichever
    // worker the root unit started on, not to the unit.
    let _scope = if crate::run_stop::current_generation().is_some() {
        None
    } else {
        // SAFETY: `standalone_gen` lives for the entire duration of `run_config`.
        Some(unsafe { crate::run_stop::GenerationScope::enter(&standalone_gen) })
    };
    // SAFETY: forwarded caller contract.
    match unsafe { run_config_inner(config, entry, run) } {
        Ok(status) => status,
        Err(crate::run_stop::EndReason::RunStop(reason)) => {
            if crate::run_stop::is_contained() {
                NOT_STARTED
            } else {
                stop(reason.as_bytes())
            }
        }
        Err(crate::run_stop::EndReason::HostStop) => 0,
    }
}

/// Contained (embedded) entry that never alters process-wide `SIGPIPE` and
/// returns any run stop or host stop instead of calling `process::stop`.
pub(crate) unsafe fn run_config_contained(
    config: &Config,
    entry: Entry,
    _generation: &crate::run_stop::GenerationEnd,
) -> Result<u8, crate::run_stop::EndReason> {
    // SAFETY: caller pinned config, entry, and entered GenerationScope.
    unsafe { run_config_inner(config, entry, run_plain) }
}

/// Contained (embedded) entry with checked foreign support (`waits` helper pool).
///
/// # Safety
/// Caller pinned `config`, `entry`, and entered `GenerationScope`.
#[cfg(not(feature = "standalone"))]
pub(crate) unsafe fn run_config_contained_with_foreign(
    config: &Config,
    entry: Entry,
    waits: bool,
    _generation: &crate::run_stop::GenerationEnd,
) -> Result<u8, crate::run_stop::EndReason> {
    // SAFETY: caller pinned config, entry, and entered GenerationScope.
    unsafe {
        run_config_inner(config, entry, |context, main| {
            with_foreign(context, main, waits)
        })
    }
}

unsafe fn run_config_inner(
    config: &Config,
    entry: Entry,
    run: impl FnOnce(
        &mut Context,
        &mut RunMain<'_>,
    ) -> std::io::Result<Result<(u8, i32), crate::run_stop::EndReason>>,
) -> Result<u8, crate::run_stop::EndReason> {
    #[cfg(not(feature = "standalone"))]
    let measured = if crate::run_stop::is_contained() && config.scheduler.event.is_none() {
        crate::embed::measure::provider().map(|scheduler| Config {
            scheduler,
            ..*config
        })
    } else {
        None
    };
    #[cfg(not(feature = "standalone"))]
    let config = measured.as_ref().unwrap_or(config);
    // SAFETY: the driver pins configuration and the generated entry thunk.
    unsafe {
        #[cfg(feature = "testing")]
        let meter_output = std::env::var_os("HIRSEL_TEST_METER_OUT");
        #[cfg(feature = "testing")]
        let key = if meter_output.is_some() {
            Ok([0; 16])
        } else {
            map_hash_key()
        };
        #[cfg(not(feature = "testing"))]
        let key = map_hash_key();
        let key = match key {
            Ok(key) => key,
            Err(error) => {
                diagnostic(
                    config,
                    format!("not started: map hash entropy: {error}\n").as_bytes(),
                );
                if let Some(close) = config.close_grants {
                    close(config.state);
                }
                return Ok(NOT_STARTED);
            }
        };
        let _pipe_policy = (!crate::run_stop::is_contained()).then(PipePolicy::enter);
        #[cfg(feature = "prof")]
        let _worker_profile = crate::prof::Worker::enter();
        let mut context = Context::with_unit_heap(config.root_limit);
        #[cfg(feature = "testing")]
        if meter_output.is_some() {
            let inject = std::env::var("HIRSEL_TEST_METER_LIMIT")
                .ok()
                .map(|limit| limit.parse().expect("a test step limit"));
            context.step_observation = Some(crate::steps::Observation::new(inject));
        }
        #[cfg(feature = "prof")]
        crate::mem_prof::root(context.limit_record().addr(), context.available());
        context.run = config;
        context.set_map_keys(crate::map_keys(&key));
        #[cfg(feature = "charge-observation")]
        let _observation = crate::peak::Run::start(&context);
        let mut files = crate::host_file::Files::default();
        context.host_files = &mut files;
        if let Err(capacity) = causes::task_start(&mut context) {
            diagnostic(
                config,
                format!(
                    "not started: the root limit is below main's startup charge of {} bytes\n",
                    capacity.requested
                )
                .as_bytes(),
            );
            if let Some(close) = config.close_grants {
                close(config.state);
            }
            return Ok(NOT_STARTED);
        }
        let mut result = MainResult::default();
        struct Call {
            entry: Entry,
            config: *const Config,
        }
        unsafe extern "C" fn invoke(
            context: *mut Context,
            result: *mut u8,
            argument: *mut u8,
        ) -> u8 {
            // SAFETY: root wrapper pins Call and the MainResult destination.
            unsafe {
                *result.cast::<MainResult>() = MainResult::default();
                let call = &*argument.cast::<Call>();
                (call.entry)(context, call.config, result.cast())
            }
        }
        let mut main = |context: &mut Context| {
            #[cfg(feature = "charge-observation")]
            crate::peak::checkpoint("startup");
            let mut call = Call { entry, config };
            let previous_fd = crate::fiber::set_stop_fd(if config.stop_fd == 0 {
                2
            } else {
                config.stop_fd
            });
            // Installation: a provider missing a callback is refused here, before
            // any operation could use it (REPLAY-SPEC section 3).
            if let Err(stop) = crate::provider::try_install(config.provider) {
                crate::fiber::set_stop_fd(previous_fd);
                return Err(crate::run_stop::EndReason::RunStop(stop));
            }
            for n in 0..config.capabilities_len {
                if let Err(stop) =
                    crate::provider::try_install((*config.capabilities.add(n)).provider)
                {
                    crate::fiber::set_stop_fd(previous_fd);
                    return Err(crate::run_stop::EndReason::RunStop(stop));
                }
            }
            let failed = crate::tasks::run_root_contained(
                context,
                invoke,
                (&mut call as *mut Call).cast(),
                (&raw mut result).cast(),
                size_of::<Call>(),
                size_of::<MainResult>(),
                config.scheduler,
            );
            match failed {
                Ok(failed) => Ok((failed, previous_fd)),
                Err(end) => {
                    crate::fiber::set_stop_fd(previous_fd);
                    Err(end)
                }
            }
        };
        let (failed, previous_fd) = match run(&mut context, &mut main) {
            Ok(Ok(result)) => result,
            Ok(Err(end)) => {
                // On a contained end, release host resources (files/context)
                // without running Hirsel cleanup or diagnostics.
                // On standalone RunStop, caller invokes process::stop before
                // close_grants, matching FAIL-15.
                if crate::run_stop::is_contained() {
                    drop(result.take_text());
                    files.close_all(&mut context);
                    drop(context);
                    if let Some(close) = config.close_grants {
                        close(config.state);
                    }
                }
                return Err(end);
            }
            Err(error) => {
                diagnostic(
                    config,
                    format!("not started: foreign helpers: {error}\n").as_bytes(),
                );
                if let Some(close) = config.close_grants {
                    close(config.state);
                }
                return Ok(NOT_STARTED);
            }
        };
        #[cfg(feature = "charge-observation")]
        crate::peak::checkpoint("main");
        crate::fiber::set_stop_fd(previous_fd);
        let text = result.take_text();
        let status = if failed != 0 {
            for cause in context.causes.iter() {
                if let Some(describe) = config.describe {
                    let line = describe(config.state, cause);
                    (config.diagnostic)(config.state, line.data, line.len);
                } else {
                    diagnostic(config, crate::abi_host::format_cause(cause).as_bytes());
                }
                if std::ptr::eq(cause, context.root_refusal) {
                    let limit = config.root_limit;
                    let hint = format!(
                        " (root limit {limit} bytes; raise it with --limit or the manifest)"
                    );
                    diagnostic(config, hint.as_bytes());
                }
                diagnostic(config, b"\n");
            }
            101
        } else {
            // PROG-3: `error: ` and the value, after its cleanup has ended.
            if result.kind == 1
                && let Some(text) = &text
            {
                diagnostic(config, b"error: ");
                diagnostic(config, text);
                diagnostic(config, b"\n");
            }
            match result.kind {
                0 => 0,
                2 if (1..=100).contains(&result.exit) => result.exit,
                _ => 1,
            }
        };
        drop(text);
        files.close_all(&mut context);
        #[cfg(feature = "testing")]
        let meter =
            meter_output
                .zip(context.step_observation.as_ref())
                .map(|(path, observation)| {
                    (path, observation.report(&context), context.current.clone())
                });
        drop(context);
        if let Some(close) = config.close_grants {
            close(config.state);
        }
        #[cfg(feature = "testing")]
        if let Some((path, (steps, refusal), funding)) = meter {
            let bytes = config.root_limit - funding.available();
            let site = refusal.map_or("null".to_string(), |site| site.to_string());
            std::fs::write(
                path,
                format!(r#"{{"steps":{steps},"used":{bytes},"refusal_site":{site}}}"#),
            )
            .expect("write test metering report");
        }
        Ok(status)
    }
}
unsafe fn diagnostic(config: &Config, bytes: &[u8]) {
    // SAFETY: borrowed bytes stay live through the host diagnostic callback.
    unsafe {
        (config.diagnostic)(config.state, bytes.as_ptr(), bytes.len());
    }
}
/// Return the original byte limit of the current task.
///
/// # Safety
/// The generated call supplies a live context for the running task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_root_limit(context: *const Context) -> u64 {
    // SAFETY: the generated call runs with its own live context.
    unsafe { (*context).root_limit }
}

/// # Safety
/// Args is the run's live configuration; the returned view stays pinned by it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_args_len(context: *const Context, args: u64) -> u64 {
    // SAFETY: the run owns immutable argument descriptors.
    unsafe { crate::host_file::args(context, args).len() as u64 }
}
/// # Safety
/// Same as args_len. Index is user input and is checked before address arithmetic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_args_get(
    context: *const Context,
    args: u64,
    index: u64,
) -> *const crate::provider::Buffer {
    // SAFETY: the run pins the vector; only a checked index reads it.
    unsafe {
        crate::host_file::args(context, args)
            .get(index as usize)
            .map_or(std::ptr::null(), |b| b as *const _)
    }
}

/// FAIL-15/PROG-3: `run stop: <Name>\n`, status 102, skipping cleanup.
/// Only async-signal-safe writes and _exit are used so
/// the stack overflow handler can take the same path from its alternate stack.
pub fn stop(name: &[u8]) -> ! {
    // A secret read's terminal modes go back before the process ends.
    crate::term_restore::restore_now();
    // SAFETY: static/borrowed bytes remain live until _exit, no Rust unwinding.
    unsafe {
        let fd = crate::fiber::stop_fd();
        libc::write(fd, b"run stop: ".as_ptr().cast(), 10);
        libc::write(fd, name.as_ptr().cast(), name.len());
        libc::write(fd, b"\n".as_ptr().cast(), 1);
        libc::_exit(102)
    }
}

/// Standalone fallback when a stop occurs outside any contained generation
/// (e.g. a standalone direct unit test calling an internal helper directly).
pub(crate) fn uncontained_stop(reason: crate::run_stop::Stop) -> ! {
    stop(reason.as_bytes())
}

// Rust std's Unix startup ignores SIGPIPE so write returns EPIPE. Generated C
// entries do not run Rust startup. Hold that same policy while runs are active,
// then restore the embedding host's action after the last run. The counter
// permits concurrent independent runs.
struct PipeState {
    users: u64,
    previous: Option<libc::sigaction>,
}
static PIPE_STATE: std::sync::Mutex<PipeState> = std::sync::Mutex::new(PipeState {
    users: 0,
    previous: None,
});
struct PipePolicy;
impl PipePolicy {
    fn enter() -> Self {
        let mut state = PIPE_STATE.lock().unwrap();
        if state.users == 0 {
            // SAFETY: initialized actions, valid signal; kernel copies both structures.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = libc::SIG_IGN;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(libc::SIGPIPE, &action, &mut previous) != 0 {
                    stop(b"Signal");
                }
                state.previous = Some(previous);
            }
        }
        state.users = state
            .users
            .checked_add(1)
            .unwrap_or_else(|| stop(b"ActiveRuns"));
        Self
    }
}
impl Drop for PipePolicy {
    fn drop(&mut self) {
        let mut state = PIPE_STATE.lock().unwrap();
        state.users -= 1;
        if state.users == 0 {
            // SAFETY: the final run restores exactly the host action saved at entry.
            unsafe {
                libc::sigaction(
                    libc::SIGPIPE,
                    &state.previous.take().unwrap_or_else(|| stop(b"Signal")),
                    std::ptr::null_mut(),
                );
            }
        }
    }
}

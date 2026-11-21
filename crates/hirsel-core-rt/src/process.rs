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
#[derive(Clone, Copy, Default)]
pub struct MainResult {
    /// 0 Ok, 1 Error(E) for E != Exit, 2 Error(Exit).
    pub kind: u8,
    pub exit: u8,
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

/// Arm the serving or preparing child's exit before it can acquire store
/// authority. Check the parent after arming to close the fork-to-arm race.
pub(crate) fn arm_host_parent() -> std::io::Result<()> {
    let Some(parent) = std::env::var_os("HIRSEL_HOST_PID") else {
        return Ok(());
    };
    let expected = parent
        .to_str()
        .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        .filter(|pid| *pid > 1)
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: these calls take no memory arguments and affect only this child.
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::getppid() != expected {
            return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    Ok(())
}

/// Take the place control socket once (field 0) or read the scratch grant (1).
/// # Safety
/// The context and its configuration are pinned for the current run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_launch_handle(context: *mut Context, field: u64) -> u64 {
    #[cfg(feature = "record")]
    {
        let _commit = crate::sequencer::enter();
        // SAFETY: the run pins Files and the sequencer serializes handle transfers.
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
            let _commit = crate::sequencer::enter();
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
            stop(b"DescribeCapability");
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
        if hirsel_core_launch_word(context, 5, 0) == 0 {
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

/// # Safety
/// Config, entry and all borrowed grants/arguments remain live through the run.
/// The generated entry thunk returns only after cleanup on every exit edge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_run(config: *const Config, entry: Entry) -> u8 {
    // SAFETY: the driver pins configuration and the generated entry thunk.
    unsafe {
        let config = &*config;
        let key = match map_hash_key() {
            Ok(key) => key,
            Err(error) => {
                diagnostic(
                    config,
                    format!("not started: map hash entropy: {error}\n").as_bytes(),
                );
                if let Some(close) = config.close_grants {
                    close(config.state);
                }
                return NOT_STARTED;
            }
        };
        run_with_map_hash_key(config, entry, key)
    }
}

fn map_hash_key() -> std::io::Result<[u8; 16]> {
    let mut key = [0; 16];
    let mut remaining = &mut key[..];
    while !remaining.is_empty() {
        // SAFETY: remaining is a writable slice passed for exactly its length.
        let n = unsafe { libc::getrandom(remaining.as_mut_ptr().cast(), remaining.len(), 0) };
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

/// Enter with the key restored from a checked Run header.
pub(crate) unsafe fn run_with_map_hash_key(config: &Config, entry: Entry, key: [u8; 16]) -> u8 {
    // SAFETY: the caller pins the same operands as hirsel_core_run.
    unsafe {
        let _pipe_policy = PipePolicy::enter();
        let mut context = Context::new(config.root_limit);
        context.run = config;
        context.map_hash_key = hirsel_core_layout::map_hash::PreparedKey::new(&key);
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
            return NOT_STARTED;
        }
        #[cfg(feature = "charge-observation")]
        crate::peak::checkpoint("startup");
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
        let mut call = Call { entry, config };
        let previous_fd = crate::fiber::set_stop_fd(if config.stop_fd == 0 {
            2
        } else {
            config.stop_fd
        });
        // Installation: a provider missing a callback is refused here, before
        // any operation could use it (REPLAY-SPEC section 3).
        crate::provider::install(config.provider);
        for n in 0..config.capabilities_len {
            crate::provider::install((*config.capabilities.add(n)).provider);
        }
        let failed = crate::tasks::run_root(
            &mut context,
            invoke,
            (&mut call as *mut Call).cast(),
            (&mut result as *mut MainResult).cast(),
            size_of::<Call>(),
            size_of::<MainResult>(),
            config.scheduler,
        );
        #[cfg(feature = "charge-observation")]
        crate::peak::checkpoint("main");
        crate::fiber::set_stop_fd(previous_fd);
        let status = if failed != 0 {
            for cause in context.causes.iter() {
                if let Some(describe) = config.describe {
                    let line = describe(config.state, cause);
                    (config.diagnostic)(config.state, line.data, line.len);
                    diagnostic(config, b"\n");
                } else {
                    diagnostic(config, crate::abi_host::format_cause(cause).as_bytes());
                    diagnostic(config, b"\n");
                }
            }
            101
        } else {
            match result.kind {
                0 => 0,
                2 if (1..=100).contains(&result.exit) => result.exit,
                _ => 1,
            }
        };
        files.close_all(&mut context);
        drop(context);
        if let Some(close) = config.close_grants {
            close(config.state);
        }
        status
    }
}
unsafe fn diagnostic(config: &Config, bytes: &[u8]) {
    // SAFETY: borrowed bytes stay live through the host diagnostic callback.
    unsafe {
        (config.diagnostic)(config.state, bytes.as_ptr(), bytes.len());
    }
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
    // SAFETY: static/borrowed bytes remain live until _exit, no Rust unwinding.
    unsafe {
        let fd = crate::fiber::stop_fd();
        libc::write(fd, b"run stop: ".as_ptr().cast(), 10);
        libc::write(fd, name.as_ptr().cast(), name.len());
        libc::write(fd, b"\n".as_ptr().cast(), 1);
        libc::_exit(102)
    }
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

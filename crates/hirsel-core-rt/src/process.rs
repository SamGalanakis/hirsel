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
}

pub const NOT_STARTED: u8 = 103;

/// # Safety
/// Config, entry and all borrowed grants/arguments remain live through the run.
/// The generated entry thunk returns only after cleanup on every exit edge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_run(config: *const Config, entry: Entry) -> u8 {
    // SAFETY: the driver pins configuration and the generated entry thunk.
    unsafe {
        let config = &*config;
        let _pipe_policy = PipePolicy::enter();
        let mut context = Context::new(config.root_limit);
        context.run = config;
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
        let failed = crate::tasks::run_root(
            &mut context,
            invoke,
            (&mut call as *mut Call).cast(),
            (&mut result as *mut MainResult).cast(),
            size_of::<Call>(),
            size_of::<MainResult>(),
            config.scheduler,
        );
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
pub unsafe extern "C" fn hirsel_core_args_len(context: *const Context, _args: u64) -> u64 {
    // SAFETY: the run owns immutable argument descriptors.
    unsafe { (*(*context).run).args_len as u64 }
}
/// # Safety
/// Same as args_len. Index is user input and is checked before address arithmetic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_args_get(
    context: *const Context,
    _args: u64,
    index: u64,
) -> *const crate::provider::Buffer {
    // SAFETY: configuration is live; only a checked index reads its argument table.
    unsafe {
        let config = &*(*context).run;
        if index >= config.args_len as u64 {
            return std::ptr::null();
        }
        config.args.add(index as usize)
    }
}

/// Run stops skip cleanup. Only async-signal-safe writes and _exit are used so
/// the stack overflow handler can take the same path from its alternate stack.
pub fn stop(name: &[u8]) -> ! {
    // SAFETY: static/borrowed bytes remain live until _exit, no Rust unwinding.
    unsafe {
        let fd = crate::fiber::stop_fd();
        libc::write(fd, name.as_ptr().cast(), name.len());
        libc::write(fd, b"\n".as_ptr().cast(), 1);
        libc::_exit(102)
    }
}

// Rust std's Unix startup ignores SIGPIPE so write returns EPIPE. Generated C
// entries do not run Rust startup. Hold that same policy while runs are active,
// then restore the embedding host's action after the last run. No source signal
// operations exist in v1. The counter permits concurrent independent runs.
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
                    &state.previous.take().unwrap(),
                    std::ptr::null_mut(),
                );
            }
        }
    }
}

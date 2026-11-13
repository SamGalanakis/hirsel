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
/// required grants are refused by the driver before calling run (CAP-4).
#[repr(C)]
pub struct Config {
    pub root_limit: u64,
    pub args: *const View,
    pub args_len: usize,
    pub grants: *const u64,
    pub grants_len: usize,
    pub provider: *const Provider,
    pub state: *mut u8,
    pub diagnostic: unsafe extern "C" fn(*mut u8, *const u8, usize),
    /// Source binder formats the cause and rebound site as one line, without a
    /// newline. Its immutable view lives through diagnostic. Null uses tag/site.
    pub describe: Option<unsafe extern "C" fn(*mut u8, *const Cause) -> View>,
}

/// # Safety
/// Config, entry and all borrowed grants/arguments remain live through the run.
/// The generated entry thunk returns only after cleanup on every exit edge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_run(config: *const Config, entry: Entry) -> u8 {
    // SAFETY: the driver pins configuration and the generated entry thunk.
    unsafe {
        let config = &*config;
        let mut context = Context::new(config.root_limit);
        if causes::task_start(&mut context).is_err() {
            diagnostic(config, b"Capacity at 0\n");
            return 101;
        }
        let mut result = MainResult::default();
        let failed = entry(&mut context, config, &mut result);
        if failed != 0 {
            for cause in context.causes.iter() {
                if let Some(describe) = config.describe {
                    let line = describe(config.state, cause);
                    (config.diagnostic)(config.state, line.data, line.len);
                    diagnostic(config, b"\n");
                } else {
                    let name = [
                        "Overflow",
                        "Bounds",
                        "Division",
                        "Unwrap",
                        "Fail",
                        "Cancelled",
                        "Capacity",
                    ][cause.kind as usize];
                    diagnostic(config, format!("{name} at {}\n", cause.site).as_bytes());
                }
            }
            101
        } else {
            match result.kind {
                0 => 0,
                2 if (1..=100).contains(&result.exit) => result.exit,
                _ => 1,
            }
        }
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
pub unsafe extern "C" fn hirsel_core_args_len(config: *const Config) -> u64 {
    // SAFETY: the run owns immutable argument descriptors.
    unsafe { (*config).args_len as u64 }
}
#[repr(C)]
pub struct Argument {
    pub value: View,
    pub present: bool,
}
/// # Safety
/// Same as args_len. Index is user input and is checked before address arithmetic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_args_get(config: *const Config, index: u64) -> Argument {
    // SAFETY: configuration is live; only a checked index reads its argument table.
    unsafe {
        let config = &*config;
        if index >= config.args_len as u64 {
            return Argument {
                value: View {
                    data: std::ptr::null(),
                    len: 0,
                },
                present: false,
            };
        }
        Argument {
            value: *config.args.add(index as usize),
            present: true,
        }
    }
}

/// Run stops skip cleanup. Only async-signal-safe writes and _exit are used so
/// the stack overflow handler can take the same path from its alternate stack.
pub fn stop(name: &[u8]) -> ! {
    // SAFETY: static/borrowed bytes remain live until _exit, no Rust unwinding.
    unsafe {
        libc::write(2, name.as_ptr().cast(), name.len());
        libc::write(2, b"\n".as_ptr().cast(), 1);
        libc::_exit(102)
    }
}

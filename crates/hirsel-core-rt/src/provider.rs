//! CAP-8 issue/delivery seam. Host adapters and scripted adapters use the same ABI.
use crate::{Context, causes};

pub const OPEN: u8 = 0;
pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const CLOSE: u8 = 3;
pub const TRY_LOCK: u8 = 4;
pub const UNLOCK: u8 = 5;
pub const MKDIR: u8 = 6;
pub const MKDTEMP: u8 = 7;
pub const REMOVE: u8 = 8;
pub const REMOVE_TREE: u8 = 9;
pub const WALL_MS: u8 = 10;
pub const MONO_NS: u8 = 11;
pub const STDOUT: u8 = 12;
pub const STDERR: u8 = 13;
pub const RENAME: u8 = 14;
pub const SLEEP_UNTIL: u8 = 15;
pub const SYNC: u8 = 16;
pub const SYNC_DIR: u8 = 17;
pub const RANDOM: u8 = 18;

/// IoError tags. Other carries the host error number.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IoError {
    pub tag: u8,
    pub code: u32,
}
impl IoError {
    pub const OK: Self = Self { tag: 0, code: 0 };
    pub fn from_errno(code: i32) -> Self {
        let tag = match code {
            libc::ENOENT => 1,
            libc::EACCES | libc::EPERM | libc::EXDEV | libc::ELOOP => 2,
            libc::EEXIST => 3,
            libc::EBADF => 4,
            _ => 5,
        };
        Self {
            tag,
            code: if tag == 5 { code as u32 } else { 0 },
        }
    }
}

/// FORM String/Bytes carrier. Capacity 0 borrows storage even when data is
/// nonnull and len is nonzero. Read/mkdtemp never grow; writes only read len.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
    pub capacity: usize,
}
#[repr(C)]
pub struct Request {
    pub operation: u8,
    pub mode: u8,
    pub handle: u64,
    pub path: *const u8,
    pub path_len: usize,
    pub buffer: Buffer,
    pub site: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    pub value: u64,
    pub error: IoError,
    pub written: u64,
    pub status: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Wait {
    pub kind: u8,
    pub deadline_ns: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Step {
    pub kind: u8,
    pub outcome: Outcome,
    pub deadline_ns: u64,
}
pub const WAIT_READY: u8 = 0;
pub const WAIT_UNTIL: u8 = 1;
pub const WAIT_PARK: u8 = 2;
pub const STEP_DONE: u8 = 0;
pub const STEP_PENDING: u8 = 1;
unsafe extern "C" fn wait_ready(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait {
        kind: WAIT_READY,
        deadline_ns: 0,
    }
}

/// Four-state provider ABI: C (cancelled), W (waiting), I (issued), D (delivered).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Provider {
    pub state: *mut u8,
    pub wait: Option<unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Wait>,
    pub issue: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step,
    pub resume: Option<unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step>,
    pub abort: Option<unsafe extern "C" fn(*mut u8, *mut Request, u64, u8) -> Outcome>,
    pub deliver: unsafe extern "C" fn(*mut u8, *mut Request, Outcome) -> Outcome,
}

/// # Safety
/// All pointers and buffer extents are the emitter's live borrowed operands.
/// Provider callbacks obey the operation's custody and partial-effect contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
) -> Outcome {
    // SAFETY: exclusively borrowed task context/request for this operation.
    unsafe {
        let request = &mut *request;
        if request.operation != WALL_MS && request.operation != MONO_NS {
            if crate::tasks::check_cancel(context, request.site) != 0 {
                return Outcome {
                    status: 1,
                    ..Outcome::default()
                };
            }
            if request.operation == MKDTEMP
                && request
                    .buffer
                    .len
                    .checked_add(32)
                    .is_none_or(|n| n > request.buffer.capacity)
            {
                causes::record(
                    &mut *context,
                    causes::Cause::new(causes::BOUNDS, request.site),
                );
                return Outcome {
                    status: 1,
                    ..Outcome::default()
                };
            }
        }
        let provider = &*provider;
        let previous = crate::os::set_current_context(context);
        let token = crate::tasks::current_id();
        let wait = provider.wait.unwrap_or(wait_ready);
        loop {
            let w = wait(provider.state, request, token);
            match w.kind {
                WAIT_READY => break,
                WAIT_UNTIL => {
                    let now = crate::os::monotonic_ns();
                    let duration =
                        std::time::Duration::from_nanos(w.deadline_ns.saturating_sub(now));
                    crate::tasks::sleep_for(
                        context,
                        duration.min(std::time::Duration::from_millis(1)),
                    );
                }
                WAIT_PARK => crate::tasks::park_provider(token),
                _ => break,
            }
            if crate::tasks::check_cancel(context, request.site) != 0 {
                crate::os::set_current_context(previous);
                return Outcome {
                    status: 1,
                    ..Outcome::default()
                };
            }
        }
        let mut step = (provider.issue)(provider.state, request, token);
        while step.kind == STEP_PENDING {
            if crate::tasks::check_cancel(context, request.site) != 0 {
                step.outcome = provider.abort.map_or(
                    Outcome {
                        status: 1,
                        ..Outcome::default()
                    },
                    |abort| abort(provider.state, request, token, 1),
                );
                break;
            }
            if step.deadline_ns != 0 {
                let now = crate::os::monotonic_ns();
                if now >= step.deadline_ns {
                    if let Some(abort) = provider.abort {
                        step.outcome = abort(provider.state, request, token, 2);
                    }
                    break;
                }
                crate::tasks::sleep_for(
                    context,
                    std::time::Duration::from_nanos(step.deadline_ns - now),
                );
            } else {
                crate::tasks::park_provider(token);
            }
            step = provider.resume.map_or(
                Step {
                    kind: STEP_DONE,
                    ..step
                },
                |resume| resume(provider.state, request, token),
            );
        }
        let outcome = (provider.deliver)(provider.state, request, step.outcome);
        crate::os::set_current_context(previous);
        outcome
    }
}

/// Delivery for the OS provider. Recorders replace it to capture result bytes.
/// # Safety
/// Same borrowed request and state contract as Provider::deliver.
pub unsafe extern "C" fn deliver(_: *mut u8, _: *mut Request, result: Outcome) -> Outcome {
    result
}

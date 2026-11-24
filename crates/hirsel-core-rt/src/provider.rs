//! CAP-8 issue/delivery seam. Host adapters and scripted adapters use the same ABI.
use crate::{Context, causes};

pub use hirsel_core_ids::host_op::*;

/// IoError tags. Other carries the host error number.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IoError {
    pub tag: u8,
    pub code: u32,
}
/// macOS's ENOTCAPABLE.
const ENOTCAPABLE: i32 = 107;

impl IoError {
    pub const OK: Self = Self { tag: 0, code: 0 };
    pub fn from_errno(code: i32) -> Self {
        let tag = match code {
            libc::ENOENT => 1,
            libc::EACCES | libc::EPERM | libc::EXDEV | libc::ELOOP => 2,
            // macOS reports a path that leaves its O_RESOLVE_BENEATH root
            // with ENOTCAPABLE, where Linux openat2 reports EXDEV.
            ENOTCAPABLE if cfg!(target_os = "macos") => 2,
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
impl Request {
    pub(crate) fn offset(&self) -> u64 {
        // SAFETY: positioned requests pin eight little-endian offset bytes.
        u64::from_le_bytes(unsafe { self.path.cast::<[u8; 8]>().read() })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    pub value: u64,
    pub error: IoError,
    pub written: u64,
    pub status: u8,
    /// File kind for a successful STAT or STAT_FILE.
    pub kind: u8,
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
/// Whether an operation of this kind is a cancellation point: every `waits`
/// operation except the clock reads (CONC-7; REPLAY-SPEC section 3, C).
#[inline(always)]
pub(crate) fn cancellation_point(operation: u8) -> bool {
    !matches!(
        operation,
        WALL_MS
            | MONO_NS
            | SECRET_MAC
            | SECRET_KDF
            | SCRAM_VERIFY
            | crate::foreign::handles::RELEASE_WAIT_OP
    )
}
pub const ABORT_CANCEL: u8 = 1;
pub const ABORT_DEADLINE: u8 = 2;

/// # Safety
/// Provider and request obey the pinned callback contract for a first issue.
#[inline(always)]
pub(crate) unsafe fn issue(provider: &Provider, request: *mut Request, token: u64) -> Step {
    unsafe {
        if (*request).operation == SCRAM {
            #[cfg(feature = "record")]
            let live = !crate::tape::owns(provider);
            #[cfg(not(feature = "record"))]
            let live = true;
            if live {
                crate::scram::issue(&*request);
            }
        }
        (provider.issue)(provider.state, request, token)
    }
}

/// The four-state provider ABI (REPLAY-SPEC section 3): an operation is C (cancelled
/// at entry), W (waiting before issue), I (issued, in progress) or D (delivered).
/// Every callback is required. `wait`, `resume` and `abort` are called by the
/// scheduler at the moment of a wake, outside any task, and must not block;
/// `issue` and `deliver` run in the task. `Wait::deadline_ns` and
/// `Step::deadline_ns` use the scheduler's monotonic clock domain: host
/// CLOCK_MONOTONIC for OS runs, simulated nanoseconds for model runs.
/// A step's 0 is none.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Provider {
    pub state: *mut u8,
    pub wait: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Wait,
    pub issue: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step,
    pub resume: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Step,
    pub abort: unsafe extern "C" fn(*mut u8, *mut Request, u64, u8) -> Outcome,
    pub deliver: unsafe extern "C" fn(*mut u8, *mut Request, Outcome) -> Outcome,
}

/// Installation is where a provider is refused: a C provider with a NULL
/// callback stops the run (`Provider`) before main, never at an operation.
/// # Safety
/// A non-null provider points at six readable pointer-sized words.
#[cfg_attr(hirsel_runtime_size, optimize(size))]
pub(crate) unsafe fn install(provider: *const Provider) {
    if provider.is_null() {
        return;
    }
    // SAFETY: read as plain words, so a NULL never becomes a Rust fn value.
    let words = unsafe { provider.cast::<[usize; 6]>().read() };
    if words[1..].contains(&0) {
        crate::process::stop(b"Provider");
    }
}

/// # Safety
/// All pointers and buffer extents are the emitter's live borrowed operands, and
/// the provider is installed. Callbacks obey the operation's custody contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
) -> Outcome {
    // SAFETY: forwarded caller contract.
    unsafe {
        drive_with(context, provider, request, None, |provider, request| {
            crate::net::authorize(provider, request)
        })
    }
}

/// A clock read (WALL_MS or MONO_NS). On the OS provider this call form runs
/// its `wait`, `issue` and `deliver` bodies directly: a clock read never waits,
/// parks or is a cancellation point, so it takes no token and no pending state,
/// and reads on the task's stack (`os::clock`). Every other provider takes
/// `drive`. Either form passes the same entry and replay boundary (CAP-5, CAP-8).
/// # Safety
/// Same contract as `hirsel_core_host`, for a clock request.
#[inline(always)]
pub(crate) unsafe fn clock(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
) -> Outcome {
    // SAFETY: forwarded caller contract.
    unsafe {
        if !crate::os::owns(&*provider) {
            return drive(context, provider, request, None);
        }
        crate::process::deny_describe_capability(context);
        let _commit = crate::tasks::before_provider_entry();
        #[cfg(test)]
        crate::os::probe::note_outside(crate::os::probe::WAIT, request, 0);
        let outcome = crate::os::clock((*provider).state, request);
        #[cfg(test)]
        {
            let answer = crate::os::probe::answer(&outcome);
            crate::os::probe::note(crate::os::probe::ISSUE, request, answer);
            crate::os::probe::note(crate::os::probe::DELIVER, request, answer);
        }
        outcome
    }
}

/// The failure of an operation that ended before issue.
pub(crate) const CANCELLED: Outcome = Outcome {
    value: 0,
    error: IoError::OK,
    written: 0,
    status: 1,
    kind: 0,
};
/// Run one operation through C, W, I and D. Returns the outcome `deliver`
/// returned and records in `issued` the one the provider produced (issue,
/// resume or abort); or returns Cancelled, recording nothing, when the
/// operation ended before issue: in C, cancelled in W, or refused for bounds.
/// Nothing recorded means the provider never saw the operation issued, so
/// whatever it would have consumed is still the caller's (host_file CLOSE).
/// # Safety
/// Same contract as `hirsel_core_host`.
#[inline(always)]
pub(crate) unsafe fn drive(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
    issued: Option<&mut Option<Outcome>>,
) -> Outcome {
    // SAFETY: forwarded, for operations that need no Net authorization.
    unsafe { drive_with(context, provider, request, issued, |_, _| true) }
}

#[inline(always)]
pub(crate) unsafe fn drive_with(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
    issued: Option<&mut Option<Outcome>>,
    authorize: impl FnOnce(&Provider, &mut Request) -> bool,
) -> Outcome {
    // SAFETY: forwarded; the stackful form always ends with an outcome.
    unsafe {
        admit::<false>(context, provider, request, issued, authorize)
            .unwrap_or_else(|| unreachable!("a stackful operation never returns parked"))
    }
}
/// A Net operation in the returnable form (`net::returnable_op`): the same
/// entry, replay boundary, cancellation, bounds and authorization as
/// `hirsel_core_host`, once. None means it parked in the task's frame record.
/// # Safety
/// As `hirsel_core_host`; the request lives in the current task's frame record.
pub(crate) unsafe fn returnable(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
) -> Option<Outcome> {
    // SAFETY: forwarded caller contract.
    unsafe {
        admit::<true>(context, provider, request, None, |provider, request| {
            crate::net::authorize(provider, request)
        })
    }
}
#[inline(always)]
unsafe fn admit<const RETURNABLE: bool>(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
    issued: Option<&mut Option<Outcome>>,
    authorize: impl FnOnce(&Provider, &mut Request) -> bool,
) -> Option<Outcome> {
    // SAFETY: exclusively borrowed task context/request for this operation.
    unsafe {
        crate::process::deny_describe_capability(context);
        let _commit = crate::tasks::before_provider_entry();
        let site = (*request).site;
        let operation = (*request).operation;
        if cancellation_point(operation) {
            if crate::tasks::check_cancel(context, site) != 0 {
                return Some(CANCELLED);
            }
            let request = &*request;
            if (operation == MKDTEMP
                && request.buffer.capacity.saturating_sub(request.buffer.len) < 32)
                || (operation == NEXT_ENTRIES
                    && request.buffer.capacity.saturating_sub(request.buffer.len) < 258)
                || (operation == WRITE_AT
                    && request
                        .offset()
                        .checked_add(request.buffer.len as u64)
                        .is_none())
            {
                causes::record(&mut *context, causes::Cause::new(causes::BOUNDS, site));
                return Some(CANCELLED);
            }
        }
        if (operation == READ_AT && !matches!((*request).mode, 0 | 4))
            || (operation == WRITE_AT && !matches!((*request).mode, 1 | 3 | 4))
        {
            return Some(Outcome {
                error: IoError::from_errno(libc::EACCES),
                ..Outcome::default()
            });
        }
        if !authorize(&*provider, &mut *request) {
            return Some(crate::net::DENIED);
        }
        if RETURNABLE {
            crate::tasks::provider_operation_returnable(context, &*provider, request)
        } else {
            Some(crate::tasks::provider_operation(
                context, &*provider, request, issued,
            ))
        }
    }
}

/// `deliver` as a provider callback, for recognising the OS provider's own.
pub(crate) const DELIVER: unsafe extern "C" fn(*mut u8, *mut Request, Outcome) -> Outcome = deliver;
/// Delivery for the OS provider. Recorders replace it to capture result bytes.
/// Never inlined, so it has one address and `DELIVER` recognises it: a small
/// function is otherwise copied into each codegen unit that names it.
/// # Safety
/// Same borrowed request and state contract as Provider::deliver.
#[inline(never)]
pub unsafe extern "C" fn deliver(_: *mut u8, _request: *mut Request, result: Outcome) -> Outcome {
    #[cfg(test)]
    crate::os::probe::note(
        crate::os::probe::DELIVER,
        _request,
        crate::os::probe::answer(&result),
    );
    result
}

/// Callbacks for test providers whose operations are always Ready and Done:
/// the scheduler never resumes or aborts them (REPLAY-SPEC section 3).
#[cfg(test)]
pub(crate) mod immediate {
    use super::*;
    pub(crate) unsafe extern "C" fn wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
        Wait::default()
    }
    pub(crate) unsafe extern "C" fn resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
        unreachable!("an immediate provider never answers Pending")
    }
    pub(crate) unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
        unreachable!("an immediate provider never answers Pending")
    }
}

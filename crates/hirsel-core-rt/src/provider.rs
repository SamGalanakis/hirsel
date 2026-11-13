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

/// Both callbacks may suspend at scheduler boundaries. Issue may supply a replayed
/// result instead of touching the OS. Delivery always runs after an issued call,
/// even if cancellation was requested meanwhile. State is host-owned for the run.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Provider {
    pub state: *mut u8,
    pub issue: unsafe extern "C" fn(*mut u8, *mut Request) -> Outcome,
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
                    causes::Cause {
                        kind: causes::BOUNDS,
                        site: request.site,
                        message: Default::default(),
                    },
                );
                return Outcome {
                    status: 1,
                    ..Outcome::default()
                };
            }
        }
        let provider = &*provider;
        let outcome = (provider.issue)(provider.state, request);
        (provider.deliver)(provider.state, request, outcome)
    }
}

/// Delivery for the OS provider. Recorders replace it to capture result bytes.
/// # Safety
/// Same borrowed request and state contract as Provider::deliver.
pub unsafe extern "C" fn deliver(_: *mut u8, _: *mut Request, result: Outcome) -> Outcome {
    result
}

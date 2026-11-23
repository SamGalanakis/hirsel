//! FORM #50's closed provider rows. No operation allocates program storage.
use crate::{Context, entry::*, host_file::*, provider::*};
use hirsel_core_ids::host_op::{DIR_READ_ONLY, needs_writable_dir};
use std::ptr;

fn request(operation: u8, mode: u8, site: u64, buffer: Buffer) -> Request {
    Request {
        operation,
        mode,
        handle: 0,
        path: ptr::null(),
        path_len: 0,
        buffer,
        site,
    }
}

/// Deny a checked operation at entry, without issuing it to a provider.
///
/// # Safety
/// The context belongs to the currently running task.
pub(crate) unsafe fn denied(context: *mut Context, site: u64) -> Outcome {
    // SAFETY: the caller owns this task's entry and context.
    unsafe {
        crate::process::deny_describe_capability(context);
        crate::tasks::before_provider_entry();
        if crate::tasks::check_cancel(context, site) != 0 {
            return CANCELLED;
        }
    }
    Outcome {
        error: IoError::from_errno(libc::EACCES),
        ..Outcome::default()
    }
}

/// # Safety
/// The carrier is live, or null for a row without an operand.
unsafe fn carrier(operand: *mut Buffer) -> Buffer {
    if operand.is_null() {
        Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    } else {
        // SAFETY: copy the carrier so borrowed bytes remain immutable.
        unsafe { *operand }
    }
}

/// Random and stream operations. The checked operation determines the
/// shape of its operands, as for `hirsel_core_host_op`.
///
/// # Safety
/// Context is running and handle designates the checked capability. Operand is
/// a live String or Bytes carrier, or null if the operation needs neither.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_io(
    context: *mut Context,
    op: u8,
    handle: u64,
    operand: *mut Buffer,
    mode: u8,
    site: u64,
) -> Outcome {
    // SAFETY: the checked row supplies the correct capability and carrier.
    unsafe {
        let buffer = carrier(operand);
        let mut request = request(op, mode, site, buffer);
        let provider = hirsel_core_capability_provider(context, handle);
        request.handle = hirsel_core_capability_handle(context, handle);
        let result = drive(context, provider, &mut request, None);
        if op == crate::provider::RANDOM && request.buffer.len != buffer.len {
            (*operand).len = request.buffer.len;
        }
        result
    }
}

/// Read a clock through the capability's provider.
///
/// # Safety
/// Context is running, op is WallMs or MonoNs, and handle designates the
/// checked Clock capability. Operand follows the common host ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_clock(
    context: *mut Context,
    op: u8,
    handle: u64,
    operand: *mut Buffer,
    mode: u8,
    site: u64,
) -> Outcome {
    // SAFETY: the checked clock row supplies its capability and request.
    unsafe {
        let mut request = request(op, mode, site, carrier(operand));
        let provider = hirsel_core_capability_provider(context, handle);
        request.handle = hirsel_core_capability_handle(context, handle);
        crate::provider::clock(context, provider, &mut request)
    }
}

/// # Safety
/// Context is running; op is a HostOp code and handle has that row's checked
/// capability/HostFile type. Operand is the row's live String/Bytes carrier, or
/// null for a row without one. Capacity-0 storage is borrowed and immutable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_op(
    context: *mut Context,
    op: u8,
    handle: u64,
    operand: *mut Buffer,
    mode: u8,
    site: u64,
) -> Outcome {
    // SAFETY: FORM supplies the operation-specific operands; copy the carrier so
    // immutable path/write/argv carriers are never mutably borrowed or rewritten.
    unsafe {
        let buffer = carrier(operand);
        let mut request = request(op, mode, site, buffer);
        let outcome = match op {
            READ | WRITE | CLOSE | TRY_LOCK | UNLOCK | SYNC | NEXT_ENTRIES | STAT_FILE => {
                hirsel_core_host_file(context, handle, &mut request)
            }
            OPEN | RENAME | MKDIR | MKDTEMP | REMOVE | REMOVE_TREE | SYNC_DIR | OPEN_DIR
            | ENTRIES | STAT => {
                let rights = handle & DIR_READ_ONLY;
                if rights != 0 && needs_writable_dir(op, mode) {
                    return denied(context, site);
                }
                let handle = handle & !DIR_READ_ONLY;
                let provider = hirsel_core_capability_provider(context, handle);
                request.handle = hirsel_core_capability_handle(context, handle);
                if matches!(op, OPEN | MKDIR | REMOVE | REMOVE_TREE | OPEN_DIR | STAT) {
                    request.path = buffer.data;
                    request.path_len = buffer.len;
                }
                if matches!(op, OPEN | ENTRIES) {
                    open_file(context, provider, &mut request, rights != 0)
                } else {
                    let mut result = drive(context, provider, &mut request, None);
                    if matches!(op, MKDTEMP | OPEN_DIR)
                        && result.status == 0
                        && result.error == IoError::OK
                    {
                        result.value = (*(*context).host_files)
                            .registry
                            .grant(*provider, result.value)
                            | rights;
                    }
                    result
                }
            }
            WALL_MS | MONO_NS => {
                return hirsel_core_host_clock(context, op, handle, operand, mode, site);
            }
            crate::provider::RANDOM | crate::provider::STDOUT | crate::provider::STDERR => {
                return hirsel_core_host_io(context, op, handle, operand, mode, site);
            }
            _ => unreachable!(),
        };
        let appended = matches!(op, READ | MKDTEMP)
            || (op == NEXT_ENTRIES && outcome.status == 0 && outcome.error == IoError::OK);
        if appended && request.buffer.len != buffer.len {
            // Only these rows write the length; spare-capacity checks guarantee
            // owned mutable storage. They never change data or capacity.
            (*operand).len = request.buffer.len;
        }
        outcome
    }
}

/// Metadata rows expose the outcome's kind byte as an additional FORM result.
///
/// # Safety
/// The checked row supplies a Dir and path, or a live HostFile and null operand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_stat(
    context: *mut Context,
    op: u8,
    handle: u64,
    operand: *mut Buffer,
    mode: u8,
    site: u64,
) -> Outcome {
    // SAFETY: the two stat rows use the common provider calling convention.
    unsafe { hirsel_core_host_op(context, op, handle, operand, mode, site) }
}

/// Positioned I/O carries its offset in the request's eight-byte path operand.
///
/// # Safety
/// The checked operation owns the file key and supplies a live Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_at(
    context: *mut Context,
    op: u8,
    handle: u64,
    at: u64,
    operand: *mut Buffer,
    site: u64,
) -> Outcome {
    let offset = at.to_le_bytes();
    // SAFETY: the checked row pins the carrier and the offset through delivery.
    unsafe {
        let buffer = *operand;
        let mut request = request(op, 0, site, buffer);
        request.path = offset.as_ptr();
        request.path_len = offset.len();
        let outcome = hirsel_core_host_file(context, handle, &mut request);
        if op == READ_AT
            && outcome.status == 0
            && outcome.error == IoError::OK
            && request.buffer.len != buffer.len
        {
            (*operand).len = request.buffer.len;
        }
        outcome
    }
}

/// Host rename uses two path carriers; the combined request is only an issue-time view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_rename(
    context: *mut Context,
    handle: u64,
    from: *mut Buffer,
    to: *mut Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        if handle & DIR_READ_ONLY != 0 {
            return denied(context, site);
        }
        let a = *from;
        let b = *to;
        let mut path = Vec::with_capacity(a.len + 1 + b.len);
        path.extend_from_slice(std::slice::from_raw_parts(a.data, a.len));
        path.push(0);
        path.extend_from_slice(std::slice::from_raw_parts(b.data, b.len));
        let mut request = Request {
            operation: RENAME,
            mode: 0,
            handle,
            path: path.as_ptr(),
            path_len: path.len(),
            buffer: Buffer {
                data: std::ptr::null_mut(),
                len: 0,
                capacity: 0,
            },
            site,
        };
        let provider = hirsel_core_capability_provider(context, handle);
        request.handle = hirsel_core_capability_handle(context, handle);
        drive(context, provider, &mut request, None)
    }
}

/// Cancellable clock wait. The deadline is carried in a temporary issue buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_sleep_until(
    context: *mut Context,
    handle: u64,
    deadline: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { clock_wait(context, SLEEP_UNTIL, handle, deadline, site) }
}

/// Cancellable wait until the clock's wall time reaches `wall_ms`, carried as
/// SLEEP_UNTIL's deadline is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_sleep_until_wall(
    context: *mut Context,
    handle: u64,
    wall_ms: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { clock_wait(context, SLEEP_UNTIL_WALL, handle, wall_ms, site) }
}

unsafe fn clock_wait(
    context: *mut Context,
    operation: u8,
    handle: u64,
    at: u64,
    site: u64,
) -> Outcome {
    unsafe {
        let mut bytes = at.to_ne_bytes();
        let mut request = Request {
            operation,
            mode: 0,
            handle,
            path: std::ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 8,
                capacity: 8,
            },
            site,
        };
        let provider = hirsel_core_capability_provider(context, handle);
        request.handle = hirsel_core_capability_handle(context, handle);
        drive(context, provider, &mut request, None)
    }
}

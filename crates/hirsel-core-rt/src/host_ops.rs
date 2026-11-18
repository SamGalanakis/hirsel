//! FORM #50's closed provider rows. No operation allocates program storage.
use crate::{Context, entry::*, host_file::*, provider::*};
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

/// Clock, random and stream operations. The checked operation determines the
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
        let result = if matches!(op, WALL_MS | MONO_NS) {
            crate::provider::clock(context, provider, &mut request)
        } else {
            drive(context, provider, &mut request, None)
        };
        if op == crate::provider::RANDOM && request.buffer.len != buffer.len {
            (*operand).len = request.buffer.len;
        }
        result
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
            READ | WRITE | CLOSE | TRY_LOCK | UNLOCK | SYNC => {
                hirsel_core_host_file(context, handle, &mut request)
            }
            OPEN | RENAME | MKDIR | MKDTEMP | REMOVE | REMOVE_TREE | SYNC_DIR => {
                let provider = hirsel_core_capability_provider(context, handle);
                request.handle = hirsel_core_capability_handle(context, handle);
                if matches!(op, OPEN | MKDIR | REMOVE | REMOVE_TREE) {
                    request.path = buffer.data;
                    request.path_len = buffer.len;
                }
                if op == OPEN {
                    hirsel_core_host_file_open(context, provider, &mut request)
                } else {
                    let mut result = drive(context, provider, &mut request, None);
                    if op == MKDTEMP && result.status == 0 && result.error == IoError::OK {
                        result.value = (*(*context).host_files)
                            .registry
                            .grant(*provider, result.value);
                    }
                    result
                }
            }
            crate::provider::RANDOM
            | WALL_MS
            | MONO_NS
            | crate::provider::STDOUT
            | crate::provider::STDERR => {
                return hirsel_core_host_io(context, op, handle, operand, mode, site);
            }
            _ => unreachable!(),
        };
        if matches!(op, READ | MKDTEMP) && request.buffer.len != buffer.len {
            // Only these rows write the length; spare-capacity checks guarantee
            // owned mutable storage. They never change data or capacity.
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
    unsafe {
        let mut bytes = deadline.to_ne_bytes();
        let mut request = Request {
            operation: SLEEP_UNTIL,
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

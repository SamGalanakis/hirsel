//! Provider-aware mechanical HostFile ownership. The native carrier is a u64
//! run-local key; provider metadata is private and never read inline by emitters.
use crate::{Context, provider::*};
use std::{collections::BTreeMap, ptr};

#[derive(Clone, Copy)]
struct File {
    provider: Provider,
    handle: u64,
}
#[derive(Default)]
pub(crate) struct Files {
    next: u64,
    live: BTreeMap<u64, File>,
}
impl Files {
    fn insert(&mut self, file: File) -> u64 {
        self.next = self
            .next
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        self.live.insert(self.next, file);
        self.next
    }
    pub(crate) unsafe fn close_all(&mut self, context: *mut Context) {
        while let Some((_, file)) = self.live.pop_last() {
            // SAFETY: removed sole ownership before calling the pinned provider.
            unsafe { close(context, file) };
        }
    }
}

fn close_request(handle: u64, site: u64) -> Request {
    Request {
        operation: CLOSE,
        mode: 0,
        handle,
        path: ptr::null(),
        path_len: 0,
        buffer: Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
        site,
    }
}
unsafe fn issue(file: File, request: *mut Request) -> Outcome {
    // SAFETY: caller pins the request and provider state through both callbacks.
    unsafe {
        let result = (file.provider.issue)(file.provider.state, request);
        (file.provider.deliver)(file.provider.state, request, result)
    }
}
unsafe fn close(context: *mut Context, file: File) {
    // SAFETY: sole file owner; cleanup cannot observe cancellation. OS issue uses
    // the scheduler's guarded foreign-call stack, including during finalizers.
    unsafe {
        (*context).cancel_shield += 1;
        let _ = issue(file, &mut close_request(file.handle, 0));
        (*context).cancel_shield -= 1;
    }
}

/// Open through the selected provider and publish an eight-byte HostFile key.
/// No logical admission; registry storage is provider bookkeeping (MEM-24).
/// # Safety
/// Live run context, provider and OPEN request; callbacks obey CAP-8 and retain
/// their state until the run closes its grants. Success transfers sole ownership.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_file_open(
    context: *mut Context,
    provider: *const Provider,
    request: *mut Request,
) -> Outcome {
    // SAFETY: exclusive operation borrows; no registry borrow spans a callback.
    unsafe {
        if crate::tasks::check_cancel(context, (*request).site) != 0 {
            return Outcome {
                status: 1,
                ..Outcome::default()
            };
        }
        let provider = *provider;
        let issued = (provider.issue)(provider.state, request);
        let mut result = (provider.deliver)(provider.state, request, issued);
        if result.status == 0 && result.error == IoError::OK {
            result.value = (*(*context).host_files).insert(File {
                provider,
                handle: result.value,
            });
        } else if issued.status == 0 && issued.error == IoError::OK {
            // Delivery failed after a committed open: the unpublished file is ours.
            close(
                context,
                File {
                    provider,
                    handle: issued.value,
                },
            );
        }
        result
    }
}

/// Read/write/lock/unlock or explicitly close a HostFile through its provider.
/// CLOSE consumes the carrier even when cancelled before issue or close fails.
/// # Safety
/// File is a live sole-owned run-local key; request and its buffers are borrowed
/// through delivery. Source typing supplies a file operation (not OPEN).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_host_file(
    context: *mut Context,
    file: u64,
    request: *mut Request,
) -> Outcome {
    // SAFETY: handles are compiler-owned; remove before callbacks can suspend.
    unsafe {
        let registry = (*context).host_files;
        let owned = if (*request).operation == CLOSE {
            (*registry).live.remove(&file).unwrap()
        } else {
            (&(*registry).live)[&file]
        };
        (*request).handle = owned.handle;
        if (*request).operation == CLOSE {
            if crate::tasks::check_cancel(context, (*request).site) != 0 {
                close(context, owned);
                return Outcome {
                    status: 1,
                    ..Outcome::default()
                };
            }
            issue(owned, request)
        } else {
            hirsel_core_host(context, &owned.provider, request)
        }
    }
}

/// Mechanical drop: consume the file, bypass cancellation, ignore close outcome.
/// Admits nothing, returns no failure status, and never retries an OS close.
/// # Safety
/// File is the sole live carrier in this run, consumed exactly once by this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_hostfile_drop(context: *mut Context, file: u64) {
    // SAFETY: removing the sole ownership prevents teardown from closing twice.
    unsafe {
        let owned = (*(*context).host_files).live.remove(&file).unwrap();
        close(context, owned);
    }
}

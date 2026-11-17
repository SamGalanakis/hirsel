//! Provider-aware mechanical HostFile ownership. The native carrier is a u64
//! run-local key; provider metadata is private and never read inline by emitters.
use crate::{Context, provider::*};
use std::{collections::BTreeMap, ptr};

#[derive(Clone, Copy)]
struct File {
    group: Option<crate::tape::schedule::Group>,
    provider: Provider,
    handle: u64,
}
pub(crate) const DYNAMIC_GRANT: u64 = 1 << 63;
/// A substitute Args vector (tape.rs): pinned for the run, so its views stay
/// valid after the tape ends.
struct Vector {
    _bytes: Vec<Vec<u8>>,
    views: Vec<Buffer>,
}
#[derive(Default)]
pub(crate) struct Files {
    pub(crate) sockets: hirsel_core_net::Sockets,
    pub(crate) net_grants: Vec<crate::net::Grant>,
    next: u64,
    live: BTreeMap<u64, File>,
    grants: BTreeMap<u64, Box<File>>,
    tapes: BTreeMap<u64, crate::tape::Tape>,
    vectors: BTreeMap<u64, Vector>,
}
impl Files {
    pub(crate) fn tape(&mut self, tape: crate::tape::Tape) -> u64 {
        let id = self.id();
        self.tapes.insert(id, tape);
        id
    }
    pub(crate) fn tape_ref(&self, key: u64) -> &crate::tape::Tape {
        &self.tapes[&key]
    }
    pub(crate) fn untape(&mut self, key: u64) -> crate::tape::Tape {
        self.tapes.remove(&key).unwrap()
    }
    /// Pin a substitute Args vector and return its handle.
    pub(crate) fn vector(&mut self, bytes: Vec<Vec<u8>>) -> u64 {
        let id = DYNAMIC_GRANT | self.id();
        let views = bytes
            .iter()
            .map(|b| Buffer {
                data: b.as_ptr().cast_mut(),
                len: b.len(),
                capacity: 0,
            })
            .collect();
        self.vectors.insert(
            id,
            Vector {
                _bytes: bytes,
                views,
            },
        );
        id
    }
    fn id(&mut self) -> u64 {
        self.next = self
            .next
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        if self.next == DYNAMIC_GRANT {
            crate::process::stop(b"HostHandles");
        }
        self.next
    }
    fn insert(&mut self, file: File) -> u64 {
        let id = self.id();
        self.live.insert(id, file);
        id
    }
    pub(crate) fn grant(&mut self, provider: Provider, handle: u64) -> u64 {
        let id = DYNAMIC_GRANT | self.id();
        self.grants.insert(
            id,
            Box::new(File {
                provider,
                handle,
                group: None,
            }),
        );
        id
    }
    pub(crate) fn taped_grant(
        &mut self,
        provider: Provider,
        handle: u64,
        group: crate::tape::schedule::Group,
    ) -> u64 {
        let id = self.grant(provider, handle);
        self.grants.get_mut(&id).unwrap().group = Some(group);
        id
    }
    pub(crate) fn grant_group(&self, handle: u64) -> Option<crate::tape::schedule::Group> {
        self.grants.get(&handle).and_then(|g| g.group)
    }
    pub(crate) fn grant_provider(&self, handle: u64) -> *const Provider {
        &raw const self.grants[&handle].provider
    }
    pub(crate) fn grant_handle(&self, handle: u64) -> u64 {
        self.grants[&handle].handle
    }
    pub(crate) unsafe fn close_all(&mut self, context: *mut Context) {
        // A tape still open writes its buffered frames through its Dir's
        // provider, which outlives this teardown (section 8: as a crash).
        while let Some((_, tape)) = self.tapes.pop_last() {
            drop(tape);
        }
        self.sockets = hirsel_core_net::Sockets::default();
        self.net_grants.clear();
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
unsafe fn close(context: *mut Context, file: File) {
    // SAFETY: sole file owner; cleanup cannot observe cancellation, so the
    // driver's C check and W/I cancellation do not apply under the shield.
    unsafe {
        (*context).cancel_shield += 1;
        let _ = hirsel_core_host(context, &file.provider, &mut close_request(file.handle, 0));
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
        let mut issued = None;
        let mut result = drive(context, provider, request, Some(&mut issued));
        let Some(issued) = issued else {
            return result;
        };
        let provider = *provider;
        if result.status == 0 && result.error == IoError::OK {
            result.value = (*(*context).host_files).insert(File {
                group: None,
                provider,
                handle: result.value,
            });
        } else if issued.status == 0 && issued.error == IoError::OK {
            // Delivery failed after a committed open: the unpublished file is ours.
            close(
                context,
                File {
                    group: None,
                    provider,
                    handle: issued.value,
                },
            );
        }
        result
    }
}

/// Read/write/lock/unlock or explicitly close a HostFile through its provider.
/// CLOSE consumes the carrier even when cancelled before issue or close fails:
/// when it ends before issue (in C or cancelled in W) the provider never had
/// it, so it is closed here under a shield, once. An issued close belongs to
/// its provider, which completes it in issue, resume or abort; it is never
/// closed again. Every other operation borrows the file, which stays live
/// for its owner's close, drop or run teardown.
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
        if (*request).operation != CLOSE {
            return drive(context, &owned.provider, request, None);
        }
        let mut issued = None;
        let result = drive(context, &owned.provider, request, Some(&mut issued));
        if issued.is_none() {
            close(context, owned);
        }
        result
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

/// The argument vector `args` designates: a substitute's (tape/abi.rs), or
/// the run's own.
/// # Safety
/// A live run context; `args` is a live Args operand of the run.
pub(crate) unsafe fn args<'a>(context: *const Context, args: u64) -> &'a [Buffer] {
    // SAFETY: the run pins its argument table and every substitute vector.
    unsafe {
        if args & DYNAMIC_GRANT != 0 {
            &(&(*(*context).host_files).vectors)[&args].views
        } else {
            let run = &*(*context).run;
            if run.args_len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(run.args, run.args_len)
            }
        }
    }
}

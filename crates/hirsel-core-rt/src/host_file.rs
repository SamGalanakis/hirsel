//! Provider-aware mechanical HostFile ownership. The native carrier is a u64
//! run-local key; provider metadata is private and never read inline by emitters.
use crate::{Context, provider::*};
use std::{
    collections::BTreeMap,
    mem::ManuallyDrop,
    ptr,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
struct File {
    provider: Provider,
    handle: u64,
}
// SAFETY: transferring the opaque callback descriptor does not access its
// state. The runtime invokes it under the run's provider calling contract.
unsafe impl Send for File {}
// SAFETY: sharing this immutable descriptor grants no access to its state.
unsafe impl Sync for File {}

pub(crate) const DYNAMIC_GRANT: u64 = 1 << 63;
/// A substitute Args vector (tape.rs): pinned for the run, so its views stay
/// valid after the tape ends.
struct Vector {
    _bytes: Vec<Vec<u8>>,
    views: Vec<Buffer>,
}

// SAFETY: the views point into the vector's own heap allocations. Moving
// the owning entry between workers leaves the bytes and views in place.
unsafe impl Send for Vector {}

#[derive(Default)]
struct Entries {
    next: u64,
    live: Option<Resources<File>>,
    grants: BTreeMap<u64, Arc<File>>,
    vectors: BTreeMap<u64, Vector>,
}
/// Install cleanup together with the first owned resource. A run that never
/// publishes this kind carries neither its close path nor its map destructor.
struct Resources<T> {
    values: ManuallyDrop<BTreeMap<u64, T>>,
    close: unsafe fn(&mut BTreeMap<u64, T>, *mut Context),
    drop_values: fn(&mut ManuallyDrop<BTreeMap<u64, T>>),
}

impl<T> Resources<T> {
    fn new(close: unsafe fn(&mut BTreeMap<u64, T>, *mut Context)) -> Self {
        Self {
            values: ManuallyDrop::new(BTreeMap::new()),
            close,
            drop_values: |values| {
                // SAFETY: Drop owns the map and invokes this once.
                unsafe { ManuallyDrop::drop(values) }
            },
        }
    }

    unsafe fn close(&mut self, context: *mut Context) {
        // SAFETY: the run owns the registry and pins the context through cleanup.
        unsafe { (self.close)(&mut self.values, context) }
    }
}

impl<T> Drop for Resources<T> {
    fn drop(&mut self) {
        (self.drop_values)(&mut self.values)
    }
}

impl Entries {
    fn id(&mut self) -> u64 {
        self.next += 1;
        if self.next == DYNAMIC_GRANT {
            crate::process::stop(b"HostHandles");
        }
        self.next
    }
}

#[derive(Default)]
pub(crate) struct Registry(Mutex<Entries>);

#[derive(Default)]
pub(crate) struct Files {
    pub(crate) sockets: hirsel_core_net::Sockets,
    pub(crate) connected: std::collections::BTreeMap<u64, (crate::net::Address, Provider)>,
    pub(crate) tls: crate::os_tls::Streams,
    pub(crate) net_grants: Mutex<Vec<crate::net::Grant>>,
    pub(crate) env_grants: Mutex<Vec<crate::env::Grant>>,
    #[cfg(feature = "record")]
    pub(crate) net_tape: Option<crate::net::SocketTape>,
    pub(crate) secrets: Mutex<crate::secret::Secrets>,
    pub(crate) tls_writes: BTreeMap<u64, crate::secret::TlsWrite>,
    // Children inherit these entries without borrowing the scheduler's sockets.
    pub(crate) registry: Registry,
}
impl Registry {
    /// Pin a substitute Args vector and return its handle.
    pub(crate) fn vector(&self, bytes: Vec<Vec<u8>>) -> u64 {
        let views = bytes
            .iter()
            .map(|b| Buffer {
                data: b.as_ptr().cast_mut(),
                len: b.len(),
                capacity: 0,
            })
            .collect();
        let mut registry = self.0.lock().unwrap();
        let id = DYNAMIC_GRANT | registry.id();
        registry.vectors.insert(
            id,
            Vector {
                _bytes: bytes,
                views,
            },
        );
        id
    }
    fn views(&self, key: u64) -> &[Buffer] {
        let views = &raw const self.0.lock().unwrap().vectors[&key].views[..];
        // SAFETY: the slice points into a pinned Vec allocation never mutated
        // or removed while Files is shared.
        unsafe { &*views }
    }
    fn insert(&self, file: File) -> u64 {
        let mut registry = self.0.lock().unwrap();
        let id = registry.id();
        registry
            .live
            .get_or_insert_with(|| Resources::new(close_files))
            .values
            .insert(id, file);
        id
    }
    fn file(&self, key: u64) -> File {
        self.0.lock().unwrap().live.as_ref().unwrap().values[&key]
    }
    fn take_file(&self, key: u64) -> File {
        self.0
            .lock()
            .unwrap()
            .live
            .as_mut()
            .unwrap()
            .values
            .remove(&key)
            .unwrap()
    }
    fn insert_grant(&self, file: File) -> u64 {
        let mut registry = self.0.lock().unwrap();
        let id = DYNAMIC_GRANT | registry.id();
        registry.grants.insert(id, Arc::new(file));
        id
    }
    pub(crate) fn grant(&self, provider: Provider, handle: u64) -> u64 {
        self.insert_grant(File { provider, handle })
    }
    pub(crate) fn grant_provider(&self, handle: u64) -> *const Provider {
        let file = Arc::as_ptr(&self.0.lock().unwrap().grants[&handle]);
        // SAFETY: grants stay pinned until teardown.
        unsafe { &raw const (*file).provider }
    }
    pub(crate) fn grant_handle(&self, handle: u64) -> u64 {
        self.0.lock().unwrap().grants[&handle].handle
    }
}
impl Files {
    pub(crate) unsafe fn close_all(&mut self, context: *mut Context) {
        self.sockets = hirsel_core_net::Sockets::default();
        self.tls.sessions.clear();
        self.connected.clear();
        self.net_grants.get_mut().unwrap().clear();
        self.env_grants.get_mut().unwrap().clear();
        *self.secrets.get_mut().unwrap() = crate::secret::Secrets::default();
        if let Some(files) = &mut self.registry.0.get_mut().unwrap().live {
            // SAFETY: the run's Context and providers remain pinned.
            unsafe { files.close(context) };
        }
    }
}

unsafe fn close_files(files: &mut BTreeMap<u64, File>, context: *mut Context) {
    while let Some((_, file)) = files.pop_last() {
        // SAFETY: removed sole ownership before calling the pinned provider.
        unsafe { close(context, file) };
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
        let _ = drive(
            context,
            &file.provider,
            &mut close_request(file.handle, 0),
            None,
        );
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
            result.value = (*(*context).host_files).registry.insert(File {
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
            (*registry).registry.take_file(file)
        } else {
            (*registry).registry.file(file)
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
        let owned = (*(*context).host_files).registry.take_file(file);
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
            (*(*context).host_files).registry.views(args)
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

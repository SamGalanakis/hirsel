//! Provider-aware ownership of mechanical files and listings. The carrier is a u64
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
    mode: u8,
    close_op: u8,
}
// SAFETY: transferring the opaque callback descriptor does not access its
// state. The runtime invokes it under the run's provider calling contract.
unsafe impl Send for File {}
// SAFETY: sharing this immutable descriptor grants no access to its state.
unsafe impl Sync for File {}

pub(crate) const DYNAMIC_GRANT: u64 = 1 << 63;
/// A tape's Args vector, pinned for the lifetime of the run.
struct Vector {
    _bytes: Vec<Vec<u8>>,
    views: Vec<Buffer>,
}

// SAFETY: the views point into the vector's own heap allocations. Moving
// the owning entry between workers leaves the bytes and views in place.
unsafe impl Send for Vector {}

struct DynamicGrant {
    provider: Provider,
    handle: u64,
    terminal: bool,
}

// SAFETY: the provider descriptor is immutable and its state is only accessed
// under the run's provider calling contract.
unsafe impl Send for DynamicGrant {}
unsafe impl Sync for DynamicGrant {}

#[derive(Default)]
struct Entries {
    next: u64,
    live: Option<Resources<File>>,
}

#[derive(Default)]
struct Designations {
    next: u64,
    grants: BTreeMap<u64, Arc<DynamicGrant>>,
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

fn next_id(next: &mut u64) -> u64 {
    *next += 1;
    if *next == DYNAMIC_GRANT {
        crate::process::stop(b"HostHandles");
    }
    *next
}

#[derive(Default)]
pub(crate) struct Registry {
    owned: Mutex<Entries>,
    pub(crate) run: Arc<RunGrants>,
}

/// Copyable designations and their backing storage live through the run.
#[derive(Default)]
pub(crate) struct RunGrants {
    designations: Mutex<Designations>,
    pub(crate) net_grants: Mutex<Vec<crate::net::Grant>>,
    pub(crate) env_grants: Mutex<Vec<crate::env::Grant>>,
    pub(crate) secrets: Mutex<crate::secret::Secrets>,
    pub(crate) cells: crate::provide::Cells,
}

// SAFETY: the tables use mutexes and pin the storage behind each designation.
// Outstanding charges pin their payer slots. Refunds can settle from any unit.
unsafe impl Send for RunGrants {}
// SAFETY: table mutation is synchronized. The run retains the storage until
// every unit has stopped using its designations.
unsafe impl Sync for RunGrants {}

pub(crate) struct Files {
    pub(crate) sockets: hirsel_core_net::Sockets,
    pub(crate) connected: std::collections::BTreeMap<u64, (crate::net::Address, Provider)>,
    pub(crate) tls: crate::os_tls::Streams,
    #[cfg(feature = "record")]
    pub(crate) net_tape: Option<crate::net::SocketTape>,
    #[cfg(feature = "record")]
    pub(crate) secret_mac_tape: Option<crate::secret::MacTape>,
    #[cfg(feature = "record")]
    pub(crate) managed_control: (u64, u64),
    pub(crate) tls_writes: BTreeMap<u64, crate::secret::TlsWrite>,
    pub(crate) registry: Registry,
}
impl Registry {
    pub(crate) fn for_unit(&self) -> Self {
        Self {
            owned: Mutex::default(),
            run: self.run.clone(),
        }
    }

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
        let mut registry = self.run.designations.lock().unwrap();
        let id = DYNAMIC_GRANT | next_id(&mut registry.next);
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
        let views = &raw const self.run.designations.lock().unwrap().vectors[&key].views[..];
        // SAFETY: the slice points into a pinned Vec allocation never mutated
        // or removed while Files is shared.
        unsafe { &*views }
    }
    fn insert(&self, file: File) -> u64 {
        let mut registry = self.owned.lock().unwrap();
        let id = next_id(&mut registry.next);
        registry
            .live
            .get_or_insert_with(|| Resources::new(close_files))
            .values
            .insert(id, file);
        id
    }
    fn file(&self, key: u64) -> File {
        self.owned.lock().unwrap().live.as_ref().unwrap().values[&key]
    }
    fn take_file(&self, key: u64) -> File {
        self.owned
            .lock()
            .unwrap()
            .live
            .as_mut()
            .unwrap()
            .values
            .remove(&key)
            .unwrap()
    }
    pub(crate) fn grant(&self, provider: Provider, handle: u64) -> u64 {
        self.grant_with_terminal(provider, handle, false)
    }
    pub(crate) fn grant_with_terminal(
        &self,
        provider: Provider,
        handle: u64,
        terminal: bool,
    ) -> u64 {
        let mut registry = self.run.designations.lock().unwrap();
        let id = DYNAMIC_GRANT | next_id(&mut registry.next);
        registry.grants.insert(
            id,
            Arc::new(DynamicGrant {
                provider,
                handle,
                terminal,
            }),
        );
        id
    }
    pub(crate) fn grant_terminal(&self, handle: u64) -> bool {
        self.run.designations.lock().unwrap().grants[&handle].terminal
    }
    pub(crate) fn grant_provider(&self, handle: u64) -> *const Provider {
        let file = Arc::as_ptr(&self.run.designations.lock().unwrap().grants[&handle]);
        // SAFETY: grants stay pinned until teardown.
        unsafe { &raw const (*file).provider }
    }
    pub(crate) fn grant_handle(&self, handle: u64) -> u64 {
        self.run.designations.lock().unwrap().grants[&handle].handle
    }
    /// A virtual provider's live resource: a ClockHand or StopTrigger
    /// (CLOSE_CELL, which closes nothing but itself), a `Served` end
    /// (CLOSE_SERVED) or a `Call` (CLOSE_CALL, `handle` its call id).
    pub(crate) fn held(&self, provider: Provider, handle: u64, close_op: u8) -> u64 {
        self.insert(File {
            provider,
            handle,
            mode: 0,
            close_op,
        })
    }
    pub(crate) fn file_handle(&self, key: u64) -> u64 {
        self.file(key).handle
    }
    pub(crate) fn file_provider(&self, key: u64) -> Provider {
        self.file(key).provider
    }
    /// Consume a live StopTrigger or Call without closing it: its provider
    /// and handle.
    pub(crate) fn take_held(&self, key: u64) -> (Provider, u64) {
        let file = self.take_file(key);
        (file.provider, file.handle)
    }
}
impl Default for Files {
    fn default() -> Self {
        Self::with_registry(Registry::default())
    }
}

impl Files {
    pub(crate) fn with_registry(registry: Registry) -> Self {
        Self {
            sockets: Default::default(),
            connected: Default::default(),
            tls: Default::default(),
            tls_writes: Default::default(),
            #[cfg(feature = "record")]
            net_tape: None,
            #[cfg(feature = "record")]
            secret_mac_tape: None,
            #[cfg(feature = "record")]
            managed_control: (0, 0),
            registry,
        }
    }

    pub(crate) unsafe fn close_all(&mut self, context: *mut Context) {
        self.sockets = hirsel_core_net::Sockets::default();
        self.tls.sessions.clear();
        self.connected.clear();
        if let Some(files) = &mut self.registry.owned.get_mut().unwrap().live {
            // SAFETY: the run's Context and providers remain pinned.
            unsafe { files.close(context) };
        }
        if let Some(run) = Arc::get_mut(&mut self.registry.run) {
            run.net_grants.get_mut().unwrap().clear();
            run.env_grants.get_mut().unwrap().clear();
            *run.secrets.get_mut().unwrap() = crate::secret::Secrets::default();
            crate::provide::forget(&mut run.cells);
        }
    }
}

unsafe fn close_files(files: &mut BTreeMap<u64, File>, context: *mut Context) {
    while let Some((_, file)) = files.pop_last() {
        // SAFETY: removed sole ownership before calling the pinned provider.
        unsafe { close(context, file) };
    }
}

fn close_request(file: File, site: u64) -> Request {
    Request {
        operation: file.close_op,
        mode: 0,
        handle: file.handle,
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
        if file.close_op == crate::scram::CLOSE_SCRAM {
            crate::scram::drop_state(file.handle);
            return;
        }
        (*context).cancel_shield += 1;
        let _ = drive(context, &file.provider, &mut close_request(file, 0), None);
        (*context).cancel_shield -= 1;
    }
}

/// Open a mechanical file or listing through the selected provider.
/// No logical admission; registry storage is provider bookkeeping (MEM-24).
/// # Safety
/// Live run context, provider and OPEN or ENTRIES request; callbacks obey CAP-8 and retain
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
        let mode = (*request).mode;
        // A port's file or listing is a handle its server issued: dropping it
        // is a RELEASE the server learns of, never a Close it must answer.
        let close_op = if crate::provide::owns(&*provider) {
            RELEASE
        } else if (*request).operation == ENTRIES {
            CLOSE_ENTRIES
        } else {
            CLOSE
        };
        let mut result = drive(context, provider, request, Some(&mut issued));
        let Some(issued) = issued else {
            return result;
        };
        let provider = *provider;
        if result.status == 0 && result.error == IoError::OK {
            result.value = (*(*context).host_files).registry.insert(File {
                provider,
                handle: result.value,
                mode,
                close_op,
            });
        } else if issued.status == 0 && issued.error == IoError::OK {
            // Delivery failed after issue opened a resource. Close the unpublished key.
            close(
                context,
                File {
                    provider,
                    handle: issued.value,
                    mode,
                    close_op,
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
        if matches!((*request).operation, READ_AT | WRITE_AT) {
            (*request).mode = owned.mode;
        }
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

/// Drop a file or listing, bypassing cancellation and ignoring close errors.
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

/// The argument vector for a live run or its recording provider.
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

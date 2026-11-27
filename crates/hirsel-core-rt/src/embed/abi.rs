//! C embed ABI v1 exports (`hirsel_embed_v1_*`, EMB-4).
//!
//! Matches [`hirsel_core_ids::embed::HEADER_V1`] verbatim, with strict
//! phase-ordered validation (guard-page safe `size` prefix read, zero tail
//! check, `manifest_fp` verification, immediate `*out` zeroing) before touching
//! the registry or invoking any callback.
use super::{
    account::{AccountError, ClientAccount, SESSION_BASE_BYTES, try_copy_slice},
    client::ClientEngine,
    codec::SessionEnd,
    current_program,
    delivery::{Delivery, EndedFn, RuntimeEndedCb, SessionCbs},
    dial::Transport,
    launch::LaunchedPlace,
    registry::{AdmissionBarrier, Generation, StopDecision, registry},
};
use hirsel_core_ids::embed::{ABI_VERSION, parse_manifest};
use std::{
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::{Arc, Condvar, Mutex},
};

pub type HirselStatus = i32;

pub const HIRSEL_OK: HirselStatus = 0;
pub const HIRSEL_ABI_MISMATCH: HirselStatus = 1;
pub const HIRSEL_REFUSED: HirselStatus = 2;
pub const HIRSEL_STOPPED: HirselStatus = 3;
pub const HIRSEL_UNKNOWN_ENTRY: HirselStatus = 4;
pub const HIRSEL_BUSY: HirselStatus = 5;
pub const HIRSEL_ALREADY_RUNNING: HirselStatus = 6;
pub const HIRSEL_ID_IN_USE: HirselStatus = 7;
pub const HIRSEL_DEFERRED: HirselStatus = 8;
pub const HIRSEL_NOT_LIVE: HirselStatus = 9;
pub const HIRSEL_FULL: HirselStatus = 10;
pub const HIRSEL_HOST_MEMORY: HirselStatus = 11;
pub const HIRSEL_OVER_BOUND: HirselStatus = 12;

pub const HIRSEL_END_STOPPED: u32 = 0;
pub const HIRSEL_END_REFUSED: u32 = 1;
pub const HIRSEL_END_PLACE_FAILED: u32 = 2;
pub const HIRSEL_END_RUN_STOP: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HirselRuntime {
    pub generation: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HirselSession {
    pub generation: u64,
    pub session: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HirselBytes {
    pub ptr: *const u8,
    pub len: usize,
}

impl Default for HirselBytes {
    fn default() -> Self {
        Self {
            ptr: ptr::null(),
            len: 0,
        }
    }
}

impl HirselBytes {
    pub fn from_slice(s: &[u8]) -> Self {
        Self {
            ptr: if s.is_empty() {
                ptr::null()
            } else {
                s.as_ptr()
            },
            len: s.len(),
        }
    }

    /// Validate `ptr` / `len` (`ptr` may be NULL only if `len == 0`).
    ///
    /// # Safety
    /// If `len > 0`, `ptr` must point to `len` readable bytes.
    pub unsafe fn as_slice<'a>(self) -> Result<&'a [u8], HirselStatus> {
        if self.len == 0 {
            return Ok(&[]);
        }
        if self.ptr.is_null() {
            return Err(HIRSEL_ABI_MISMATCH);
        }
        // SAFETY: caller guarantees `ptr` is readable for `len` bytes when non-null.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr, self.len) })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HirselEmbedCallbacks {
    pub size: u32,
    pub context: *mut c_void,
    pub outcome: Option<unsafe extern "C" fn(*mut c_void, u64, HirselBytes)>,
    pub change: Option<unsafe extern "C" fn(*mut c_void, u64, HirselBytes)>,
    pub sub_ended: Option<unsafe extern "C" fn(*mut c_void, u64, HirselBytes)>,
    pub room_event: Option<unsafe extern "C" fn(*mut c_void, u64, HirselBytes)>,
    pub session_ended: Option<unsafe extern "C" fn(*mut c_void, HirselBytes)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HirselEmbedConfig {
    pub size: u32,
    pub abi: u32,
    pub manifest_fp: HirselBytes,
    pub digest: HirselBytes,
    pub n_place: u64,
    pub client_bytes: u64,
    pub start_timeout_ms: u32,
    pub grants: HirselBytes,
    pub dir_bases: HirselBytes,
    pub store_dir: HirselBytes,
    pub context: *mut c_void,
    pub ended: Option<EndedFn>,
}

/// Guard-page safe read of a `size`-prefixed C struct:
/// 1. Rejects NULL.
/// 2. Reads ONLY the leading `uint32_t size` (4 bytes) first.
/// 3. If `size < size_of::<T>()`, returns `Err(HIRSEL_ABI_MISMATCH)` without
///    reading a single byte past `size`.
/// 4. If `size > size_of::<T>()`, verifies every byte in `size_of::<T>()..size` is 0.
/// 5. Copies `T` out.
unsafe fn read_sized_struct<T: Copy>(ptr: *const T) -> Result<T, HirselStatus> {
    if ptr.is_null() {
        return Err(HIRSEL_ABI_MISMATCH);
    }
    // SAFETY: caller's struct begins with `uint32_t size`.
    let size = unsafe { ptr.cast::<u32>().read() } as usize;
    let min_size = size_of::<T>();
    if size < min_size {
        return Err(HIRSEL_ABI_MISMATCH);
    }
    if size > min_size {
        // SAFETY: caller states `size` bytes are readable.
        let tail =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u8>().add(min_size), size - min_size) };
        if tail.iter().any(|&b| b != 0) {
            return Err(HIRSEL_ABI_MISMATCH);
        }
    }
    // SAFETY: `size >= size_of::<T>()`, so reading `T` stays within `0..size`.
    Ok(unsafe { ptr.read() })
}

// ---------------------------------------------------------------------------
// 14 C ABI v1 exports (matching HEADER_V1 & EXPORTS_V1)
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_abi() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_manifest() -> HirselBytes {
    match current_program() {
        Some(prog) => HirselBytes::from_slice(prog.manifest),
        None => HirselBytes::default(),
    }
}

/// # Safety
/// `config` and `out` follow the `hirsel_embed.h` ABI v1 contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_start(
    config: *const HirselEmbedConfig,
    out: *mut HirselRuntime,
) -> HirselStatus {
    // SAFETY: forwards the C ABI's caller requirements.
    unsafe { start(config, out, current_program(), None) }
}

/// # Safety
/// `config` and `out` follow the C ABI. The remaining arguments refer to
/// immutable tables and code emitted together in one native Hirsel archive.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_archive_start(
    config: *const HirselEmbedConfig,
    out: *mut HirselRuntime,
    manifest: *const u8,
    manifest_len: usize,
    target: *const u8,
    target_len: usize,
    root_limit: u64,
    parameters: *const crate::entry::Requirement,
    parameters_len: usize,
    names: *const crate::process::View,
    revision: *const [u8; 32],
    place: u64,
    foreign_waits: u64,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
) -> HirselStatus {
    // SAFETY: the emitter supplies these immutable static tables.
    unsafe {
        let program = super::EmbedProgram {
            manifest: std::slice::from_raw_parts(manifest, manifest_len),
            target: std::str::from_utf8_unchecked(std::slice::from_raw_parts(target, target_len)),
            root_limit,
            parameters: std::slice::from_raw_parts(parameters, parameters_len),
            parameter_names: &[],
            revision: Some(*revision),
            place: place != 0,
            foreign_waits: foreign_waits != 0,
            sources,
            entry,
            probe_dir: None,
        };
        let names = std::slice::from_raw_parts(names, parameters_len);
        start(config, out, Some(&program), Some(names))
    }
}

unsafe fn start(
    config: *const HirselEmbedConfig,
    out: *mut HirselRuntime,
    program: Option<&super::EmbedProgram>,
    names: Option<&[crate::process::View]>,
) -> HirselStatus {
    // Phase 1: Output pointer check and immediate zeroing.
    if out.is_null() {
        return HIRSEL_ABI_MISMATCH;
    }
    // SAFETY: `out` is non-null and writable per ABI contract.
    unsafe {
        *out = HirselRuntime { generation: 0 };
    }

    // Phase 2: ABI v1 validation BEFORE touching the registry.
    let Some(program) = program else {
        return HIRSEL_ABI_MISMATCH;
    };
    let parsed_manifest = match parse_manifest(program.manifest) {
        Ok(m) => Arc::new(m),
        Err(_) => return HIRSEL_ABI_MISMATCH,
    };

    // SAFETY: `read_sized_struct` never reads past `config.size`.
    let cfg = match unsafe { read_sized_struct(config) } {
        Ok(c) => c,
        Err(status) => return status,
    };
    if cfg.abi != ABI_VERSION || cfg.start_timeout_ms == 0 {
        return HIRSEL_ABI_MISMATCH;
    }
    let Some(ended_fn) = cfg.ended else {
        return HIRSEL_ABI_MISMATCH;
    };
    // SAFETY: validated `HirselBytes` borrow host memory for this call.
    let (fp_slice, digest_slice, grants_slice, dir_bases_slice, store_dir_slice) = unsafe {
        let Ok(fp) = cfg.manifest_fp.as_slice() else {
            return HIRSEL_ABI_MISMATCH;
        };
        let Ok(dig) = cfg.digest.as_slice() else {
            return HIRSEL_ABI_MISMATCH;
        };
        let Ok(gr) = cfg.grants.as_slice() else {
            return HIRSEL_ABI_MISMATCH;
        };
        let Ok(db) = cfg.dir_bases.as_slice() else {
            return HIRSEL_ABI_MISMATCH;
        };
        let Ok(sd) = cfg.store_dir.as_slice() else {
            return HIRSEL_ABI_MISMATCH;
        };
        (fp, dig, gr, db, sd)
    };
    if fp_slice.len() != 32 || fp_slice != parsed_manifest.fingerprint.as_slice() {
        return HIRSEL_ABI_MISMATCH;
    }
    let digest = match digest_slice.len() {
        0 => None,
        32 => Some(<[u8; 32]>::try_from(digest_slice).unwrap()),
        _ => return HIRSEL_ABI_MISMATCH,
    };

    // Copy config buffers before taking the registry lock (fallible allocation).
    let grants_owned = match try_copy_slice(grants_slice) {
        Ok(v) => v,
        Err(_) => return HIRSEL_HOST_MEMORY,
    };
    let dir_bases_owned = match try_copy_slice(dir_bases_slice) {
        Ok(v) => v,
        Err(_) => return HIRSEL_HOST_MEMORY,
    };
    let store_dir_owned = match try_copy_slice(store_dir_slice) {
        Ok(v) => v,
        Err(_) => return HIRSEL_HOST_MEMORY,
    };

    let mut linked_names = Vec::new();
    let parameter_names = match names {
        Some(names) => {
            if linked_names.try_reserve_exact(names.len()).is_err() {
                return HIRSEL_HOST_MEMORY;
            }
            for name in names {
                // SAFETY: the linked table holds immutable UTF-8 views.
                let text = unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(name.data, name.len))
                };
                linked_names.push(text);
            }
            &linked_names
        }
        None => program.parameter_names,
    };

    // Phase 3: Singleton reservation (`Stopped -> Starting`, monotonic generation).
    let Some(gen_id) = registry().reserve_start() else {
        return HIRSEL_ALREADY_RUNNING;
    };

    let runtime_cb = RuntimeEndedCb {
        context: cfg.context,
        ended: ended_fn,
    };

    // Phase 4: Semantic validation and launch.
    let launch_result = (|| -> Result<Arc<Generation>, String> {
        let account = Arc::new(ClientAccount::new(cfg.client_bytes)?);
        let (place, dialer) = LaunchedPlace::start(
            program,
            parameter_names,
            digest,
            cfg.n_place,
            cfg.start_timeout_ms,
            &grants_owned,
            &dir_bases_owned,
            &store_dir_owned,
        )?;
        let engine_slot: Arc<Mutex<Option<Arc<ClientEngine>>>> = Arc::new(Mutex::new(None));
        let acc_refund = account.clone();
        let eng_resume = engine_slot.clone();
        let refund_fn = Arc::new(move |bytes: u64| {
            acc_refund.refund(bytes);
            if let Some(eng) = eng_resume.lock().unwrap().as_ref() {
                eng.resume_paused_reads();
            }
        });
        let delivery = match Delivery::spawn(runtime_cb, refund_fn) {
            Ok(d) => d,
            Err(e) => {
                place.force_stop_and_join();
                return Err(format!("DeliveryThread({e})"));
            }
        };
        let engine = ClientEngine::new(parsed_manifest, account.clone(), delivery.clone());
        *engine_slot.lock().unwrap() = Some(engine.clone());
        let eng_for_io = engine.clone();
        let transport = match Transport::spawn(
            dialer,
            Arc::new(move |ev| eng_for_io.on_transport_event(ev)),
        ) {
            Ok(t) => t,
            Err(e) => {
                delivery.shutdown_and_join();
                place.force_stop_and_join();
                return Err(format!("TransportThread({e})"));
            }
        };
        engine.attach_transport(transport.clone());
        place.set_spontaneous_end_hook(Box::new(move |code, reason| {
            registry().on_spontaneous_end(gen_id, code, reason);
        }));
        Ok(Arc::new(Generation {
            id: gen_id,
            barrier: AdmissionBarrier::new(),
            account,
            delivery,
            engine,
            transport,
            place,
            spontaneous_done: Arc::new((Mutex::new(false), Condvar::new())),
        }))
    })();

    match launch_result {
        Ok(gen_rec) => {
            registry().publish_running(gen_rec);
            // SAFETY: `out` is non-null and writable.
            unsafe {
                *out = HirselRuntime { generation: gen_id };
            }
            HIRSEL_OK
        }
        Err(reason) => {
            // Deliver `config.ended(HIRSEL_END_REFUSED, reason)` synchronously while still `Starting`.
            let reason_bytes = reason.as_bytes();
            let view = HirselBytes::from_slice(reason_bytes);
            // SAFETY: host config callback and context are live through this synchronous call.
            unsafe {
                (runtime_cb.ended)(runtime_cb.context, HIRSEL_END_REFUSED, view);
            }
            registry().abort_start(gen_id);
            HIRSEL_REFUSED
        }
    }
}

/// # Safety
/// `callbacks`, `credential`, and `out` follow `hirsel_embed.h`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_open(
    runtime: HirselRuntime,
    callbacks: *const HirselEmbedCallbacks,
    credential: HirselBytes,
    out: *mut HirselSession,
) -> HirselStatus {
    if out.is_null() {
        return HIRSEL_ABI_MISMATCH;
    }
    // SAFETY: `out` is non-null and writable.
    unsafe {
        *out = HirselSession {
            generation: 0,
            session: 0,
        };
    }
    // Validate ABI v1 arguments BEFORE touching the registry.
    // SAFETY: guard-page safe sized read.
    let raw_cbs = match unsafe { read_sized_struct(callbacks) } {
        Ok(c) => c,
        Err(status) => return status,
    };
    let Some(cbs) = SessionCbs::from_raw(&raw_cbs) else {
        return HIRSEL_ABI_MISMATCH;
    };
    // SAFETY: `credential` borrows host bytes for this call.
    let cred_slice = match unsafe { credential.as_slice() } {
        Ok(s) => s,
        Err(status) => return status,
    };

    // Admit runtime handle under registry lock.
    let Some((gen_rec, _gen_guard)) = registry().admit_runtime(runtime.generation) else {
        return HIRSEL_STOPPED;
    };

    #[cfg(test)]
    super::registry::test_hooks::maybe_pause_admitted();

    // Reserve session record + credential copy + terminal reserve from admissible pool.
    let (cred_owned, charge) = match gen_rec
        .account
        .try_copy_charged(cred_slice, SESSION_BASE_BYTES)
    {
        Ok((v, c)) => (if v.is_empty() { None } else { Some(v) }, c),
        Err(AccountError::Full) => return HIRSEL_FULL,
        Err(AccountError::HostMemory) => return HIRSEL_HOST_MEMORY,
    };

    let Some(sid) = registry().register_session(runtime.generation) else {
        gen_rec.account.refund(charge);
        return HIRSEL_STOPPED;
    };

    gen_rec.delivery.register_session(sid, cbs);
    gen_rec.engine.open_session(sid, cred_owned, charge);

    // SAFETY: `out` is non-null and writable.
    unsafe {
        *out = HirselSession {
            generation: runtime.generation,
            session: sid,
        };
    }
    HIRSEL_OK
}

/// # Safety
/// `args` follows `hirsel_bytes` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_call(
    session: HirselSession,
    call: u64,
    want: u32,
    args: HirselBytes,
    deadline_ms: u32,
) -> HirselStatus {
    // SAFETY: `args` borrows host bytes for this call.
    let args_slice = match unsafe { args.as_slice() } {
        Ok(s) => s,
        Err(status) => return status,
    };
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    #[cfg(test)]
    super::registry::test_hooks::maybe_pause_admitted();
    gen_rec
        .engine
        .call(session.session, call, want, args_slice, deadline_ms)
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_abandon(session: HirselSession, call: u64) -> HirselStatus {
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec.engine.abandon(session.session, call)
}

/// # Safety
/// `args` follows `hirsel_bytes` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_subscribe(
    session: HirselSession,
    sub: u64,
    want: u32,
    args: HirselBytes,
) -> HirselStatus {
    // SAFETY: `args` borrows host bytes for this call.
    let args_slice = match unsafe { args.as_slice() } {
        Ok(s) => s,
        Err(status) => return status,
    };
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec
        .engine
        .subscribe(session.session, sub, want, args_slice, false)
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_unsubscribe(session: HirselSession, sub: u64) -> HirselStatus {
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec.engine.unsubscribe(session.session, sub, false)
}

/// # Safety
/// `args` follows `hirsel_bytes` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_room_join(
    session: HirselSession,
    room: u64,
    want: u32,
    args: HirselBytes,
) -> HirselStatus {
    // SAFETY: `args` borrows host bytes for this call.
    let args_slice = match unsafe { args.as_slice() } {
        Ok(s) => s,
        Err(status) => return status,
    };
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec
        .engine
        .subscribe(session.session, room, want, args_slice, true)
}

/// # Safety
/// `state` follows `hirsel_bytes` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_room_set(
    session: HirselSession,
    room: u64,
    present: u32,
    state: HirselBytes,
) -> HirselStatus {
    // SAFETY: `state` borrows host bytes for this call.
    let state_slice = match unsafe { state.as_slice() } {
        Ok(s) => s,
        Err(status) => return status,
    };
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec
        .engine
        .room_set(session.session, room, present != 0, state_slice)
}

/// # Safety
/// `message` follows `hirsel_bytes` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_v1_room_send(
    session: HirselSession,
    room: u64,
    to: u64,
    message: HirselBytes,
) -> HirselStatus {
    // SAFETY: `message` borrows host bytes for this call.
    let msg_slice = match unsafe { message.as_slice() } {
        Ok(s) => s,
        Err(status) => return status,
    };
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec
        .engine
        .room_send(session.session, room, to, msg_slice)
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_room_leave(session: HirselSession, room: u64) -> HirselStatus {
    let Some((gen_rec, _gg, _sg)) = registry().admit_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };
    gen_rec.engine.unsubscribe(session.session, room, true)
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_close(session: HirselSession) -> HirselStatus {
    let Some((gen_rec, sess_barrier)) =
        registry().begin_close_session(session.generation, session.session)
    else {
        return HIRSEL_STOPPED;
    };

    let sid = session.session;
    let gen_id = session.generation;
    if gen_rec.delivery.is_delivery_thread() {
        let engine = gen_rec.engine.clone();
        gen_rec.delivery.defer_close_from_delivery(
            sid,
            Box::new(move || {
                // Drain admitted session producers after the current callback returned.
                sess_barrier.close_and_drain();
                engine.close_session(sid);
                let end_bytes = SessionEnd::Closed.encode_bytes();
                let on_done: Box<dyn FnOnce() + Send> = Box::new(move || {
                    registry().remove_session(gen_id, sid);
                });
                (end_bytes, on_done)
            }),
        );
        return HIRSEL_DEFERRED;
    }

    // Host thread close: wait for admitted session producers, close protocol session,
    // wait for active callback + deliver session_ended(Closed), then release.
    sess_barrier.close_and_drain();
    gen_rec.engine.close_session(sid);
    gen_rec.delivery.close_session_sync(
        sid,
        SessionEnd::Closed.encode_bytes(),
        Box::new(move || {
            registry().remove_session(gen_id, sid);
        }),
    );
    HIRSEL_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn hirsel_embed_v1_stop(runtime: HirselRuntime) -> HirselStatus {
    match registry().stop(runtime.generation) {
        StopDecision::Stopped => HIRSEL_STOPPED,
        StopDecision::Busy => HIRSEL_BUSY,
        StopDecision::RunTeardown {
            active,
            session_barriers,
            was_ended,
        } => {
            if was_ended {
                // Wait for spontaneous teardown to finish delivering callbacks.
                let (lock, cv) = &*active.spontaneous_done;
                let mut done = lock.lock().unwrap();
                while !*done {
                    done = cv.wait(done).unwrap();
                }
                active.transport.shutdown_and_join();
                active.delivery.shutdown_and_join();
                active.place.join_only();
            } else {
                active.barrier.close_and_drain();
                for b in session_barriers {
                    b.close_and_drain();
                }
                active.transport.shutdown_and_join();
                let engine = active.engine.clone();
                let gen_id = active.id;
                active.delivery.finish_generation_sync(
                    SessionEnd::RuntimeStopped.encode_bytes(),
                    HIRSEL_END_STOPPED,
                    Vec::new(),
                    move |sid| {
                        registry().remove_session(gen_id, sid);
                        engine.close_session(sid);
                    },
                );
                active.engine.release_all_sessions();
                active.delivery.shutdown_and_join();
                active.place.stop_gracefully_and_join();
            }
            registry().finish_stop(runtime.generation);
            HIRSEL_OK
        }
    }
}

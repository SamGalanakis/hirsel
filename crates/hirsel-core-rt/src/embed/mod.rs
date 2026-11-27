//! Embedded runtime slot (EMB-3 & EMB-4).
//!
//! Compiled only when `feature = "standalone"` is NOT enabled (the embed slot).
//! Provides the in-process place launch, C embed ABI v1 exports, host-client
//! byte accounting, 3145 leased-stream lane kernel, and ordered callback delivery.
pub mod abi;
pub mod account;
pub mod client;
pub mod codec;
mod codec_embed;
pub mod delivery;
pub mod dial;
pub mod grants;
mod image;
pub mod kernel;
mod kernel_step;
mod lanes;
pub mod launch;
pub mod registry;
mod room;
pub mod ws;

#[cfg(test)]
mod tests;

use crate::{abi_sites::SourceTable, entry::Requirement, process::Entry};
use std::sync::RwLock;

/// Descriptor of the compiled Hirsel program linked into this embedded library.
///
/// Slice 3's embed archive export member registers this descriptor via a link-time
/// registration hook or `register_program`; Slice 2's test harnesses register their
/// compiled fixture program directly.
pub struct EmbedProgram {
    pub manifest: &'static [u8],
    pub target: &'static str,
    pub root_limit: u64,
    pub parameters: &'static [Requirement],
    pub parameter_names: &'static [&'static str],
    pub revision: Option<[u8; 32]>,
    pub place: bool,
    pub foreign_waits: bool,
    pub sources: *const SourceTable,
    pub entry: Entry,
    /// Optional directory probe override (`None` runs `OsProvider::probe_directory`).
    pub probe_dir: Option<fn(&crate::os::OsProvider, u64) -> std::io::Result<()>>,
}

// SAFETY: `EmbedProgram` holds immutable static program tables and function pointer.
unsafe impl Send for EmbedProgram {}
unsafe impl Sync for EmbedProgram {}

static PROGRAM: RwLock<Option<&'static EmbedProgram>> = RwLock::new(None);

/// Register the compiled program served by `hirsel_embed_v1_*`.
pub fn register_program(program: &'static EmbedProgram) {
    *PROGRAM.write().unwrap() = Some(program);
}

/// Clear the registered program (tests only).
#[cfg(test)]
pub(crate) fn clear_program() {
    *PROGRAM.write().unwrap() = None;
}

/// Look up the currently registered embedded program.
pub(crate) fn current_program() -> Option<&'static EmbedProgram> {
    *PROGRAM.read().unwrap()
}

/// Called by `units_lang.rs::hirsel_core_conn_new` to capture the embedded place's
/// `Dialer` client handle when armed by `runner.operate_with_report`.
pub(crate) fn maybe_capture_dialer(client: &crate::units::ledger::conn::Client) {
    launch::maybe_capture_dialer(client);
}

/// Register a compiled Hirsel program from its generated `OsConfig` link tables
/// plus native-Describe manifest JSON bytes (Slice 2 real-place link harness).
///
/// # Safety
/// `input`, `parameter_names`, `sources`, and `manifest` must point to valid
/// memory with the declared extents.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_harness_register(
    input: *const crate::entry::OsConfig,
    parameter_names: *const crate::process::View,
    sources: *const SourceTable,
    entry: Entry,
    manifest_ptr: *const u8,
    manifest_len: usize,
    foreign_waits: u8,
) -> i32 {
    if input.is_null() || manifest_ptr.is_null() || manifest_len == 0 {
        return -1;
    }
    // SAFETY: caller provides live compiler-emitted OsConfig, View tables, and manifest bytes.
    let (target, root_limit, params, param_names, revision, place, manifest) = unsafe {
        let cfg = &*input;
        let target_bytes = if cfg.target.len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(cfg.target.data, cfg.target.len)
        };
        let Ok(target_str) = std::str::from_utf8(target_bytes) else {
            return -2;
        };
        let target: &'static str = Box::leak(target_str.to_owned().into_boxed_str());
        let raw_params = if cfg.parameters_len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(cfg.parameters, cfg.parameters_len)
        };
        let params_vec: Vec<Requirement> = raw_params
            .iter()
            .map(|r| Requirement {
                kind: r.kind,
                index: r.index,
            })
            .collect();
        let params: &'static [Requirement] = Box::leak(params_vec.into_boxed_slice());

        let mut names_vec: Vec<&'static str> = Vec::with_capacity(cfg.parameters_len);
        if !parameter_names.is_null() {
            for i in 0..cfg.parameters_len {
                let v = &*parameter_names.add(i);
                let b = if v.len == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(v.data, v.len)
                };
                let s = std::str::from_utf8(b).unwrap_or("");
                names_vec.push(Box::leak(s.to_owned().into_boxed_str()));
            }
        }
        let param_names: &'static [&'static str] = Box::leak(names_vec.into_boxed_slice());
        let revision = if cfg.revision.is_null() {
            None
        } else {
            Some(*cfg.revision)
        };
        let manifest_vec = std::slice::from_raw_parts(manifest_ptr, manifest_len).to_vec();
        let manifest: &'static [u8] = Box::leak(manifest_vec.into_boxed_slice());
        (
            target,
            cfg.root_limit,
            params,
            param_names,
            revision,
            cfg.place != 0,
            manifest,
        )
    };

    let program: &'static EmbedProgram = Box::leak(Box::new(EmbedProgram {
        manifest,
        target,
        root_limit,
        parameters: params,
        parameter_names: param_names,
        revision,
        place,
        foreign_waits: foreign_waits != 0,
        sources,
        entry,
        probe_dir: None,
    }));
    register_program(program);
    0
}

/// Compute EMB-5 `manifest_fingerprint` for `manifest` into `out_32` (test harness helper).
///
/// # Safety
/// `manifest_ptr` must be readable for `manifest_len`; `out_32` must be writable for 32 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_harness_manifest_fp(
    manifest_ptr: *const u8,
    manifest_len: usize,
    out_32: *mut u8,
) -> i32 {
    if manifest_ptr.is_null() || out_32.is_null() {
        return -1;
    }
    // SAFETY: caller provides valid readable and writable buffers.
    let slice = unsafe { std::slice::from_raw_parts(manifest_ptr, manifest_len) };
    let Ok(fp) = hirsel_core_ids::embed::manifest_fingerprint(slice) else {
        return -2;
    };
    // SAFETY: `out_32` has 32 writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(fp.as_ptr(), out_32, 32);
    }
    0
}

/// Read the `Welcome.target` observed by the active embedded generation (m2 law check).
///
/// # Safety
/// `out` must point to `cap` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_embed_harness_welcome_target(
    gen_id: u64,
    out: *mut u8,
    cap: usize,
) -> isize {
    let Some((gen_rec, _guard)) = registry::registry().admit_runtime(gen_id) else {
        return -1;
    };
    let Some(target) = gen_rec.engine.last_welcome_target() else {
        return 0;
    };
    let bytes = target.as_bytes();
    if bytes.len() > cap || out.is_null() {
        return -2;
    }
    // SAFETY: caller provides `cap` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
    }
    bytes.len() as isize
}

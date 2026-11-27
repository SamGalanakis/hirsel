//! The pack host's join of the runtime and the loader (PACK-9).
//!
//! The runtime does not link the loader and the loader does not link the runtime. A
//! pack host links both through this crate: it makes the process's one loader with the
//! runtime's real row addresses, gives the runtime the loader's text band and PC
//! lookup, and reports a fault in loaded code after the pool has stopped.

use std::sync::Mutex;

use hirsel_code::{CauseDataHandle, CauseDataOps, Config, Loader, Refusal};
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::{
    causes::{CauseData, CauseDataRef},
    code,
    run_stop::Execution,
};
use hirsel_mod::Mask;

pub mod rows;
pub mod runtime;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

fn create_cause_data(literals: &[u8], sites: &[u8]) -> Option<CauseDataHandle> {
    let data = CauseData::new(literals, sites)?;
    let slot_addr = data.slot_addr();
    let (ro_ptr, ro_len) = data.literals();
    let owner = CauseDataRef::into_raw(data).addr();
    Some(CauseDataHandle {
        owner,
        slot_addr,
        ro_literals_base: ro_ptr.addr(),
        ro_literals_len: ro_len,
    })
}

unsafe fn retain_cause_data(owner: usize) {
    // SAFETY: `hirsel-code` calls this under hazard protection while the owner is live.
    unsafe { CauseData::retain(std::ptr::with_exposed_provenance(owner)) };
}

unsafe fn release_cause_data(owner: usize) {
    // SAFETY: `hirsel-code` transfers the image's owned count after hazard drain.
    unsafe { CauseData::release(std::ptr::with_exposed_provenance(owner)) };
}

/// A loader configuration for this host image: this build's runtime ABI, its row
/// addresses, no prelinked images, independent R/NX cause data owners (PACK-9.7),
/// and the default batch and retired budget. `policy` is the pool's policy bits
/// (PACK-2): a module loads only when its own are equal.
pub fn config(policy: u64, mask: Mask, band_bytes: usize, installed_bytes: u64) -> Config {
    Config {
        abi: hirsel_mod::abi(),
        policy,
        mask,
        band_bytes,
        installed_bytes,
        retired_bytes: Config::RETIRED_BYTES,
        batch: Config::BATCH,
        row: Box::new(rows::address),
        instance: Box::new(|_| None),
        prelinked: &[],
        cause_data: Some(CauseDataOps {
            create: create_cause_data,
            retain: retain_cause_data,
            release: release_cause_data,
        }),
    }
}

/// Why [`start`] gave no loader.
#[derive(Debug)]
pub enum Refused {
    /// This process already has its loader.
    Started,
    Loader(Refusal),
    Runtime(code::Refused),
}

static LOADER: Mutex<Option<&'static Loader>> = Mutex::new(None);

/// Register `loader` with the PACK-14 runtime pool table (if not already registered)
/// and return its host-issued `Loader` capability handle for Hirsel code.
pub fn loader_handle(loader: &'static Loader) -> u64 {
    runtime::install_ops();
    runtime::register_pool(loader)
}

/// Make this process's loader and register its text and data bands with the runtime,
/// so that the fault handler, profile sampler, and cause recorder resolve addresses
/// through it. The loader lives as long as the process.
pub fn start(config: Config) -> Result<&'static Loader, Refused> {
    let mut slot = LOADER.lock().unwrap();
    if slot.is_some() {
        return Err(Refused::Started);
    }
    let loader: &'static Loader =
        Box::leak(Box::new(Loader::new(config).map_err(Refused::Loader)?));
    let (low, high) = loader.text_band();
    let (data_low, data_high) = loader.data_band();
    code::register(Box::leak(Box::new(code::Band {
        low,
        high,
        data_low,
        data_high,
        loader: std::ptr::from_ref(loader).cast(),
        resolve,
        resolve_literal,
    })))
    .map_err(Refused::Runtime)?;
    runtime::install_ops();
    let _ = runtime::register_pool(loader);
    *slot = Some(loader);
    Ok(loader)
}

/// The loader's PC lookup in the shape the runtime calls.
///
/// # Safety
/// `loader` is the process's loader.
unsafe fn resolve(loader: *const (), pc: usize) -> Option<code::Hit> {
    // SAFETY: the caller passes the loader `start` registered, which is never dropped.
    let hit = unsafe { &*loader.cast::<Loader>() }.resolve(pc)?;
    Some(code::Hit {
        digest: hit.digest,
        offset: hit.offset,
    })
}

/// The loader's lock-free data-band literal lookup in the shape the runtime calls.
///
/// # Safety
/// `loader` is the process's loader.
unsafe fn resolve_literal(loader: *const (), ptr: usize, len: usize) -> Option<code::LiteralOwner> {
    // SAFETY: the caller passes the loader `start` registered, which is never dropped.
    let (owner, relocated) = unsafe { &*loader.cast::<Loader>() }.resolve_literal(ptr, len)?;
    Some(code::LiteralOwner {
        owner: std::ptr::with_exposed_provenance(owner),
        relocated: std::ptr::with_exposed_provenance(relocated),
    })
}

/// A fault in loaded code, as the host reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Report {
    pub pc: usize,
    /// The first 16 bytes of the faulting module's digest and the fault's offset in its
    /// text. `None` when the PC was in the text of an image that had already died.
    pub hit: Option<([u8; 16], u32)>,
    /// The module digest this host now refuses to load. `None` when no image owned the
    /// PC.
    pub quarantined: Option<ContentHash>,
}

impl Report {
    /// The name of the span the fault is in, read from the faulting module's bytes.
    pub fn span<'a>(&self, module: &hirsel_mod::Module<'a>) -> Option<&'a str> {
        let (_, offset) = self.hit?;
        let index = module
            .spans()
            .position(|span| span.start <= offset && offset - span.start < span.length)?;
        Some(module.name(index))
    }
}

/// The fault in loaded code that stopped `execution` with `Code`, if one did. The image
/// that faulted is quarantined. Call it after the execution's pool has stopped.
pub fn report(execution: &Execution) -> Option<Report> {
    let fault = execution.code_fault()?;
    let loader = *LOADER.lock().unwrap();
    // By the image the handler recorded: the address may be another image's by now.
    let quarantined = loader.zip(fault.hit).and_then(|(loader, hit)| {
        loader.fault(hirsel_code::Hit {
            digest: hit.digest,
            offset: hit.offset,
        })
    });
    Some(Report {
        pc: fault.pc,
        hit: fault.hit.map(|hit| (hit.digest, hit.offset)),
        quarantined,
    })
}

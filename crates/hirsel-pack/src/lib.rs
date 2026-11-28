//! The pack host's join of the runtime and the loader (PACK-9).
//!
//! The runtime does not link the loader and the loader does not link the runtime. A
//! pack host links both through this crate: it makes each pool's loader with the
//! runtime's real row addresses, gives the runtime the loader's bands and lookups, and
//! reports a fault in loaded code after the pool has stopped.

use hirsel_code::{CauseDataHandle, CauseDataOps, Config, Loader, Refusal};
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::{
    causes::{CauseData, CauseDataRef},
    code,
    fiber::ArmError,
    run_stop::Execution,
};
use hirsel_mod::Mask;

mod place;
pub use place::{DispatchError, Place, PlaceError, PlaceSlot};
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
    Loader(Refusal),
    /// The fault handler could not be installed for the signals a trap in loaded code
    /// raises.
    Signals(ArmError),
}

/// The loader a band was registered with.
///
/// # Safety
/// `band` is one [`start`] registered. The runtime has no others: this crate is the
/// join, and a host that links it registers bands through `start` alone.
unsafe fn loader_of(band: &code::Band) -> &'static Loader {
    // SAFETY: `start` leaked the loader it registered.
    unsafe { &*band.loader.cast::<Loader>() }
}

/// The pool a `Loader` capability handle names.
fn pool(handle: u64) -> Option<&'static Loader> {
    code::pools()
        .find(|(issued, _)| *issued == handle)
        // SAFETY: a registered band is one of `start`'s.
        .map(|(_, band)| unsafe { loader_of(band) })
}

/// The host-issued `Loader` capability handle of `loader`'s pool for Hirsel code: the
/// same handle however often it is asked, and `None` for a loader [`start`] did not
/// make.
pub fn loader_handle(loader: &Loader) -> Option<u64> {
    code::pools()
        // SAFETY: a registered band is one of `start`'s.
        .find(|(_, band)| unsafe { loader_of(band) }.same_pool(loader))
        .map(|(handle, _)| handle)
}

/// Make a pool's loader and register its text and data bands with the runtime, so that
/// the fault handler, profile sampler, cause recorder and handle rows resolve
/// addresses through it. The loader lives as long as the process.
pub fn start(config: Config) -> Result<&'static Loader, Refused> {
    let loader: &'static Loader =
        Box::leak(Box::new(Loader::new(config).map_err(Refused::Loader)?));
    let (low, high) = loader.text_band();
    let (data_low, data_high) = loader.data_band();
    runtime::install_ops();
    code::register(Box::leak(Box::new(code::Band {
        low,
        high,
        data_low,
        data_high,
        loader: std::ptr::from_ref(loader).cast(),
        resolve,
        resolve_literal,
        pin: runtime::pin,
        bind: runtime::bind,
    })))
    .map_err(Refused::Signals)?;
    Ok(loader)
}

/// The loader's PC lookup in the shape the runtime calls.
///
/// # Safety
/// `loader` is the loader of the band this was registered with.
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
/// `loader` is the loader of the band this was registered with.
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
    // By the image the handler recorded: the address may be another image's by now.
    let quarantined = fault.hit.and_then(|hit| {
        let (_, band) =
            code::pools().find(|(_, band)| (band.low..band.high).contains(&fault.pc))?;
        // SAFETY: a registered band is one of `start`'s.
        unsafe { loader_of(band) }.fault(hirsel_code::Hit {
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

//! The pack host's join of the runtime and the loader (PACK-9).
//!
//! The runtime does not link the loader and the loader does not link the runtime. A
//! pack host links both through this crate: it makes the process's one loader with the
//! runtime's real row addresses, gives the runtime the loader's text band and PC
//! lookup, and reports a fault in loaded code after the pool has stopped.

use std::sync::Mutex;

use hirsel_code::{Config, Loader, Refusal};
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::{code, run_stop::Execution};
use hirsel_mod::Mask;

pub mod rows;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

/// A loader configuration for this host image: this build's runtime ABI and policy,
/// its row addresses, no prelinked images and the default batch and retired budget.
pub fn config(mask: Mask, band_bytes: usize, installed_bytes: u64) -> Config {
    Config {
        abi: hirsel_mod::abi(),
        policy: hirsel_mod::POLICY,
        mask,
        band_bytes,
        installed_bytes,
        retired_bytes: Config::RETIRED_BYTES,
        batch: Config::BATCH,
        row: Box::new(rows::address),
        instance: Box::new(|_| None),
        prelinked: &[],
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

/// Make this process's loader and register its text band with the runtime, so that the
/// fault handler and the profile sampler resolve code addresses through it. The loader
/// lives as long as the process.
pub fn start(config: Config) -> Result<&'static Loader, Refused> {
    let mut slot = LOADER.lock().unwrap();
    if slot.is_some() {
        return Err(Refused::Started);
    }
    let loader: &'static Loader =
        Box::leak(Box::new(Loader::new(config).map_err(Refused::Loader)?));
    let (low, high) = loader.text_band();
    code::register(Box::leak(Box::new(code::Band {
        low,
        high,
        loader: std::ptr::from_ref(loader).cast(),
        resolve,
    })))
    .map_err(Refused::Runtime)?;
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

//! Baseline-safe x86-64 ISA detection.
//!
//! Standalone executables call `hirsel_core_init_isa` at process start.
//! Embedded launches call [`init_embedded`], which reads no environment
//! variable and returns `Err("UnsupportedCpu")` instead of calling `exit`
//! (EMB-3).
#[cfg(target_arch = "x86_64")]
use hirsel_core_layout::isa::{X86_64_BASE, detect_x86_64};
#[cfg(target_arch = "x86_64")]
use std::sync::{
    Once,
    atomic::{AtomicU8, Ordering},
};

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub static hirsel_core_isa_level: AtomicU8 = AtomicU8::new(X86_64_BASE.number);

/// Standalone launcher entry: reads `HIRSEL_ISA_CAP` and exits with
/// `NOT_STARTED` if the host CPU is below baseline. Never called in the embed
/// slot.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_init_isa() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let cap = std::env::var("HIRSEL_ISA_CAP")
            .ok()
            .and_then(|text| text.parse::<u8>().ok())
            .unwrap_or(u8::MAX);
        let Some(level) = detect_x86_64(cap) else {
            eprintln!(
                "not started: this program requires an {} CPU",
                X86_64_BASE.name
            );
            std::process::exit(crate::process::NOT_STARTED.into());
        };
        hirsel_core_isa_level.store(level, Ordering::Relaxed);
    });
}

/// Embedded launch ISA check (EMB-3): reads no environment variable and
/// returns `Err("UnsupportedCpu")` to its caller instead of exiting.
#[cfg(target_arch = "x86_64")]
pub fn init_embedded() -> Result<u8, &'static str> {
    init_embedded_with(|| detect_x86_64(u8::MAX))
}

/// Embedded ISA check through an explicit detector seam (used by tests to
/// verify denied x86-64 CPU refusal without reading environment variables).
#[cfg(target_arch = "x86_64")]
pub fn init_embedded_with(detect: impl FnOnce() -> Option<u8>) -> Result<u8, &'static str> {
    let Some(level) = detect() else {
        return Err("UnsupportedCpu");
    };
    hirsel_core_isa_level.store(level, Ordering::Relaxed);
    Ok(level)
}

#[cfg(not(target_arch = "x86_64"))]
pub fn init_embedded() -> Result<u8, &'static str> {
    Ok(0)
}

#[cfg(not(target_arch = "x86_64"))]
pub fn init_embedded_with(detect: impl FnOnce() -> Option<u8>) -> Result<u8, &'static str> {
    detect().ok_or("UnsupportedCpu")
}

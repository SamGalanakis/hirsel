//! The profiler's host seam: how a CPU profile is captured, with one provider
//! per host OS chosen at compile time. Linux samples a launched or attached
//! process with perf events; macOS samples a launched Hirsel program from
//! inside its runtime. Both feed the same importer, profile and queries.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use linux as provider;
#[cfg(target_os = "macos")]
use macos as provider;

pub(crate) use provider::{
    CLOCK, KernelSymbols, TARGET, kernel_address, mapped_file, monotonic_ns, vdso,
};
pub use provider::{attach, run};

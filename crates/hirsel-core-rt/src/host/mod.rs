//! The runtime's host seam: one Hirsel-owned interface per concern (durable
//! sync, clocks, confined paths, child processes), with one provider per host
//! OS chosen at compile time. Readiness lives in `hirsel_core_net::ready`. No
//! runtime site waits on an address yet, so the seam has no address wait.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as provider;

pub use provider::exec;
pub(crate) use provider::{clock, path, sync};

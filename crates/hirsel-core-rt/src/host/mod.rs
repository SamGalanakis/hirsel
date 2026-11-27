//! The runtime's host seam: one Hirsel-owned interface per concern (durable
//! sync, file status, clocks, confined paths, child processes), with one provider per host
//! OS chosen at compile time. Readiness lives in `hirsel_core_net::ready`. No
//! runtime site waits on an address yet, so the seam has no address wait.
#[cfg(target_vendor = "apple")]
mod darwin;
pub(crate) mod envelope;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod exec_helper;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use exec_helper::embed_grants as embed_exec;
/// The `main` of the shipped `hirsel-exec-helper` executable (EMB-3).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use exec_helper::main as exec_helper_main;
/// EMB-7: an iOS launch refuses every Exec grant before this.
#[cfg(target_os = "ios")]
pub(crate) fn embed_exec(
    _: &[hirsel_core_net::exec::Request],
    _: &[u8],
    _: u32,
    _: &std::sync::Arc<crate::run_stop::GenerationEnd>,
) -> Result<Box<[crate::exec::Grant]>, String> {
    Ok(Box::default())
}
pub(crate) mod inherited;
#[cfg(target_os = "linux")]
mod linux;
pub(crate) mod trust;
#[cfg(target_vendor = "apple")]
use darwin as provider;
#[cfg(target_os = "linux")]
use linux as provider;

#[cfg(test)]
pub(crate) use provider::HOST_TARGET;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use provider::exec;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use provider::unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use provider::unix::Endpoint as UnixEndpoint;
#[cfg(target_os = "ios")]
pub(crate) type UnixEndpoint = std::convert::Infallible;
use hirsel_core_ids::launch::Metadata;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn native_metadata(target: &str, n_place: u64, describe: bool) -> Metadata {
    Metadata::native(target, n_place, describe)
}
#[cfg(target_os = "ios")]
pub(crate) fn native_metadata(target: &str, n_place: u64, describe: bool) -> Metadata {
    Metadata::source(target, n_place, describe)
}

pub(crate) mod unix_api;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix_native;
#[cfg(feature = "prof")]
pub(crate) use provider::prof_sampler;
pub(crate) use provider::{clock, contained, file, path, stat::stat, sync, sys};

/// A terminal's size as `cols | rows << 16` from `TIOCGWINSZ`, zero when the
/// descriptor answers no size or either dimension is zero. Linux and Darwin
/// share the call; only the request constant differs, and libc names it.
pub(crate) fn terminal_size(fd: i32) -> u64 {
    if fd < 0 {
        return 0;
    }
    // SAFETY: TIOCGWINSZ writes one winsize into the pointed-to struct.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut size) } != 0
        || size.ws_col == 0
        || size.ws_row == 0
    {
        return 0;
    }
    u64::from(size.ws_col) | u64::from(size.ws_row) << 16
}

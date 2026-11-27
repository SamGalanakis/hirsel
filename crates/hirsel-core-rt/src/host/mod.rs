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
    _: &std::sync::Arc<crate::run_stop::Execution>,
) -> Result<Box<[crate::exec::Grant]>, String> {
    Ok(Box::default())
}
pub(crate) mod inherited;
#[cfg(target_os = "linux")]
mod linux;
pub(crate) mod trust;
#[cfg(target_os = "linux")]
pub use linux::file::register_file_buffers;

pub(crate) const TIED_FILE_WRITES: bool = cfg!(target_os = "linux");

pub(crate) fn write_ring() -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    return linux::file::write_ring();
    #[cfg(target_vendor = "apple")]
    Ok(crate::units::uses_file_ring())
}
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

/// Current process resident set size in KiB on Linux, or 0 where unavailable.
pub fn resident_kib() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let page_kib = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64).max(4096) / 1024;
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map_or(0, |pages| pages * page_kib)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Mapping permission string (e.g. `"r--p"`) containing `addr` on Linux, or `None`.
pub fn mapping_perms(addr: usize) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
        maps.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            let (lo, hi) = parts.next()?.split_once('-')?;
            let lo = usize::from_str_radix(lo, 16).ok()?;
            let hi = usize::from_str_radix(hi, 16).ok()?;
            (lo <= addr && addr < hi).then(|| parts.next().unwrap_or("").to_string())
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = addr;
        None
    }
}

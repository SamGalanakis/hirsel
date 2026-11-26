//! The runtime's host seam: one Hirsel-owned interface per concern (durable
//! sync, file status, clocks, confined paths, child processes), with one provider per host
//! OS chosen at compile time. Readiness lives in `hirsel_core_net::ready`. No
//! runtime site waits on an address yet, so the seam has no address wait.
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
use darwin as provider;
#[cfg(target_os = "linux")]
use linux as provider;

#[cfg(test)]
pub(crate) use provider::HOST_TARGET;
pub use provider::exec;
pub(crate) use provider::{clock, file, path, stat::stat, sync, sys, unix};

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

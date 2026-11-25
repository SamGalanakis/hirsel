//! Net's host seam: the socket, confined-path and executable-image mechanisms
//! that differ between host kernels, with one provider per host OS chosen at
//! compile time. Readiness has its own seam in `reactor`.
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
use darwin as provider;
#[cfg(target_os = "linux")]
use linux as provider;

pub(crate) use provider::{image, udp, unix};

pub(crate) fn checked(result: i32) -> std::io::Result<i32> {
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Set a socket option that takes one native int.
pub(crate) fn set_option(
    fd: &impl std::os::fd::AsRawFd,
    level: i32,
    name: i32,
    value: i32,
) -> std::io::Result<()> {
    // SAFETY: the descriptor is live and the option reads one initialized int.
    checked(unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            level,
            name,
            (&raw const value).cast(),
            size_of::<i32>() as _,
        )
    })?;
    Ok(())
}

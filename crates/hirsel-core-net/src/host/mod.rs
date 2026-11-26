//! Net's host seam: the socket, confined-path and executable-image mechanisms
//! that differ between host kernels, with one provider per host OS chosen at
//! compile time. Readiness has its own seam in `reactor`.
#[cfg(target_vendor = "apple")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_vendor = "apple")]
use darwin as provider;
#[cfg(target_os = "linux")]
use linux as provider;

pub(crate) use provider::udp;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use provider::{image, unix as native_unix};

#[cfg(target_vendor = "apple")]
use darwin::path::open_beneath;
#[cfg(target_os = "linux")]
use linux::unix::open_beneath;

#[path = "unix_api.rs"]
pub mod unix;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod exec_image;
pub mod exec_api {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub use super::exec_image::{ImageSeal, PinnedImage, PinnedProfile, probe_seals};

    #[cfg(target_os = "ios")]
    pub fn probe_seals() -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}

/// The macOS provider's pinned Unix connect (`freeze`), which the runtime's
/// Darwin provider also uses for send-to; empty on Linux, whose runtime
/// resolves to the socket itself.
pub mod pinned {
    #[cfg(target_os = "macos")]
    pub use super::darwin::unix::{Frozen, freeze, socket_identity};
}

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

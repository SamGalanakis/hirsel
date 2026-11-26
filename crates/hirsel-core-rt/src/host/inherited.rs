//! Descriptor and image handoff used by standalone launchers.
#[cfg(target_os = "ios")]
use crate::provider::{Request, Step};
#[cfg(target_os = "ios")]
use std::{io, os::fd::OwnedFd};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod standalone;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use super::unix_native::receive_operator;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use standalone::{adopt_listener, receive_launch_image, receive_listener};

#[cfg(target_os = "ios")]
pub(crate) unsafe fn receive_listener() -> io::Result<Option<OwnedFd>> {
    Ok(None)
}

#[cfg(target_os = "ios")]
pub(crate) unsafe fn receive_operator() -> io::Result<Option<(OwnedFd, u32)>> {
    Ok(None)
}

#[cfg(target_os = "ios")]
pub(crate) unsafe fn adopt_listener(_: *mut Request, _: OwnedFd) -> Step {
    Step {
        outcome: crate::os_net::error(libc::ENOTSUP),
        ..Step::default()
    }
}

#[cfg(target_os = "ios")]
pub(crate) fn receive_launch_image() -> io::Result<Option<[u8; 32]>> {
    Ok(None)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use standalone::LISTEN_FD;

//! How this host reserves, protects, writes and publishes code pages.
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "ios")]
pub(crate) use ios::*;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;

/// What a run of slab pages allows.
#[derive(Clone, Copy)]
pub(crate) enum Access {
    None,
    /// A data run that is being filled.
    Write,
    /// A published data run.
    Read,
}

//! How this host reserves, protects and publishes code pages.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod other;

#[cfg(target_os = "linux")]
pub(crate) use linux::*;
#[cfg(not(target_os = "linux"))]
pub(crate) use other::*;

/// What a run of slab pages allows.
#[derive(Clone, Copy)]
pub(crate) enum Access {
    None,
    Write,
    Read,
    Execute,
}

//! The driver's host seam, beside its only user: one Hirsel-owned interface per
//! host mechanism the driver itself uses, with one provider per host OS chosen
//! at compile time. It moves to the crate root when a second module needs it.
//! What the driver emits never reads the host (macOS design §3).
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub(super) use darwin::clone_file;
#[cfg(target_os = "linux")]
pub(super) use linux::clone_file;

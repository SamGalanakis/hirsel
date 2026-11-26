//! The driver's host seam: one Hirsel-owned interface per host mechanism the
//! driver itself uses, with one provider per host OS chosen at compile time.
//! What the driver emits never reads the host (macOS design §3).
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub use darwin::close_inherited_descriptors;
#[cfg(target_os = "macos")]
pub(crate) use darwin::{spawn as launch, *};
#[cfg(target_os = "linux")]
pub use linux::close_inherited_descriptors;
#[cfg(target_os = "linux")]
pub(crate) use linux::{spawn as launch, *};

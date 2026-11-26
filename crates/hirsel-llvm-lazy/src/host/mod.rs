//! How this host opens the embedded engine component: one file per host OS.
//! `open_component` loads exactly the component's bytes, from a file no other
//! process can replace while the dynamic loader reads it.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{Component, LLVM_SCOPE, open_component};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{Component, LLVM_SCOPE, open_component};

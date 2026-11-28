//! Shared Darwin host mechanisms and macOS-only adapters.
pub(crate) mod clock;
pub(crate) mod contained;
#[cfg(target_os = "macos")]
pub mod exec;
pub(crate) mod file;
pub(crate) mod path;
#[cfg(feature = "prof")]
pub(crate) mod prof_sampler;
pub(crate) mod stat;
pub(crate) mod sync;
pub(crate) mod sys;
#[cfg(target_os = "macos")]
pub(crate) mod unix;
pub(crate) mod warm;

#[cfg(test)]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;

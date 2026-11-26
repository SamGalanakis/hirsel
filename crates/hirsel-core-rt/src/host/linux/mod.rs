//! The Linux host provider.
pub(crate) mod clock;
pub mod exec;
pub(crate) mod file;
pub(crate) mod path;
#[cfg(feature = "prof")]
pub(crate) mod prof_sampler;
pub(crate) mod stat;
pub(crate) mod sync;
pub(crate) mod sys;
pub(crate) mod unix;

#[cfg(all(test, target_arch = "x86_64"))]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::X86_64Linux;
#[cfg(all(test, target_arch = "aarch64"))]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Linux;

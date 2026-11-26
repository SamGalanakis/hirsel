//! The Darwin host provider (macOS 26 on arm64). Durable sync and STAT are
//! S8's; the rest is S7's.
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

#[cfg(test)]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;

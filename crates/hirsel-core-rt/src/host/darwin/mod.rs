//! The Darwin host provider. Durable sync and STAT are here (macOS S8);
//! clock, path and exec arrive with the rest of the provider (macOS S7).
pub(crate) mod stat;
pub(crate) mod sync;

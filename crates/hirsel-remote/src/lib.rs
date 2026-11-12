//! Runner calls across machines (conformance/old-spec/runners.md, `remote.tls@1`): the core
//! `RunFrame` wire type, a caller's protocol (free of I/O) and its provider, and the
//! transports that carry frames. The runtime and the interpreter's host I/O share it; a
//! runner host's protocol and call journal are `hirsel-remote-host`'s.

#[cfg(test)]
hirsel_guard::install!();

pub mod client;
pub mod data;
pub mod frame;
pub mod provider;
pub mod transport;
pub mod wire;

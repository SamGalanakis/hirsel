//! A `remote.tls@1` runner host (docs/spec/runners.md): its protocol, free of I/O, and
//! its call journal seam, over `hirsel-remote`'s frames. The driver wires them to the
//! transport and the callee runs (`hirsel serve`).

#[cfg(test)]
hirsel_guard::install!();

pub mod host;
pub mod journal;

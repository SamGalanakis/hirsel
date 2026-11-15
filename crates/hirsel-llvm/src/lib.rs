//! The LLVM backend: owned LLVM handles behind a small RAII layer, and CoreIR emission
//! through them.
//!
//! The binding is llvm-sys `=231.0.1` against the shared libLLVM 23.1.5 pinned in
//! `tools/native-toolchain/llvm-23.lock`. No raw LLVM handle is
//! public, and there is no fallback to another LLVM.

#[cfg(test)]
hirsel_guard::install!();

mod context;
pub mod core;

//! The Cranelift backend: CoreIR modules emitted as relocatable ELF objects, for fast
//! development builds.

#[cfg(test)]
hirsel_guard::install!();

pub mod core;
pub mod unwind;

mod object;

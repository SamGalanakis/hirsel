//! Heap and native differential execution over validated native programs.
pub mod coverage;
pub mod differential;
pub mod heap;
pub mod pairwise;
pub use coverage::backend_features;
pub use differential::ready_toolchain;
pub use differential::toolchain_for_matrix;
pub use differential::{
    Builds, Built, Configuration, Emission, Engine, MATRIX_VERSION, Objects, Outcome,
    ReadyExecution, ReadySupplier, Runtime, differential_ready, full_matrix, migrate_matrix,
    real_io_replay_options, real_io_replay_ready, runner_oracle_ready, runtime_builds,
    same_options, smoke_matrix, topology_matrix, trace_roots_matrix,
};
pub use heap::{HeapRun, HeapWorld, heap_campaign, heap_campaign_in, run_heap, run_heap_in};
use hirsel_native::*;
pub use pairwise::pairwise_matrix;
#[cfg(test)]
hirsel_guard::install!();
pub mod ownership_trace;

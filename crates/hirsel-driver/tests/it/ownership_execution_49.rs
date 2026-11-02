//! Actual source request, original Value plans, both native objects and runs.
use hirsel_driver::{COMPILER, native, requests::*, resident::ObservedSession};
use hirsel_engine::{CompileConfig, runtime::ProcessBudget, store::MemStore};
use std::sync::Arc;

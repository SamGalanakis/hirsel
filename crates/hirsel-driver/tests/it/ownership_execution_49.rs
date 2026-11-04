//! Actual source request, original Value plans, both native objects and runs.
use hirsel_driver::native;
use hirsel_driver::requests::*;
use hirsel_driver::resident::ObservedSession;
use hirsel_engine::{CompileConfig, runtime::ProcessBudget, store::MemStore};
use hirsel_front_phases::COMPILER;
use std::sync::Arc;

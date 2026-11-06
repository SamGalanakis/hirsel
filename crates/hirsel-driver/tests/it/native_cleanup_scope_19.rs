//! An ordinary direct call must not consume a peer cleanup-body observation.
use hirsel_driver::native;
use hirsel_engine::*;
use hirsel_engine_store as store;
use hirsel_engine_store::ResultStore;
use hirsel_front_phases::compiler_adapters;

use std::sync::Arc;

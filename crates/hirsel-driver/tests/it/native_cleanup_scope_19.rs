//! An ordinary direct call must not consume a peer cleanup-body observation.
use hirsel_driver::native;
use hirsel_engine::{
    store::{self, ResultStore},
    *,
};
use hirsel_front_phases::compiler_adapters;
use std::sync::Arc;

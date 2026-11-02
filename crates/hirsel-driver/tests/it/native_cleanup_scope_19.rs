//! An ordinary direct call must not consume a peer cleanup-body observation.
use hirsel_driver::{compiler_adapters, native};
use hirsel_engine::{
    store::{self, ResultStore},
    *,
};
use std::sync::Arc;

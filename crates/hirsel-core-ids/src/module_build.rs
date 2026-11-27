//! PACK-3's private worker reply. Process signals and resource exits travel out of band.
use serde::{Deserialize, Serialize};

pub const WORKER: &str = "__build-module";

#[derive(Debug, Deserialize, Serialize)]
pub enum ModuleBuild {
    Built,
    Rejected,
    Unsupported(String),
    ToolRefusal,
}

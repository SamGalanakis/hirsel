//! Host-driven view instances rendered by clients.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewInstance {
    pub instance_id: String,
    pub placement: String,
    pub spec: serde_json::Value,
}

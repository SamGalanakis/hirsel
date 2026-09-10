//! Current long-running monitor resources with explicit Thread destinations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    Running,
    Done,
    Failed,
    Cancelled,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub thread_id: u64,
    pub id: String,
    pub kind: ProcessKind,
    pub label: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub state: ProcessState,
    pub started_ts: DateTime<Utc>,
    pub last_event_ts: DateTime<Utc>,
    pub summary: Option<String>,
}

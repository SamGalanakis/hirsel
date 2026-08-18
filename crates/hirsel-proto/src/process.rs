//! Long-running host processes (subagents, monitors) and side-chat summaries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    Subagent,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideChatSummary {
    pub sc: String,
    pub ping_id: u64,
}

//! Lash process registry projections with explicit Thread destinations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Waiting,
    Done,
    Failed,
    Cancelled,
    Abandoned,
    CallerDeparted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub thread_id: u64,
    /// Stable identity of one process name within its owning Thread.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_process_id: Option<String>,
    #[serde(default)]
    pub trigger_recurring: bool,
    pub name: String,
    pub trigger: Option<String>,
    pub trigger_subscription_key: Option<String>,
    pub trigger_revision: Option<u64>,
    pub trigger_enabled: Option<bool>,
    pub cancellable: bool,
    pub state: ProcessState,
    pub started_ts: DateTime<Utc>,
    pub last_event_ts: DateTime<Utc>,
    pub last_fired_ts: Option<DateTime<Utc>>,
    pub last_outcome: Option<String>,
}

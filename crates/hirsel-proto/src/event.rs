//! The typed event queue: events, their sources, status, and quick replies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickReply {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    Open,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Judgment,
    Summary,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSourceKind {
    Agent,
    Subagent,
    Scheduled,
    Monitor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSource {
    pub kind: EventSourceKind,
    #[serde(rename = "ref")]
    pub r#ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub kind: EventKind,
    pub source: EventSource,
    pub name: String,
    pub description: String,
    pub ui: serde_json::Value,
    pub anchor: u64,
    pub requires_response: bool,
    pub quick_replies: Vec<QuickReply>,
    pub status: EventStatus,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub snoozed_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    /// Process-local side-chat scope currently discussing this event.
    #[serde(default)]
    pub fork_sc: Option<String>,
    pub ts: DateTime<Utc>,
}

/// Source compatibility for the Rust client and the existing agent-tool surface.
pub type Ping = Event;
pub type PingStatus = EventStatus;

//! Chat rows, their authors, and the blobs/tool summaries they carry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatAuthor {
    Owner,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<TaskFocus>,
    pub thread_id: u64,
    #[serde(default)]
    pub mentions: Vec<u64>,
    pub id: u64,
    pub author: ChatAuthor,
    pub body: String,
    #[serde(rename = "ref")]
    pub r#ref: Option<u64>,
    pub ts: DateTime<Utc>,
    #[serde(default)]
    pub attachments: Vec<Blob>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallSummary>,
}

/// A bounded, accepted snapshot of the Task the Owner is discussing in a
/// project chat. It carries context only; it never widens the recipient's
/// reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFocus {
    pub task_thread_id: u64,
    pub snapshot: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub id: String,
    pub name: String,
    pub ok: bool,
}

/// A solicited process delivery, distinct from the Agent's conversational reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageOrigin {
    Process {
        process_id: String,
        name: String,
        trigger: TriggerLabel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subscription_key: Option<String>,
        outcome: ProcessOutcome,
        result: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOutcome {
    Completed,
    Failed,
    Cancelled,
    Woke,
}

/// Display metadata from the registered source, never its subscription hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerLabel {
    Timer {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        every_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
    },
    Cron {
        expr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tz: Option<String>,
    },
    Thread {
        event: String,
        thread_id: u64,
        title: String,
    },
    Other {
        key: String,
    },
}

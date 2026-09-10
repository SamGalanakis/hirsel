//! Durable work identity. Conversation, execution and attention are independent.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadAttention {
    #[default]
    Quiet,
    NeedsOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: u64,
    pub parent_thread_id: Option<u64>,
    pub pinned_at: Option<DateTime<Utc>>,
    pub title: String,
    /// Custom compact emoji/symbol; None selects the client-generated avatar.
    #[serde(default)]
    pub icon: Option<String>,
    /// One persistent artifact presented beside this Thread conversation.
    #[serde(default)]
    pub showcased_artifact_id: Option<u64>,
    pub description: String,
    pub instrument: serde_json::Value,
    pub attention: ThreadAttention,
    pub settled_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub read: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: u64,
    /// Actual running transition; queued work never supplies a working timer.
    pub running_turn: Option<ThreadTurn>,
    pub queued_turn_count: u64,
    /// Latest terminal outcome, independent of explicit Thread settlement.
    /// A cancelled queued turn's started_at is its acceptance time, not work time.
    pub last_finished_turn: Option<ThreadTurn>,
    /// Latest factual conversation/execution activity, falling back to creation.
    /// Reading or changing lifecycle metadata does not advance this timestamp.
    pub last_activity_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadActivity {
    pub id: u64,
    pub thread_id: u64,
    pub turn_id: Option<u64>,
    pub artifact_ids: Vec<u64>,
    pub kind: String,
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadTurnState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTurn {
    pub id: u64,
    pub thread_id: u64,
    pub requester_thread_id: Option<u64>,
    pub requester_turn_id: Option<u64>,
    pub owner_message_id: Option<u64>,
    pub agent_message_id: Option<u64>,
    pub state: ThreadTurnState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadBrief {
    pub text: String,
    pub artifact_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDetail {
    pub related_items: Vec<ThreadRelatedItem>,
    pub brief: ThreadBrief,
    pub thread: Thread,
    pub messages: Vec<crate::ChatMessage>,
    pub turns: Vec<ThreadTurn>,
    /// Durable, exactly ordered timeline events for turns represented in this
    /// bounded message page. Legacy turns can truthfully have no events.
    pub turn_timelines: Vec<crate::ThreadTurnTimeline>,
    pub activities: Vec<ThreadActivity>,
    pub has_more: bool,
}

/// An explicitly saved URL reference, independent of artifact content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRelatedItem {
    pub id: u64,
    pub thread_id: u64,
    pub target: ThreadRelatedTarget,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadRelatedTarget {
    Url { url: String },
    Thread { history_id: String, thread_id: u64 },
}

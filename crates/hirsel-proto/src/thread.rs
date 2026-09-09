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
    pub title: String,
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
    pub owner_message_id: Option<u64>,
    pub agent_message_id: Option<u64>,
    pub state: ThreadTurnState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDetail {
    pub thread: Thread,
    pub messages: Vec<crate::ChatMessage>,
    pub turns: Vec<ThreadTurn>,
    pub activities: Vec<ThreadActivity>,
    pub has_more: bool,
}

//! Durable work identity. Conversation, execution and attention are independent.
use crate::ThreadIcon;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Durable product semantics for a Thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    Space,
    Task,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadAttention {
    #[default]
    Quiet,
    NeedsOwner,
}

/// Where a Thread's next turn runs, named the way both the Owner and the Agent
/// name it: an agent plus the selectors that agent understands. Deliberately
/// key-free and path-free — it is the public identity of a backend, not its
/// captured configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadExecutionTarget {
    /// Hirsel's own session: one provider instance and one model, with the
    /// full Thread tool set and the four coding operations.
    Native { provider_id: String, model: String },
    Cli {
        agent: String,
        model: String,
        variant: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: u64,
    pub kind: ThreadKind,
    pub parent_thread_id: Option<u64>,
    pub pinned_at: Option<DateTime<Utc>>,
    pub title: String,
    /// Vocabulary symbol or retained image blob; None selects the title monogram.
    #[serde(default)]
    pub icon: Option<ThreadIcon>,
    /// One persistent artifact presented beside this Thread conversation.
    #[serde(default)]
    pub showcased_artifact_id: Option<u64>,
    pub description: String,
    /// The Owner's chosen backend for the next turn. None inherits the
    /// configured default Native provider and model; a running turn keeps what
    /// it captured.
    #[serde(default)]
    pub execution: Option<ThreadExecutionTarget>,
    pub instrument: Option<serde_json::Value>,
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

impl ThreadTurnState {
    pub const ALL: [Self; 6] = [
        Self::Queued,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::Interrupted,
    ];

    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Queued | Self::Running => false,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted => true,
        }
    }
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
    /// Immutable time this work was accepted, before any queue wait.
    pub accepted_at: DateTime<Utc>,
    /// Actual execution start; absent for queued work, including cancellation before admission.
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadBrief {
    pub text: String,
    pub artifact_ids: Vec<u64>,
}

/// Who widened a Thread's reach. Only the Owner or a strict ancestor can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadGrantSource {
    Owner,
    Thread { thread_id: u64 },
}

/// What one grant widens a Thread's reach to: one named Thread and everything
/// under it, or the root — every Thread in the history, including Threads
/// created after the grant was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadGrantTarget {
    Thread {
        thread_id: u64,
        /// The target's current title, so a reach list needs no second lookup.
        title: String,
        /// The target's kind, so every surface names it the way the Owner
        /// does. Not `kind`: that name is the variant tag.
        thread_kind: ThreadKind,
    },
    Root,
}

/// One durable widening of a Thread's reach: `thread_id` may address `target`
/// and everything under it, exactly as if it were its own subtree. Default
/// reach (self + descendants) is never stored as a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadGrant {
    pub thread_id: u64,
    pub target: ThreadGrantTarget,
    pub granted_by: ThreadGrantSource,
    pub granted_at: DateTime<Utc>,
    pub note: Option<String>,
}

/// A reach target as an op names it: a Thread ID, or the literal `"root"`.
/// One spelling for the Owner's client ops and the agent's grant tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ReachTargetWire", into = "ReachTargetWire")]
pub enum ReachTarget {
    Root,
    Thread { thread_id: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ReachTargetWire {
    Thread(u64),
    Root(String),
}

impl TryFrom<ReachTargetWire> for ReachTarget {
    type Error = String;
    fn try_from(wire: ReachTargetWire) -> Result<Self, Self::Error> {
        match wire {
            ReachTargetWire::Thread(thread_id) if thread_id > 0 => Ok(Self::Thread { thread_id }),
            ReachTargetWire::Thread(_) => Err("a Thread ID is a positive integer".into()),
            ReachTargetWire::Root(name) if name == "root" => Ok(Self::Root),
            ReachTargetWire::Root(_) => {
                Err("a reach target is a Thread ID or the string \"root\"".into())
            }
        }
    }
}
impl From<ReachTarget> for ReachTargetWire {
    fn from(target: ReachTarget) -> Self {
        match target {
            ReachTarget::Root => Self::Root("root".into()),
            ReachTarget::Thread { thread_id } => Self::Thread(thread_id),
        }
    }
}
impl std::fmt::Display for ReachTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root => write!(f, "everything (root)"),
            Self::Thread { thread_id } => write!(f, "Thread #{thread_id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDetail {
    pub related_items: Vec<ThreadRelatedItem>,
    /// This Thread's durable widenings, visible before and during a turn.
    pub grants: Vec<ThreadGrant>,
    pub brief: ThreadBrief,
    pub thread: Thread,
    pub messages: Vec<crate::ChatMessage>,
    pub turns: Vec<ThreadTurn>,
    /// Durable effects for the turns represented by this bounded page.
    pub effects: Vec<crate::ThreadEffect>,
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

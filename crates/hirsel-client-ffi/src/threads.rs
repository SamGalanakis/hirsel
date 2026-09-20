use hirsel_client_core as core;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadKind {
    Space,
    Task,
}

/// The tile colour, mirroring `hirsel_proto::ThreadTint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadTint {
    Neutral,
    Red,
    Orange,
    Amber,
    Green,
    Teal,
    Blue,
    Violet,
    Pink,
}
impl From<core::ThreadTint> for ThreadTint {
    fn from(tint: core::ThreadTint) -> Self {
        match tint {
            core::ThreadTint::Neutral => Self::Neutral,
            core::ThreadTint::Red => Self::Red,
            core::ThreadTint::Orange => Self::Orange,
            core::ThreadTint::Amber => Self::Amber,
            core::ThreadTint::Green => Self::Green,
            core::ThreadTint::Teal => Self::Teal,
            core::ThreadTint::Blue => Self::Blue,
            core::ThreadTint::Violet => Self::Violet,
            core::ThreadTint::Pink => Self::Pink,
        }
    }
}
impl From<ThreadTint> for core::ThreadTint {
    fn from(tint: ThreadTint) -> Self {
        match tint {
            ThreadTint::Neutral => Self::Neutral,
            ThreadTint::Red => Self::Red,
            ThreadTint::Orange => Self::Orange,
            ThreadTint::Amber => Self::Amber,
            ThreadTint::Green => Self::Green,
            ThreadTint::Teal => Self::Teal,
            ThreadTint::Blue => Self::Blue,
            ThreadTint::Violet => Self::Violet,
            ThreadTint::Pink => Self::Pink,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadIcon {
    Symbol { name: String, tint: ThreadTint },
    Image { blob_id: String },
}
impl From<core::ThreadIcon> for ThreadIcon {
    fn from(icon: core::ThreadIcon) -> Self {
        match icon {
            core::ThreadIcon::Symbol { name, tint } => Self::Symbol {
                name,
                tint: tint.into(),
            },
            core::ThreadIcon::Image { blob_id } => Self::Image { blob_id },
        }
    }
}
impl From<ThreadIcon> for core::ThreadIcon {
    fn from(icon: ThreadIcon) -> Self {
        match icon {
            ThreadIcon::Symbol { name, tint } => Self::Symbol {
                name,
                tint: tint.into(),
            },
            ThreadIcon::Image { blob_id } => Self::Image { blob_id },
        }
    }
}
impl From<core::ThreadKind> for ThreadKind {
    fn from(kind: core::ThreadKind) -> Self {
        match kind {
            core::ThreadKind::Space => Self::Space,
            core::ThreadKind::Task => Self::Task,
        }
    }
}
impl From<ThreadKind> for core::ThreadKind {
    fn from(kind: ThreadKind) -> Self {
        match kind {
            ThreadKind::Space => Self::Space,
            ThreadKind::Task => Self::Task,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Thread {
    pub kind: ThreadKind,
    pub parent_thread_id: Option<u64>,
    pub pinned_at: Option<String>,
    pub id: u64,
    pub title: String,
    pub icon: Option<ThreadIcon>,
    pub showcased_artifact_id: Option<u64>,
    pub description: String,
    pub instrument_json: Option<String>,
    pub state: ThreadState,
    pub needs_owner: bool,
    pub settled_at: Option<String>,
    pub archived_at: Option<String>,
    pub snoozed_until: Option<String>,
    pub read: bool,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub running_turn: Option<ThreadTurn>,
    pub queued_turn_count: u64,
    pub last_finished_turn: Option<ThreadTurn>,
    pub last_activity_at: String,
}
impl From<core::Thread> for Thread {
    fn from(t: core::Thread) -> Self {
        Self {
            kind: t.kind.into(),
            parent_thread_id: t.parent_thread_id,
            pinned_at: t.pinned_at.map(|t| t.to_rfc3339()),
            id: t.id,
            title: t.title,
            icon: t.icon.map(Into::into),
            showcased_artifact_id: t.showcased_artifact_id,
            description: t.description,
            instrument_json: t.instrument.map(|ui| ui.to_string()),
            state: t.state.into(),
            needs_owner: t.attention == core::ThreadAttention::NeedsOwner,
            settled_at: t.settled_at.map(|t| t.to_rfc3339()),
            archived_at: t.archived_at.map(|t| t.to_rfc3339()),
            snoozed_until: t.snoozed_until.map(|t| t.to_rfc3339()),
            read: t.read,
            created_at: t.created_at.to_rfc3339(),
            updated_at: t.updated_at.to_rfc3339(),
            revision: t.revision,
            running_turn: t.running_turn.map(Into::into),
            queued_turn_count: t.queued_turn_count,
            last_finished_turn: t.last_finished_turn.map(Into::into),
            last_activity_at: t.last_activity_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadState {
    pub revision: u64,
    pub headline: String,
    pub own_headline: String,
    pub findings: Vec<String>,
    pub artifact_ids: Vec<u64>,
    pub checkpoint_at: Option<String>,
    pub steering_revision: u64,
}

impl From<core::ThreadState> for ThreadState {
    fn from(state: core::ThreadState) -> Self {
        Self {
            revision: state.revision,
            headline: state.headline,
            own_headline: state.own_headline,
            findings: state.findings,
            artifact_ids: state.artifact_ids,
            checkpoint_at: state.checkpoint_at.map(|value| value.to_rfc3339()),
            steering_revision: state.steering_revision,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadTurn {
    pub requester_thread_id: Option<u64>,
    pub requester_turn_id: Option<u64>,
    pub id: u64,
    pub thread_id: u64,
    pub owner_message_id: Option<u64>,
    pub agent_message_id: Option<u64>,
    pub state: ThreadTurnState,
    pub accepted_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadTurnState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl From<core::ThreadTurnState> for ThreadTurnState {
    fn from(state: core::ThreadTurnState) -> Self {
        match state {
            core::ThreadTurnState::Queued => Self::Queued,
            core::ThreadTurnState::Running => Self::Running,
            core::ThreadTurnState::Completed => Self::Completed,
            core::ThreadTurnState::Failed => Self::Failed,
            core::ThreadTurnState::Cancelled => Self::Cancelled,
            core::ThreadTurnState::Interrupted => Self::Interrupted,
        }
    }
}

impl From<core::ThreadTurn> for ThreadTurn {
    fn from(t: core::ThreadTurn) -> Self {
        Self {
            id: t.id,
            thread_id: t.thread_id,
            requester_thread_id: t.requester_thread_id,
            requester_turn_id: t.requester_turn_id,
            owner_message_id: t.owner_message_id,
            agent_message_id: t.agent_message_id,
            state: t.state.into(),
            accepted_at: t.accepted_at.to_rfc3339(),
            started_at: t.started_at.map(|time| time.to_rfc3339()),
            finished_at: t.finished_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadEffectKind {
    Created,
    SentTo,
    Delegated,
    Read,
    Edited,
    Refused,
}
impl From<core::ThreadEffectKind> for ThreadEffectKind {
    fn from(value: core::ThreadEffectKind) -> Self {
        match value {
            core::ThreadEffectKind::Created => Self::Created,
            core::ThreadEffectKind::SentTo => Self::SentTo,
            core::ThreadEffectKind::Delegated => Self::Delegated,
            core::ThreadEffectKind::Read => Self::Read,
            core::ThreadEffectKind::Edited => Self::Edited,
            core::ThreadEffectKind::Refused => Self::Refused,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadEffectTarget {
    Thread { thread_id: u64 },
    Artifact { artifact_id: u64 },
    Root,
}
impl From<core::ThreadEffectTarget> for ThreadEffectTarget {
    fn from(value: core::ThreadEffectTarget) -> Self {
        match value {
            core::ThreadEffectTarget::Thread { thread_id } => Self::Thread { thread_id },
            core::ThreadEffectTarget::Artifact { artifact_id } => Self::Artifact { artifact_id },
            core::ThreadEffectTarget::Root => Self::Root,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadEffectReceipt {
    pub id: u64,
    pub turn_id: u64,
    pub operation_id: String,
    pub effect_index: u32,
    pub tool: String,
    pub effect: ThreadEffectKind,
    pub target: ThreadEffectTarget,
    pub target_turn_id: Option<u64>,
    pub request_client_id: Option<String>,
    pub refusal_json: Option<String>,
    pub created_at: String,
}
impl From<core::ThreadEffectReceipt> for ThreadEffectReceipt {
    fn from(value: core::ThreadEffectReceipt) -> Self {
        Self {
            id: value.id,
            turn_id: value.turn_id,
            operation_id: value.operation_id,
            effect_index: value.effect_index,
            tool: value.tool,
            effect: value.effect.into(),
            target: value.target.into(),
            target_turn_id: value.target_turn_id,
            request_client_id: value.request_client_id,
            refusal_json: value
                .refusal
                .map(|refusal| serde_json::to_string(&refusal).expect("effect refusal serializes")),
            created_at: value.created_at.to_rfc3339(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum EffectAction {
    Open { target: ThreadEffectTarget },
    Archive { thread_id: u64 },
    CancelQueued { thread_id: u64, turn_id: u64 },
    Stop { thread_id: u64, turn_id: u64 },
}
impl From<core::EffectAction> for EffectAction {
    fn from(value: core::EffectAction) -> Self {
        match value {
            core::EffectAction::Open { target } => Self::Open {
                target: target.into(),
            },
            core::EffectAction::Archive { thread_id } => Self::Archive { thread_id },
            core::EffectAction::CancelQueued { thread_id, turn_id } => {
                Self::CancelQueued { thread_id, turn_id }
            }
            core::EffectAction::Stop { thread_id, turn_id } => Self::Stop { thread_id, turn_id },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadEffect {
    pub receipt: ThreadEffectReceipt,
    pub actions: Vec<EffectAction>,
}
impl From<core::ThreadEffect> for ThreadEffect {
    fn from(value: core::ThreadEffect) -> Self {
        Self {
            receipt: value.receipt.into(),
            actions: value.actions.into_iter().map(Into::into).collect(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadActivity {
    pub artifact_ids: Vec<u64>,
    pub id: u64,
    pub thread_id: u64,
    pub turn_id: Option<u64>,
    pub kind: String,
    pub data_json: String,
    pub timestamp: String,
}
impl From<core::ThreadActivity> for ThreadActivity {
    fn from(a: core::ThreadActivity) -> Self {
        Self {
            id: a.id,
            thread_id: a.thread_id,
            turn_id: a.turn_id,
            artifact_ids: a.artifact_ids,
            kind: a.kind,
            data_json: a.data.to_string(),
            timestamp: a.ts.to_rfc3339(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadStream {
    pub thread_id: u64,
    pub turn_id: u64,
    pub events_json: String,
    pub activity: crate::AgentActivity,
    pub finished: bool,
}
impl From<core::ThreadStream> for ThreadStream {
    fn from(s: core::ThreadStream) -> Self {
        Self {
            thread_id: s.thread_id,
            turn_id: s.turn_id,
            events_json: serde_json::to_string(&s.events).expect("turn events serialize"),
            activity: s.activity.into(),
            finished: s.finished,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CreatedThread {
    pub client_id: String,
    pub thread_id: u64,
}
impl From<core::CreatedThread> for CreatedThread {
    fn from(c: core::CreatedThread) -> Self {
        Self {
            client_id: c.client_id,
            thread_id: c.thread_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadBrief {
    pub thread_id: u64,
    pub text: String,
    pub artifact_ids: Vec<u64>,
}
impl From<core::ThreadBrief> for ThreadBrief {
    fn from(b: core::ThreadBrief) -> Self {
        Self {
            thread_id: b.thread_id,
            text: b.text,
            artifact_ids: b.artifact_ids,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadRelatedItem {
    pub id: u64,
    pub thread_id: u64,
    pub target: ThreadRelatedTarget,
    pub title: Option<String>,
    pub created_at: String,
}
impl From<core::ThreadRelatedItem> for ThreadRelatedItem {
    fn from(link: core::ThreadRelatedItem) -> Self {
        Self {
            id: link.id,
            thread_id: link.thread_id,
            target: link.target.into(),
            title: link.title,
            created_at: link.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadRelatedTarget {
    Url { url: String },
    Thread { history_id: String, thread_id: u64 },
}
impl From<core::ThreadRelatedTarget> for ThreadRelatedTarget {
    fn from(target: core::ThreadRelatedTarget) -> Self {
        match target {
            core::ThreadRelatedTarget::Url { url } => Self::Url { url },
            core::ThreadRelatedTarget::Thread {
                history_id,
                thread_id,
            } => Self::Thread {
                history_id,
                thread_id,
            },
        }
    }
}
impl From<ThreadRelatedTarget> for core::ThreadRelatedTarget {
    fn from(target: ThreadRelatedTarget) -> Self {
        match target {
            ThreadRelatedTarget::Url { url } => Self::Url { url },
            ThreadRelatedTarget::Thread {
                history_id,
                thread_id,
            } => Self::Thread {
                history_id,
                thread_id,
            },
        }
    }
}

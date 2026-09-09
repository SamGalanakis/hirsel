use hirsel_client_core as core;

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Thread {
    pub id: u64,
    pub title: String,
    pub description: String,
    pub instrument_json: String,
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
            id: t.id,
            title: t.title,
            description: t.description,
            instrument_json: t.instrument.to_string(),
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
pub struct ThreadTurn {
    pub id: u64,
    pub thread_id: u64,
    pub owner_message_id: Option<u64>,
    pub agent_message_id: Option<u64>,
    pub state: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}
impl From<core::ThreadTurn> for ThreadTurn {
    fn from(t: core::ThreadTurn) -> Self {
        Self {
            id: t.id,
            thread_id: t.thread_id,
            owner_message_id: t.owner_message_id,
            agent_message_id: t.agent_message_id,
            state: serde_json::to_value(t.state)
                .expect("turn state serializes")
                .as_str()
                .unwrap()
                .to_string(),
            started_at: t.started_at.to_rfc3339(),
            finished_at: t.finished_at.map(|t| t.to_rfc3339()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThreadActivity {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thread_ffi_keeps_lifecycle_instrument_and_revision() {
        let wire = serde_json::json!({"id":5,"title":"Groceries","description":"Milk","instrument":{"type":"text","text":"Milk"},"attention":"needs_owner","settled_at":null,"archived_at":null,"snoozed_until":null,"read":true,"created_at":"2026-09-09T10:00:00Z","updated_at":"2026-09-09T10:00:00Z","revision":8,"running_turn":null,"queued_turn_count":0,"last_finished_turn":null,"last_activity_at":"2026-09-09T10:00:00Z"});
        let thread = Thread::from(serde_json::from_value::<core::Thread>(wire).unwrap());
        assert_eq!(thread.id, 5);
        assert_eq!(thread.revision, 8);
        assert!(thread.read && thread.needs_owner);
        assert!(thread.settled_at.is_none());
        assert!(thread.running_turn.is_none());
        assert!(thread.last_finished_turn.is_none());
        assert_eq!(thread.queued_turn_count, 0);
        assert_eq!(thread.last_activity_at, "2026-09-09T10:00:00+00:00");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&thread.instrument_json).unwrap()["text"],
            "Milk"
        );
    }
    #[test]
    fn thread_ffi_preserves_execution_timing_and_terminal_outcomes_independently() {
        for outcome in ["completed", "failed", "cancelled", "interrupted"] {
            let wire = serde_json::json!({
                "id": 5, "title": "Groceries", "description": "Milk", "instrument": null,
                "attention": "needs_owner", "settled_at": null, "archived_at": null,
                "snoozed_until": null, "read": true, "revision": 8,
                "created_at": "2026-09-09T10:00:00Z", "updated_at": "2026-09-09T12:00:00Z",
                "running_turn": {
                    "id": 12, "thread_id": 5, "owner_message_id": 20, "agent_message_id": null,
                    "state": "running", "started_at": "2026-09-09T10:02:00Z", "finished_at": null
                },
                "queued_turn_count": 2,
                "last_finished_turn": {
                    "id": 11, "thread_id": 5, "owner_message_id": 18, "agent_message_id": 19,
                    "state": outcome, "started_at": "2026-09-09T10:00:00Z",
                    "finished_at": "2026-09-09T10:01:00Z"
                },
                "last_activity_at": "2026-09-09T10:02:30Z"
            });
            let thread = Thread::from(serde_json::from_value::<core::Thread>(wire).unwrap());
            assert_eq!(thread.queued_turn_count, 2);
            assert_eq!(thread.last_activity_at, "2026-09-09T10:02:30+00:00");
            assert_ne!(thread.last_activity_at, thread.updated_at);
            assert!(thread.read && thread.needs_owner && thread.settled_at.is_none());
            assert_eq!(thread.revision, 8);
            assert_eq!(
                thread.running_turn,
                Some(ThreadTurn {
                    id: 12,
                    thread_id: 5,
                    owner_message_id: Some(20),
                    agent_message_id: None,
                    state: "running".into(),
                    started_at: "2026-09-09T10:02:00+00:00".into(),
                    finished_at: None,
                })
            );
            assert_eq!(
                thread.last_finished_turn,
                Some(ThreadTurn {
                    id: 11,
                    thread_id: 5,
                    owner_message_id: Some(18),
                    agent_message_id: Some(19),
                    state: outcome.into(),
                    started_at: "2026-09-09T10:00:00+00:00".into(),
                    finished_at: Some("2026-09-09T10:01:00+00:00".into()),
                })
            );
        }
    }
    #[test]
    fn message_ffi_keeps_confirmed_send_identity_and_citations() {
        let message =
            crate::ChatMessage::from(core::ChatEntry::Confirmed(core::ConfirmedMessage {
                id: 42,
                thread_id: 5,
                client_id: Some("send-1".into()),
                mentions: vec![9],
                author: core::ChatAuthor::Owner,
                body: "Groceries".into(),
                reply_to: None,
                timestamp: "now".into(),
                attachments: vec![],
                tool_calls: vec![],
            }));
        assert_eq!(message.thread_id, 5);
        assert_eq!(message.mentions, vec![9]);
        assert_eq!(message.client_id.as_deref(), Some("send-1"));
        assert!(!message.pending);
    }
}

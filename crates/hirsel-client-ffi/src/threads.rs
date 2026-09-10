use hirsel_client_core as core;

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Thread {
    pub parent_thread_id: Option<u64>,
    pub pinned_at: Option<String>,
    pub id: u64,
    pub title: String,
    pub icon: Option<String>,
    pub showcased_artifact_id: Option<u64>,
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
            parent_thread_id: t.parent_thread_id,
            pinned_at: t.pinned_at.map(|t| t.to_rfc3339()),
            id: t.id,
            title: t.title,
            icon: t.icon,
            showcased_artifact_id: t.showcased_artifact_id,
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
    pub requester_thread_id: Option<u64>,
    pub requester_turn_id: Option<u64>,
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
            requester_thread_id: t.requester_thread_id,
            requester_turn_id: t.requester_turn_id,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thread_ffi_keeps_lifecycle_instrument_and_revision() {
        let wire = serde_json::json!({"id":5,"parent_thread_id":2,"pinned_at":"2026-09-09T10:00:00Z","title":"Groceries","icon":"🧑🏽‍💻","showcased_artifact_id":42,"description":"Milk","instrument":{"type":"text","text":"Milk"},"attention":"needs_owner","settled_at":null,"archived_at":null,"snoozed_until":null,"read":true,"created_at":"2026-09-09T10:00:00Z","updated_at":"2026-09-09T10:00:00Z","revision":8,"running_turn":null,"queued_turn_count":0,"last_finished_turn":null,"last_activity_at":"2026-09-09T10:00:00Z"});
        let thread = Thread::from(serde_json::from_value::<core::Thread>(wire).unwrap());
        assert_eq!(thread.id, 5);
        assert_eq!(thread.showcased_artifact_id, Some(42));
        assert_eq!(thread.icon.as_deref(), Some("🧑🏽‍💻"));
        assert_eq!(thread.parent_thread_id, Some(2));
        assert!(thread.pinned_at.is_some());
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
                "id": 5, "parent_thread_id":2,"pinned_at":null, "title": "Groceries", "description": "Milk", "instrument": null,
                "attention": "needs_owner", "settled_at": null, "archived_at": null,
                "snoozed_until": null, "read": true, "revision": 8,
                "created_at": "2026-09-09T10:00:00Z", "updated_at": "2026-09-09T12:00:00Z",
                "running_turn": {
                    "id": 12, "thread_id": 5, "requester_thread_id":2,"requester_turn_id":null,"owner_message_id": 20, "agent_message_id": null,
                    "state": "running", "started_at": "2026-09-09T10:02:00Z", "finished_at": null
                },
                "queued_turn_count": 2,
                "last_finished_turn": {
                    "id": 11, "thread_id": 5, "requester_thread_id":2,"requester_turn_id":null,"owner_message_id": 18, "agent_message_id": 19,
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
                    requester_thread_id: Some(2),
                    requester_turn_id: None,
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
                    requester_thread_id: Some(2),
                    requester_turn_id: None,
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
    fn typed_target_roundtrip_keeps_history_and_distinguishes_url_from_thread() {
        for target in [
            core::ThreadRelatedTarget::Thread {
                history_id: "original-history".into(),
                thread_id: 0,
            },
            core::ThreadRelatedTarget::Url {
                url: "https://example.com/t/0?history=original-history".into(),
            },
        ] {
            let ffi = ThreadRelatedTarget::from(target.clone());
            assert_eq!(core::ThreadRelatedTarget::from(ffi), target);
        }
    }

    #[test]
    fn related_link_snapshot_and_correlated_results_cross_ffi() {
        let link: core::ThreadRelatedItem = serde_json::from_value(serde_json::json!({
            "id": 7, "thread_id": 5, "target": {"kind":"url", "url": "https://example.com/?q=1#part"},
            "title": "Reference", "created_at": "2026-09-09T10:00:00Z"
        }))
        .unwrap();
        let client = core::Client::new(core::ClientConfig::new(
            "localhost:3090".into(),
            "test".into(),
        ))
        .unwrap();
        let mut source = client.snapshot();
        source.related_items.push(link);
        let snapshot = crate::ClientSnapshot::from(source);
        assert_eq!(
            snapshot.related_items,
            vec![ThreadRelatedItem {
                id: 7,
                thread_id: 5,
                target: ThreadRelatedTarget::Url {
                    url: "https://example.com/?q=1#part".into()
                },
                title: Some("Reference".into()),
                created_at: "2026-09-09T10:00:00+00:00".into(),
            }]
        );
        assert_eq!(
            crate::LifecycleEvent::from(core::LifecycleEvent::ThreadActionApplied {
                client_id: "action".into(),
                history_id: "A".into(),
                thread_id: 5,
            }),
            crate::LifecycleEvent::ThreadActionApplied {
                client_id: "action".into(),
                history_id: "A".into(),
                thread_id: 5,
            }
        );
        assert_eq!(
            crate::LifecycleEvent::from(core::LifecycleEvent::ThreadRelatedChanged {
                history_id: "A".into(),
                thread_id: 5,
                client_id: Some("saved".into()),
            }),
            crate::LifecycleEvent::ThreadRelatedChanged {
                history_id: "A".into(),
                thread_id: 5,
                client_id: Some("saved".into()),
            }
        );
        assert_eq!(
            crate::LifecycleEvent::from(core::LifecycleEvent::ProtocolError {
                detail: "History changed".into(),
                client_id: Some("old".into()),
            }),
            crate::LifecycleEvent::ProtocolError {
                detail: "History changed".into(),
                client_id: Some("old".into()),
            }
        );
    }

    #[test]
    fn message_ffi_keeps_confirmed_send_identity_and_citations() {
        let message =
            crate::ChatMessage::from(core::ChatEntry::Confirmed(core::ConfirmedMessage {
                id: 42,
                thread_id: 5,
                client_id: Some("send-1".into()),
                mentions: vec![9],
                artifact_ids: vec![44],
                author: core::ChatAuthor::Owner,
                body: "Groceries".into(),
                reply_to: None,
                timestamp: "now".into(),
                attachments: vec![],
                tool_calls: vec![],
            }));
        assert_eq!(message.thread_id, 5);
        assert_eq!(message.mentions, vec![9]);
        assert_eq!(message.artifact_ids, vec![44]);
        assert_eq!(message.client_id.as_deref(), Some("send-1"));
        assert!(!message.pending);
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

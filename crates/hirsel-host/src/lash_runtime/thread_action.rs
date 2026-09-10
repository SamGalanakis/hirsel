//! Historical instrument context captured when an Owner action is accepted.
use chrono::{DateTime, Utc};
use hirsel_proto::{Thread, ThreadAttention, ThreadKind};
use serde_json::Value;

/// Persisted requests retain the accepted instrument and lifecycle facts.
/// Inventory execution projections are derived live and do not belong here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadActionSnapshot {
    pub id: u64,
    pub kind: ThreadKind,
    pub title: String,
    pub description: String,
    pub instrument: Value,
    pub attention: ThreadAttention,
    pub settled_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub read: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: u64,
}

impl From<Thread> for ThreadActionSnapshot {
    fn from(thread: Thread) -> Self {
        Self {
            id: thread.id,
            kind: thread.kind,
            title: thread.title,
            description: thread.description,
            instrument: thread.instrument,
            attention: thread.attention,
            settled_at: thread.settled_at,
            archived_at: thread.archived_at,
            snoozed_until: thread.snoozed_until,
            read: thread.read,
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            revision: thread.revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn historical_thread() -> Value {
        json!({
            "id": 5, "kind": "task", "title": "Accepted title", "description": "Accepted description",
            "instrument": {"type": "button", "label": "Advance", "action": "advance"},
            "attention": "needs_owner", "settled_at": null,
            "archived_at": null, "snoozed_until": "2026-09-09T09:00:00Z",
            "read": true, "created_at": "2026-09-08T10:00:00Z",
            "updated_at": "2026-09-09T10:00:00Z", "revision": 7
        })
    }

    #[test]
    fn action_capture_keeps_original_fields_without_live_summary_projections() {
        let original = historical_thread();
        let mut current = original.clone();
        current["running_turn"] = Value::Null;
        current["queued_turn_count"] = json!(3);
        current["last_finished_turn"] = Value::Null;
        current["last_activity_at"] = json!("2026-09-09T12:00:00Z");
        let thread: Thread = serde_json::from_value(current.clone()).unwrap();
        let captured = ThreadActionSnapshot::from(thread);
        assert_eq!(serde_json::to_value(&captured).unwrap(), original);
        assert!(serde_json::from_value::<ThreadActionSnapshot>(current).is_err());
    }
}

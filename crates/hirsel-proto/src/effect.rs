//! Durable facts about the Threads and artifacts one accepted turn touched.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadEffectKind {
    Created,
    SentTo,
    Delegated,
    Read,
    Edited,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadEffectTarget {
    Thread { thread_id: u64 },
    Artifact { artifact_id: u64 },
    Root,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadEffectRefusal {
    pub reason: String,
    pub grant_summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub refusal: Option<ThreadEffectRefusal>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectAction {
    Open { target: ThreadEffectTarget },
    Archive { thread_id: u64 },
    CancelQueued { thread_id: u64, turn_id: u64 },
    Stop { thread_id: u64, turn_id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadEffect {
    pub receipt: ThreadEffectReceipt,
    pub actions: Vec<EffectAction>,
}

#[cfg(test)]
mod tests {
    use super::{EffectAction, ThreadEffectTarget};

    #[test]
    fn effect_targets_and_actions_are_closed_wire_variants() {
        assert_eq!(
            serde_json::to_value(EffectAction::Stop {
                thread_id: 7,
                turn_id: 11,
            })
            .unwrap(),
            serde_json::json!({"kind":"stop","thread_id":7,"turn_id":11})
        );
        assert!(
            serde_json::from_value::<ThreadEffectTarget>(serde_json::json!({
                "kind":"thread", "thread_id":7, "artifact_id":9
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<EffectAction>(serde_json::json!({
                "kind":"archive", "thread_id":7, "turn_id":11
            }))
            .is_err()
        );
    }
}

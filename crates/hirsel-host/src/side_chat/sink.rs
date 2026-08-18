use std::sync::{Arc, atomic::Ordering};

use async_trait::async_trait;
use hirsel_proto::{AgentActivityState, HostToClient, TurnEventKind};
use lash::{TurnActivity, TurnActivitySink};
use tokio::sync::broadcast;

use super::{
    projection::{compact_json, latest_line},
    session::SideChatSession,
};
use crate::BroadcastLog;

pub(super) struct ScopedTurnSink {
    pub(super) session: Arc<SideChatSession>,
    pub(super) broadcaster: broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
}

#[async_trait]
impl TurnActivitySink for ScopedTurnSink {
    async fn emit(&self, activity: TurnActivity) {
        let sc = Some(self.session.sc.clone());
        match activity.event {
            lash::TurnEvent::ModelRequestStarted { .. } => {
                self.publish(HostToClient::AgentActivity {
                    state: AgentActivityState::Thinking,
                    text: Some("thinking".to_string()),
                    sc,
                })
            }
            lash::TurnEvent::AssistantProseDelta { text } => {
                self.publish_turn_event(TurnEventKind::Prose {
                    text: text.to_string(),
                });
                self.publish(HostToClient::AgentActivity {
                    state: AgentActivityState::Thinking,
                    text: latest_line(&text),
                    sc,
                });
            }
            lash::TurnEvent::ReasoningDelta { text } => {
                self.publish_turn_event(TurnEventKind::Reasoning {
                    text: text.to_string(),
                });
                self.publish(HostToClient::AgentActivity {
                    state: AgentActivityState::Thinking,
                    text: latest_line(&text),
                    sc,
                });
            }
            lash::TurnEvent::ToolCallStarted {
                call_id,
                name,
                args,
                ..
            } => {
                self.publish_turn_event(TurnEventKind::ToolStart {
                    id: call_id.unwrap_or_else(|| activity.correlation_id.0.to_string()),
                    name: name.clone(),
                    summary: compact_json(&args),
                });
                self.publish(HostToClient::AgentActivity {
                    state: AgentActivityState::Thinking,
                    text: Some(format!("tool {name}")),
                    sc,
                });
            }
            lash::TurnEvent::ToolCallCompleted {
                call_id,
                name,
                output,
                ..
            } => {
                let ok = output.is_success();
                let summary = serde_json::to_value(&output)
                    .ok()
                    .and_then(|value| compact_json(&value));
                self.publish_turn_event(TurnEventKind::ToolDone {
                    id: call_id.unwrap_or_else(|| activity.correlation_id.0.to_string()),
                    name: name.clone(),
                    ok,
                    summary,
                });
                self.publish(HostToClient::AgentActivity {
                    state: AgentActivityState::Thinking,
                    text: Some(format!("tool {name} completed")),
                    sc,
                });
            }
            lash::TurnEvent::Error { message } => self.publish(HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                text: latest_line(&message),
                sc,
            }),
            _ => {}
        }
    }
}

impl ScopedTurnSink {
    fn publish_turn_event(&self, event: TurnEventKind) {
        let seq = self.session.seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.publish(HostToClient::TurnEvent {
            seq,
            event,
            sc: Some(self.session.sc.clone()),
        });
    }

    fn publish(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }
}

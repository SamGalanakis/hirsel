//! Host-owned pairing of calls that passed the active execution start guard.
use super::*;
use hirsel_proto::{ToolCallSummary, TurnEventKind};

#[derive(Default)]
pub(super) struct ToolTelemetry {
    order: Vec<String>,
    pending: HashMap<String, String>,
    completed: HashMap<String, ToolCallSummary>,
}
impl ToolTelemetry {
    fn start(&mut self, id: &str, name: &str) {
        self.order.push(id.into());
        self.pending.insert(id.into(), name.into());
    }

    fn complete(&mut self, id: &str, name: String, ok: bool) {
        self.completed.insert(
            id.into(),
            ToolCallSummary {
                id: id.into(),
                name,
                ok,
            },
        );
    }

    pub(super) fn summaries(&self) -> Vec<ToolCallSummary> {
        self.order
            .iter()
            .filter_map(|id| self.completed.get(id).cloned())
            .collect()
    }
}
impl BridgeState {
    pub(super) async fn start_tool(
        &self,
        id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut telemetry = self.telemetry.lock().await;
        // Hold SQL authority through durable publication and registration. Cancellation
        // cannot leave an emitted start outside the host's pending registry.
        let storage = self.tools.storage();
        let _guard = storage.execution_guard(&self.caller).await?;
        let published = self.tools.publish_guarded_turn_event(
            &_guard,
            self.caller.thread_id,
            self.caller.turn_id,
            TurnEventKind::ToolStart {
                id: id.into(),
                name: name.into(),
                summary: None,
                input: Some(crate::lash_runtime::bounded_turn_payload(input)),
            },
        );
        drop(_guard);
        if let Err(error) = published {
            self.tools
                .fail_turn_timeline_persistence(self.caller.turn_id, &error)
                .await;
            return Err(error);
        }
        telemetry.start(id, name);
        Ok(())
    }

    pub(super) async fn finish_tool(
        &self,
        id: &str,
        ok: bool,
        summary: Option<String>,
        result: &serde_json::Value,
    ) {
        let mut telemetry = self.telemetry.lock().await;
        let Some(name) = telemetry.pending.get(id).cloned() else {
            return;
        };
        // This invocation has produced its one real result. If timeline
        // persistence fails below, `publish_turn_event` fails the turn; leaving
        // the call pending would later fabricate an "interrupted" replacement
        // for a result we actually received.
        telemetry.pending.remove(id);
        let storage = self.tools.storage();
        // Completion is host telemetry only. Never use this guard to execute a
        // model operation, return a cached receipt or publish a new ToolStart.
        let Ok(_guard) = storage.execution_telemetry_guard(&self.caller).await else {
            return;
        };
        let published = self.tools.publish_guarded_turn_event(
            &_guard,
            self.caller.thread_id,
            self.caller.turn_id,
            TurnEventKind::ToolDone {
                id: id.into(),
                name: name.clone(),
                ok,
                summary,
                result: Some(crate::lash_runtime::bounded_turn_payload(result)),
            },
        );
        drop(_guard);
        if let Err(error) = published {
            self.tools
                .fail_turn_timeline_persistence(self.caller.turn_id, &error)
                .await;
            tracing::warn!(turn_id=self.caller.turn_id, %error, "failed to persist tool completion timeline event");
            return;
        }
        telemetry.complete(id, name, ok);
    }

    pub(super) async fn finish_pending(&self) {
        let ids = self
            .telemetry
            .lock()
            .await
            .pending
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.finish_tool(
                &id,
                false,
                Some("Tool interrupted before a result was received".into()),
                &serde_json::json!({"error":"Tool interrupted before a result was received"}),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_keep_start_order_across_reverse_completion() {
        let mut telemetry = ToolTelemetry::default();
        telemetry.start("A", "read_file");
        telemetry.start("B", "read_file");
        telemetry.complete("B", "read_file".into(), false);
        telemetry.complete("A", "read_file".into(), true);

        assert_eq!(
            telemetry.summaries(),
            vec![
                ToolCallSummary {
                    id: "A".into(),
                    name: "read_file".into(),
                    ok: true,
                },
                ToolCallSummary {
                    id: "B".into(),
                    name: "read_file".into(),
                    ok: false,
                },
            ]
        );
    }
}

//! Host-owned pairing of calls that passed the active execution start guard.
use super::*;
use hirsel_proto::ToolCallSummary;

#[derive(Default)]
pub(super) struct ToolTelemetry {
    order: Vec<String>,
    pending: HashMap<String, (String, serde_json::Value)>,
    completed: HashMap<String, ToolCallSummary>,
}
impl ToolTelemetry {
    fn start(&mut self, id: &str, name: &str, input: &serde_json::Value) {
        self.order.push(id.into());
        self.pending.insert(id.into(), (name.into(), input.clone()));
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
        let published = crate::lash_runtime::TurnIngest::publish_guarded_tool_start(
            &self.tools,
            &_guard,
            (self.caller.thread_id, self.caller.turn_id),
            id,
            name,
            input,
        );
        drop(_guard);
        if let Err(error) = published {
            self.tools
                .fail_turn_timeline_persistence(self.caller.turn_id, &error)
                .await;
            return Err(error);
        }
        telemetry.start(id, name, input);
        Ok(())
    }

    pub(super) async fn finish_tool(
        &self,
        id: &str,
        ok: bool,
        _summary: Option<String>,
        result: &serde_json::Value,
    ) {
        let mut telemetry = self.telemetry.lock().await;
        let Some((name, args)) = telemetry.pending.get(id).cloned() else {
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
        let summary = ToolCallSummary {
            id: id.into(),
            name: name.clone(),
            ok,
        };
        let published = crate::lash_runtime::TurnIngest::publish_guarded_tool_done(
            &self.tools,
            &_guard,
            (self.caller.thread_id, self.caller.turn_id),
            &summary,
            &args,
            result,
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
        drop(telemetry);
        if let Err(error) = crate::lash_runtime::TurnIngest::record_tool_completion(
            &self.tools,
            (self.caller.thread_id, self.caller.turn_id),
            &summary,
        )
        .await
        {
            self.tools
                .fail_turn_timeline_integrity(self.caller.turn_id, &error.to_string())
                .await;
        }
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
        telemetry.start("A", "read_file", &serde_json::json!({}));
        telemetry.start("B", "read_file", &serde_json::json!({}));
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

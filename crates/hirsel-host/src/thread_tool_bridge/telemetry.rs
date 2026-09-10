//! Host-owned pairing of calls that passed the active execution start guard.
use super::*;
use hirsel_proto::{HostToClient, ToolCallSummary, TurnEventKind};

#[derive(Default)]
pub(super) struct ToolTelemetry {
    next_sequence: u64,
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
    pub(super) async fn start_tool(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let mut telemetry = self.telemetry.lock().await;
        // Hold SQL authority through broadcast and registration. Cancellation
        // cannot leave an emitted start outside the host's pending registry.
        let storage = self.tools.storage();
        let _guard = storage.execution_guard(&self.caller).await?;
        let seq = telemetry.next_sequence;
        telemetry.next_sequence += 1;
        self.tools.broadcast(HostToClient::TurnEvent {
            thread_id: self.caller.thread_id,
            turn_id: self.caller.turn_id,
            seq,
            event: TurnEventKind::ToolStart {
                id: id.into(),
                name: name.into(),
                summary: None,
            },
        });
        telemetry.start(id, name);
        Ok(())
    }

    pub(super) async fn finish_tool(&self, id: &str, ok: bool, summary: Option<String>) {
        let mut telemetry = self.telemetry.lock().await;
        let Some(name) = telemetry.pending.get(id).cloned() else {
            return;
        };
        let storage = self.tools.storage();
        // Completion is host telemetry only. Never use this guard to execute a
        // model operation, return a cached receipt or publish a new ToolStart.
        let Ok(_guard) = storage.execution_telemetry_guard(&self.caller).await else {
            telemetry.pending.remove(id);
            return;
        };
        telemetry.pending.remove(id);
        let seq = telemetry.next_sequence;
        telemetry.next_sequence += 1;
        self.tools.broadcast(HostToClient::TurnEvent {
            thread_id: self.caller.thread_id,
            turn_id: self.caller.turn_id,
            seq,
            event: TurnEventKind::ToolDone {
                id: id.into(),
                name: name.clone(),
                ok,
                summary,
            },
        });
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

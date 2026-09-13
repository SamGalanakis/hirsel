use super::*;

pub(super) const TURN_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(80);
pub(super) const TURN_EVENT_BATCH_CHARS: usize = 120;
pub(super) const TURN_EVENT_SUMMARY_CHARS: usize = 120;
pub(super) const TURN_EVENT_CODE_BYTES: usize = 64 * 1024;
pub(super) const TURN_EVENT_PAYLOAD_BYTES: usize = 64 * 1024;
#[cfg(not(test))]
const TIMELINE_COMMIT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const TIMELINE_COMMIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Success cannot overtake the durable Lash observation stream.
#[derive(Clone, Default)]
pub(super) struct TimelineCommitBarrier {
    state: Arc<Mutex<TimelineCommitState>>,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct TimelineCommitState {
    committed: Option<String>,
    failures: HashMap<String, String>,
}

impl TimelineCommitBarrier {
    pub(super) async fn record(&self, turn_id: String) {
        self.state.lock().await.committed = Some(turn_id);
        self.notify.notify_waiters();
    }

    pub(super) async fn fail(&self, turn_id: String, reason: String) {
        self.state
            .lock()
            .await
            .failures
            .entry(turn_id)
            .or_insert(reason);
        self.notify.notify_waiters();
    }

    pub(super) async fn wait(&self, turn_id: &str) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + TIMELINE_COMMIT_TIMEOUT;
        loop {
            let notified = self.notify.notified();
            let state = self.state.lock().await;
            if let Some(reason) = state.failures.get(turn_id) {
                return Err(anyhow::anyhow!(reason.clone()));
            }
            if state.committed.as_deref() == Some(turn_id) {
                return Ok(());
            }
            drop(state);
            tokio::time::timeout_at(deadline, notified)
                .await
                .map_err(|_| anyhow::anyhow!("timed out waiting for durable timeline commit"))?;
        }
    }

    pub(super) async fn clear(&self, turn_id: &str) {
        let mut state = self.state.lock().await;
        if state.committed.as_deref() == Some(turn_id) {
            state.committed = None;
        }
        state.failures.remove(turn_id);
    }
}

/// Thin Host adapter. Projection and persistence belong to `TurnIngest`.
pub(super) fn host_executor_event(
    event: &RemoteSessionObservationEventPayload,
) -> Option<ExecutorEvent> {
    let RemoteSessionObservationEventPayload::TurnActivity { activity } = event else {
        return None;
    };
    match &activity.event {
        RemoteTurnEvent::ModelRequestStarted { .. } => {
            Some(ExecutorEvent::Started { external_id: None })
        }
        RemoteTurnEvent::AssistantProseDelta { text } => {
            Some(ExecutorEvent::Prose { text: text.clone() })
        }
        RemoteTurnEvent::ReasoningDelta { text } => {
            Some(ExecutorEvent::Reasoning { text: text.clone() })
        }
        RemoteTurnEvent::CodeBlockStarted {
            language,
            code,
            graph_key,
        } => {
            let (code, truncated) = clamp_code(code);
            Some(ExecutorEvent::CodeStart {
                id: graph_key.clone(),
                language: language.clone(),
                code,
                truncated,
            })
        }
        RemoteTurnEvent::CodeBlockCompleted {
            success,
            error,
            duration_ms,
            graph_key,
            ..
        } => Some(ExecutorEvent::CodeDone {
            id: graph_key.clone(),
            ok: *success,
            summary: code_done_summary(
                *success,
                error.as_ref().map(|failure| failure.message.as_str()),
                *duration_ms,
            ),
        }),
        RemoteTurnEvent::ToolCallStarted {
            call_id: Some(id),
            name,
            args,
            ..
        } => Some(ExecutorEvent::ToolStart {
            id: id.clone(),
            name: name.clone(),
            args: args.clone(),
        }),
        RemoteTurnEvent::ToolCallCompleted {
            call_id: Some(id),
            name,
            output,
            ..
        } => Some(ExecutorEvent::ToolDone {
            id: id.clone(),
            name: name.clone(),
            ok: tool_output_ok(output),
            output: output.clone(),
        }),
        RemoteTurnEvent::Error { message } => Some(ExecutorEvent::Diagnostic {
            text: message.clone(),
        }),
        _ => None,
    }
}

pub(super) fn tool_call_summaries(output: &lash::TurnOutput) -> Vec<ToolCallSummary> {
    let summaries = output
        .result
        .tool_calls
        .iter()
        .filter(|call| {
            !matches!(
                call.output.outcome,
                lash_core::ToolCallOutcome::Cancelled(_)
            )
        })
        .filter_map(|call| {
            call.call_id.as_ref().map(|id| ToolCallSummary {
                id: id.clone(),
                name: call.tool.clone(),
                ok: call.output.is_success(),
            })
        })
        .collect::<Vec<_>>();
    if !summaries.is_empty() {
        return summaries;
    }
    output
        .activities
        .iter()
        .filter_map(|activity| match &activity.event {
            lash::TurnEvent::ToolCallCompleted {
                call_id: Some(id),
                name,
                output,
                ..
            } if !matches!(output.outcome, lash_core::ToolCallOutcome::Cancelled(_)) => {
                Some(ToolCallSummary {
                    id: id.clone(),
                    name: name.clone(),
                    ok: output.is_success(),
                })
            }
            _ => None,
        })
        .collect()
}

pub(super) fn lash_terminal_projection(
    output: Option<&lash::TurnOutput>,
) -> (
    ExecutorTerminalOutcome,
    Option<String>,
    Vec<ToolCallSummary>,
) {
    let Some(output) = output else {
        return (
            ExecutorTerminalOutcome::Failed {
                reason: "executor stopped without a turn output".into(),
            },
            None,
            Vec::new(),
        );
    };
    let calls = tool_call_summaries(output);
    match &output.result.outcome {
        lash::TurnOutcome::Finished(_) => (
            ExecutorTerminalOutcome::Done,
            output
                .assistant_message()
                .map(str::to_owned)
                .or_else(|| output.final_value().map(render_final_value)),
            calls,
        ),
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. }) => (
            ExecutorTerminalOutcome::Cancelled,
            (!output.result.assistant_output.safe_text.trim().is_empty()).then(|| {
                output
                    .result
                    .assistant_output
                    .safe_text
                    .trim_end()
                    .to_string()
            }),
            calls,
        ),
        lash::TurnOutcome::Stopped(stop) => (
            ExecutorTerminalOutcome::Failed {
                reason: format!("Lash executor stopped: {stop:?}"),
            },
            None,
            calls,
        ),
        lash::TurnOutcome::AgentFrameSwitch { .. } => (
            ExecutorTerminalOutcome::Failed {
                reason: "executor attempted an unsupported agent-frame switch".into(),
            },
            None,
            calls,
        ),
    }
}

#[cfg(test)]
pub(super) fn turn_chat_payload(
    output: &lash::TurnOutput,
) -> Option<(String, Vec<ToolCallSummary>)> {
    let (outcome, text, calls) = lash_terminal_projection(Some(output));
    super::turn_ingest::terminal_projection(outcome, text, calls).1
}

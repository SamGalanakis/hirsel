use super::*;

/// Coalescing window for assistant prose/reasoning deltas.
///
/// lash hands us one `AssistantProseDelta` per provider chunk, so this is the
/// only thing standing between the wire and token-level streaming. It is a
/// legibility/cost tradeoff, not a capability limit: the old 250ms/400-char
/// window delivered a finished paragraph in one frame, which reads as a jump
/// rather than a reply being written. A ~12 frames/second ceiling streams
/// visibly while still amortising JSON framing over a handful of tokens.
pub(super) const TURN_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(80);
pub(super) const TURN_EVENT_BATCH_CHARS: usize = 120;
pub(super) const TURN_EVENT_SUMMARY_CHARS: usize = 120;
/// Agent programs are shown verbatim, so the only cap here is a safety valve
/// against a pathological cell blowing up the ephemeral turn stream.
pub(super) const TURN_EVENT_CODE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct TurnTimelineBridge {
    pub(super) seq: u64,
    pub(super) in_turn: bool,
    pub(super) pending: Option<PendingTimelineText>,
    pub(super) tool_id_seq: u64,
    pub(super) code_id_seq: u64,
}

pub(super) struct PendingTimelineText {
    pub(super) kind: TimelineTextKind,
    pub(super) text: String,
    pub(super) started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineTextKind {
    Prose,
    Reasoning,
}

impl TurnTimelineBridge {
    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn flush_delay(&self) -> Duration {
        self.pending
            .as_ref()
            .map(|pending| TURN_EVENT_BATCH_INTERVAL.saturating_sub(pending.started_at.elapsed()))
            .unwrap_or(TURN_EVENT_BATCH_INTERVAL)
    }

    pub(super) fn observe(
        &mut self,
        event: &RemoteSessionObservationEventPayload,
        broadcast_log: &BroadcastLog,
        broadcaster: &broadcast::Sender<HostToClient>,
    ) {
        match event {
            RemoteSessionObservationEventPayload::TurnActivity { activity } => {
                match &activity.event {
                    RemoteTurnEvent::ModelRequestStarted { .. } => {
                        self.start_turn_if_needed();
                    }
                    RemoteTurnEvent::AssistantProseDelta { text } => {
                        self.push_text(TimelineTextKind::Prose, text, broadcast_log, broadcaster);
                    }
                    RemoteTurnEvent::ReasoningDelta { text } => {
                        self.push_text(
                            TimelineTextKind::Reasoning,
                            text,
                            broadcast_log,
                            broadcaster,
                        );
                    }
                    RemoteTurnEvent::CodeBlockStarted {
                        language,
                        code,
                        graph_key,
                    } => {
                        self.start_turn_if_needed();
                        self.flush_pending(broadcast_log, broadcaster);
                        let id = self.code_event_id(graph_key.as_deref());
                        let (code, truncated) = clamp_code(code);
                        self.publish_event(
                            TurnEventKind::CodeStart {
                                id,
                                language: language.clone(),
                                code,
                                truncated,
                            },
                            broadcast_log,
                            broadcaster,
                        );
                    }
                    RemoteTurnEvent::CodeBlockCompleted {
                        success,
                        error,
                        duration_ms,
                        graph_key,
                        ..
                    } => {
                        self.start_turn_if_needed();
                        self.flush_pending(broadcast_log, broadcaster);
                        let id = self.code_event_id(graph_key.as_deref());
                        self.publish_event(
                            TurnEventKind::CodeDone {
                                id,
                                ok: *success,
                                summary: code_done_summary(
                                    *success,
                                    error.as_deref(),
                                    *duration_ms,
                                ),
                            },
                            broadcast_log,
                            broadcaster,
                        );
                    }
                    RemoteTurnEvent::ToolCallStarted {
                        call_id,
                        name,
                        args,
                        ..
                    } => {
                        self.start_turn_if_needed();
                        self.flush_pending(broadcast_log, broadcaster);
                        let id = self.tool_event_id(call_id.as_deref(), name);
                        self.publish_event(
                            TurnEventKind::ToolStart {
                                id,
                                name: name.clone(),
                                summary: condense_args(name, args),
                            },
                            broadcast_log,
                            broadcaster,
                        );
                    }
                    RemoteTurnEvent::ToolCallCompleted {
                        call_id,
                        name,
                        args,
                        output,
                        ..
                    } => {
                        self.start_turn_if_needed();
                        self.flush_pending(broadcast_log, broadcaster);
                        let id = self.tool_event_id(call_id.as_deref(), name);
                        self.publish_event(
                            TurnEventKind::ToolDone {
                                id,
                                name: name.clone(),
                                ok: tool_output_ok(output),
                                summary: condense_result(name, args, output),
                            },
                            broadcast_log,
                            broadcaster,
                        );
                    }
                    _ => {}
                }
            }
            RemoteSessionObservationEventPayload::Committed => {
                self.finish_turn(broadcast_log, broadcaster);
            }
            _ => {}
        }
    }

    pub(super) fn start_turn_if_needed(&mut self) {
        if !self.in_turn {
            self.seq = 0;
            self.pending = None;
            self.in_turn = true;
            self.tool_id_seq = 0;
            self.code_id_seq = 0;
        }
    }

    /// lash supplies call_id on native tool events; RLM cell executions may
    /// omit it, so fall back to a per-turn ordinal. Started/Completed arrive
    /// serially per call in RLM mode, so name+ordinal pairs stay aligned.
    pub(super) fn tool_event_id(&mut self, call_id: Option<&str>, name: &str) -> String {
        match call_id {
            Some(id) => id.to_string(),
            None => {
                self.tool_id_seq += 1;
                format!("{name}:{}", self.tool_id_seq.div_ceil(2))
            }
        }
    }

    /// Cell executions carry a `graph_key` when lash has one; otherwise pair
    /// started/completed the same way tool events do — serially, on a per-turn
    /// ordinal that both halves of one block resolve to.
    pub(super) fn code_event_id(&mut self, graph_key: Option<&str>) -> String {
        match graph_key {
            Some(key) => format!("code:{key}"),
            None => {
                self.code_id_seq += 1;
                format!("code:{}", self.code_id_seq.div_ceil(2))
            }
        }
    }

    pub(super) fn finish_turn(
        &mut self,
        broadcast_log: &BroadcastLog,
        broadcaster: &broadcast::Sender<HostToClient>,
    ) {
        self.flush_pending(broadcast_log, broadcaster);
        self.in_turn = false;
    }

    pub(super) fn push_text(
        &mut self,
        kind: TimelineTextKind,
        text: &str,
        broadcast_log: &BroadcastLog,
        broadcaster: &broadcast::Sender<HostToClient>,
    ) {
        if text.is_empty() {
            return;
        }
        self.start_turn_if_needed();
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.kind != kind)
        {
            self.flush_pending(broadcast_log, broadcaster);
        }
        let pending = self.pending.get_or_insert_with(|| PendingTimelineText {
            kind,
            text: String::new(),
            started_at: Instant::now(),
        });
        pending.text.push_str(text);
        if pending.text.chars().count() >= TURN_EVENT_BATCH_CHARS
            || pending.started_at.elapsed() >= TURN_EVENT_BATCH_INTERVAL
        {
            self.flush_pending(broadcast_log, broadcaster);
        }
    }

    pub(super) fn flush_pending(
        &mut self,
        broadcast_log: &BroadcastLog,
        broadcaster: &broadcast::Sender<HostToClient>,
    ) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if pending.text.is_empty() {
            return;
        }
        let event = match pending.kind {
            TimelineTextKind::Prose => TurnEventKind::Prose { text: pending.text },
            TimelineTextKind::Reasoning => TurnEventKind::Reasoning { text: pending.text },
        };
        self.publish_event(event, broadcast_log, broadcaster);
    }

    pub(super) fn publish_event(
        &mut self,
        event: TurnEventKind,
        broadcast_log: &BroadcastLog,
        broadcaster: &broadcast::Sender<HostToClient>,
    ) {
        self.seq += 1;
        publish(
            broadcast_log,
            broadcaster,
            HostToClient::TurnEvent {
                seq: self.seq,
                event,
                sc: None,
            },
        );
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
        .map(|call| ToolCallSummary {
            name: call.tool.clone(),
            ok: call.output.is_success(),
        })
        .collect::<Vec<_>>();
    if !summaries.is_empty() {
        return summaries;
    }
    output
        .activities
        .iter()
        .filter_map(|activity| match &activity.event {
            lash::TurnEvent::ToolCallCompleted { name, output, .. }
                if !matches!(output.outcome, lash_core::ToolCallOutcome::Cancelled(_)) =>
            {
                Some(ToolCallSummary {
                    name: name.clone(),
                    ok: output.is_success(),
                })
            }
            _ => None,
        })
        .collect()
}

pub(super) fn turn_chat_payload(
    output: &lash::TurnOutput,
) -> Option<(String, Vec<ToolCallSummary>)> {
    let tool_calls = tool_call_summaries(output);
    match &output.result.outcome {
        lash::TurnOutcome::Finished(_) => {
            let text = output
                .assistant_message()
                .map(str::to_owned)
                .or_else(|| output.final_value().map(render_final_value))
                .unwrap_or_default();
            (!text.trim().is_empty() || !tool_calls.is_empty()).then_some((text, tool_calls))
        }
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. }) => {
            // Lash recovers checkpointed assistant prose here; raw provider
            // deltas remain activity-only and are intentionally not materialized.
            let text = output.result.assistant_output.safe_text.trim_end();
            if text.trim().is_empty() && tool_calls.is_empty() {
                None
            } else if text.trim().is_empty() {
                Some(("— interrupted".to_string(), tool_calls))
            } else {
                Some((format!("{text}\n\n— interrupted"), tool_calls))
            }
        }
        lash::TurnOutcome::AgentFrameSwitch { .. } | lash::TurnOutcome::Stopped(_) => None,
    }
}

pub(super) async fn materialize_turn_chat(
    tools: &ToolSuite,
    output: &lash::TurnOutput,
) -> anyhow::Result<bool> {
    let Some((text, tool_calls)) = turn_chat_payload(output) else {
        return Ok(false);
    };
    tools
        .chat_send_with_tool_calls(text, None, tool_calls)
        .await?;
    Ok(true)
}

//! The one host-owned projection of executor events into durable turn state.

use super::*;
use hirsel_proto::ThreadTurnState;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExecutorEvent {
    Started {
        external_id: Option<String>,
    },
    Prose {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolStart {
        id: String,
        name: String,
        args: Value,
    },
    ToolDone {
        id: String,
        name: String,
        ok: bool,
        output: Value,
    },
    CodeStart {
        id: Option<String>,
        language: String,
        code: String,
        truncated: bool,
    },
    CodeDone {
        id: Option<String>,
        ok: bool,
        summary: Option<String>,
    },
    Diagnostic {
        text: String,
    },
    Final {
        text: String,
    },
    Terminal {
        outcome: ExecutorTerminalOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutorTerminalOutcome {
    Done,
    Failed { reason: String },
    Interrupted,
    Cancelled,
}

pub(crate) struct TurnIngest {
    history_id: String,
    pub(super) thread_id: Option<u64>,
    pub(super) turn_id: Option<u64>,
    provenance: Value,
    pending: Option<PendingText>,
    code_id_seq: u64,
    started: bool,
    diagnostic_recorded: bool,
    started_tools: HashMap<String, (String, Value)>,
    completed_tools: HashSet<String>,
    tool_calls: Vec<ToolCallSummary>,
    final_text: Option<String>,
    terminal: Option<ExecutorTerminalOutcome>,
}

struct PendingText {
    kind: TextKind,
    text: String,
    started_at: Instant,
}

type TerminalProjection = (
    ThreadTurnState,
    Option<(String, Vec<ToolCallSummary>)>,
    Option<String>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Prose,
    Reasoning,
}

impl TurnIngest {
    pub(super) fn new(history_id: &str, thread_id: u64, turn_id: u64, provenance: Value) -> Self {
        let mut ingest = Self::unrouted(history_id, provenance);
        ingest.thread_id = Some(thread_id);
        ingest.turn_id = Some(turn_id);
        ingest
    }

    pub(super) fn unrouted(history_id: &str, provenance: Value) -> Self {
        Self {
            history_id: history_id.into(),
            thread_id: None,
            turn_id: None,
            provenance,
            pending: None,
            code_id_seq: 0,
            started: false,
            diagnostic_recorded: false,
            started_tools: HashMap::new(),
            completed_tools: HashSet::new(),
            tool_calls: Vec::new(),
            final_text: None,
            terminal: None,
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn flush_delay(&self) -> Duration {
        self.pending
            .as_ref()
            .map(|pending| TURN_EVENT_BATCH_INTERVAL.saturating_sub(pending.started_at.elapsed()))
            .unwrap_or(TURN_EVENT_BATCH_INTERVAL)
    }

    pub(super) async fn reroute(
        &mut self,
        tools: &ToolSuite,
        thread_id: Option<u64>,
        turn_id: Option<u64>,
    ) -> anyhow::Result<()> {
        if self.thread_id == thread_id && self.turn_id == turn_id {
            return Ok(());
        }
        self.flush(tools).await?;
        self.thread_id = thread_id;
        self.turn_id = turn_id;
        self.reset_turn_state();
        Ok(())
    }

    pub(super) async fn accept(
        &mut self,
        tools: &ToolSuite,
        event: ExecutorEvent,
    ) -> anyhow::Result<()> {
        match event {
            ExecutorEvent::Started { external_id } => {
                if self.started {
                    return Ok(());
                }
                self.started = true;
                let mut data = self.provenance.clone();
                if !data.is_object() {
                    data = json!({"executor":data});
                }
                if let Some(external_id) = external_id {
                    data.as_object_mut()
                        .expect("provenance was normalized to an object")
                        .insert("external_id".into(), Value::String(external_id));
                }
                self.record_activity_once(tools, "execution_started", data)
                    .await?;
                self.publish_activity(tools, AgentActivityState::Thinking, Some("thinking".into()));
            }
            ExecutorEvent::Prose { text } => {
                let label = latest_line(&text);
                self.push_text(tools, TextKind::Prose, text).await?;
                self.publish_activity(tools, AgentActivityState::Thinking, label);
            }
            ExecutorEvent::Reasoning { text } => {
                let label = latest_line(&text);
                self.push_text(tools, TextKind::Reasoning, text).await?;
                self.publish_activity(tools, AgentActivityState::Thinking, label);
            }
            ExecutorEvent::ToolStart { id, name, args } => {
                if is_scoped_bridge_tool(&name) {
                    return Ok(());
                }
                if let Some((old_name, old_args)) = self.started_tools.get(&id) {
                    anyhow::ensure!(
                        old_name == &name && old_args == &args,
                        "executor reused a tool call id with different input"
                    );
                    return Ok(());
                }
                self.flush(tools).await?;
                self.publish_timeline(
                    tools,
                    TurnEventKind::ToolStart {
                        id: id.clone(),
                        name: name.clone(),
                        summary: condense_args(&name, &args),
                        input: Some(bounded_turn_payload(&args)),
                    },
                )
                .await?;
                self.started_tools.insert(id, (name.clone(), args));
                self.publish_activity(
                    tools,
                    AgentActivityState::Thinking,
                    Some(format!("tool {name}")),
                );
            }
            ExecutorEvent::ToolDone {
                id,
                name,
                ok,
                output,
            } => {
                if is_scoped_bridge_tool(&name) || self.completed_tools.contains(&id) {
                    return Ok(());
                }
                let args = match self.started_tools.get(&id) {
                    Some((started_name, args)) => {
                        anyhow::ensure!(
                            started_name == &name,
                            "executor completed a tool call with another name"
                        );
                        args.clone()
                    }
                    None => Value::Null,
                };
                self.flush(tools).await?;
                self.publish_timeline(
                    tools,
                    TurnEventKind::ToolDone {
                        id: id.clone(),
                        name: name.clone(),
                        ok,
                        summary: condense_result_with_status(&name, &args, &output, ok),
                        result: Some(bounded_turn_payload(&output)),
                    },
                )
                .await?;
                let summary = ToolCallSummary {
                    id: id.clone(),
                    name: name.clone(),
                    ok,
                };
                Self::record_tool_completion(tools, &self.history_id, self.route()?, &summary)
                    .await?;
                self.completed_tools.insert(id);
                self.tool_calls.push(summary);
                self.publish_activity(
                    tools,
                    AgentActivityState::Thinking,
                    Some(format!("tool {name} completed")),
                );
            }
            ExecutorEvent::CodeStart {
                id,
                language,
                code,
                truncated,
            } => {
                self.flush(tools).await?;
                let id = self.code_event_id(id.as_deref());
                self.publish_timeline(
                    tools,
                    TurnEventKind::CodeStart {
                        id,
                        language,
                        code,
                        truncated,
                    },
                )
                .await?;
                self.publish_activity(
                    tools,
                    AgentActivityState::Thinking,
                    Some("running code".into()),
                );
            }
            ExecutorEvent::CodeDone { id, ok, summary } => {
                self.flush(tools).await?;
                let id = self.code_event_id(id.as_deref());
                self.publish_timeline(tools, TurnEventKind::CodeDone { id, ok, summary })
                    .await?;
            }
            ExecutorEvent::Diagnostic { text } => {
                if !self.diagnostic_recorded {
                    self.diagnostic_recorded = true;
                    self.record_activity_once(
                        tools,
                        "execution_diagnostic",
                        json!({"summary":text}),
                    )
                    .await?;
                }
            }
            ExecutorEvent::Final { text } => {
                anyhow::ensure!(self.final_text.is_none(), "duplicate final executor output");
                self.final_text = Some(text);
            }
            ExecutorEvent::Terminal { outcome } => {
                self.flush(tools).await?;
                anyhow::ensure!(
                    self.terminal.is_none(),
                    "duplicate executor terminal outcome"
                );
                self.terminal = Some(outcome);
            }
        }
        Ok(())
    }

    pub(super) async fn flush(&mut self, tools: &ToolSuite) -> anyhow::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if pending.text.is_empty() {
            return Ok(());
        }
        let event = match pending.kind {
            TextKind::Prose => TurnEventKind::Prose { text: pending.text },
            TextKind::Reasoning => TurnEventKind::Reasoning { text: pending.text },
        };
        self.publish_timeline(tools, event).await
    }

    pub(super) fn tool_calls(&self) -> &[ToolCallSummary] {
        &self.tool_calls
    }

    pub(super) fn final_text(&self) -> Option<&str> {
        self.final_text.as_deref()
    }

    pub(super) fn terminal(&self) -> Option<&ExecutorTerminalOutcome> {
        self.terminal.as_ref()
    }

    pub(super) async fn complete(
        tools: &ToolSuite,
        history_id: &str,
        turn_id: u64,
        outcome: ExecutorTerminalOutcome,
        final_text: Option<String>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<hirsel_proto::ThreadTurn> {
        Self::complete_after_commit(
            tools,
            history_id,
            turn_id,
            outcome,
            final_text,
            tool_calls,
            || async { Ok(()) },
        )
        .await
    }

    pub(super) async fn complete_after_commit<F, Fut>(
        tools: &ToolSuite,
        history_id: &str,
        turn_id: u64,
        outcome: ExecutorTerminalOutcome,
        final_text: Option<String>,
        tool_calls: Vec<ToolCallSummary>,
        after_commit: F,
    ) -> anyhow::Result<hirsel_proto::ThreadTurn>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        let integrity_failure = tools.turn_timeline_integrity_failure(turn_id);
        if integrity_failure.is_none() {
            let turn = tools.storage().thread_turn(turn_id).await?;
            for tool in &tool_calls {
                Self::record_tool_completion(tools, history_id, (turn.thread_id, turn_id), tool)
                    .await?;
            }
        }
        let (state, output, reason) = if let Some(reason) = integrity_failure.as_ref() {
            (ThreadTurnState::Failed, None, Some(reason.clone()))
        } else {
            terminal_projection(outcome, final_text, tool_calls)
        };
        let completion = tools
            .storage()
            .complete_thread_turn_with_failure(
                history_id,
                turn_id,
                state,
                output,
                reason.as_deref(),
            )
            .await?;
        if integrity_failure.is_some() {
            anyhow::ensure!(
                completion.turn.state == ThreadTurnState::Failed,
                "timeline integrity failure lost to an earlier terminal projection"
            );
            tools.clear_turn_timeline_integrity_failure(turn_id);
        }
        after_commit().await?;
        Self::publish_idle(tools, completion.turn.thread_id, turn_id);
        if let Some(activity) = completion.failure_activity {
            tools.publish_thread_activity(activity).await;
        }
        if let Some(message) = completion.message {
            tools.publish_thread_message(message).await;
        }
        tools.publish_thread_turn(completion.turn.clone()).await;
        Ok(completion.turn)
    }

    pub(crate) async fn record_tool_completion(
        tools: &ToolSuite,
        history_id: &str,
        (thread_id, turn_id): (u64, u64),
        tool: &ToolCallSummary,
    ) -> anyhow::Result<()> {
        let (activity, inserted) = tools
            .storage()
            .append_thread_activity_once(
                history_id,
                &format!("turn:{turn_id}:tool:{}", tool.id),
                thread_id,
                Some(turn_id),
                "tool_completed",
                &serde_json::to_value(tool)?,
            )
            .await?;
        if inserted {
            tools.publish_thread_activity(activity).await;
        }
        tools.publish_thread_effects(turn_id).await?;
        Ok(())
    }

    pub(crate) fn publish_guarded_tool_start(
        tools: &ToolSuite,
        guard: &tokio::sync::MutexGuard<'_, rusqlite::Connection>,
        (thread_id, turn_id): (u64, u64),
        id: &str,
        name: &str,
        input: &Value,
    ) -> anyhow::Result<()> {
        Self::publish_guarded_timeline(
            tools,
            guard,
            (thread_id, turn_id),
            TurnEventKind::ToolStart {
                id: id.into(),
                name: name.into(),
                summary: condense_args(name, input),
                input: Some(bounded_turn_payload(input)),
            },
        )?;
        Ok(())
    }

    pub(crate) fn publish_guarded_tool_done(
        tools: &ToolSuite,
        guard: &tokio::sync::MutexGuard<'_, rusqlite::Connection>,
        (thread_id, turn_id): (u64, u64),
        tool: &ToolCallSummary,
        args: &Value,
        result: &Value,
    ) -> anyhow::Result<()> {
        Self::publish_guarded_timeline(
            tools,
            guard,
            (thread_id, turn_id),
            TurnEventKind::ToolDone {
                id: tool.id.clone(),
                name: tool.name.clone(),
                ok: tool.ok,
                summary: condense_result_with_status(&tool.name, args, result, tool.ok),
                result: Some(bounded_turn_payload(result)),
            },
        )?;
        Ok(())
    }

    fn publish_guarded_timeline(
        tools: &ToolSuite,
        guard: &tokio::sync::MutexGuard<'_, rusqlite::Connection>,
        (thread_id, turn_id): (u64, u64),
        event: TurnEventKind,
    ) -> anyhow::Result<()> {
        tools.publish_guarded_turn_event(guard, thread_id, turn_id, event)?;
        Ok(())
    }

    fn reset_turn_state(&mut self) {
        self.pending = None;
        self.code_id_seq = 0;
        self.started = false;
        self.diagnostic_recorded = false;
        self.started_tools.clear();
        self.completed_tools.clear();
        self.tool_calls.clear();
        self.final_text = None;
        self.terminal = None;
    }

    async fn push_text(
        &mut self,
        tools: &ToolSuite,
        kind: TextKind,
        text: String,
    ) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.kind != kind)
        {
            self.flush(tools).await?;
        }
        let pending = self.pending.get_or_insert_with(|| PendingText {
            kind,
            text: String::new(),
            started_at: Instant::now(),
        });
        pending.text.push_str(&text);
        if pending.text.chars().count() >= TURN_EVENT_BATCH_CHARS
            || pending.started_at.elapsed() >= TURN_EVENT_BATCH_INTERVAL
        {
            self.flush(tools).await?;
        }
        Ok(())
    }

    async fn publish_timeline(
        &self,
        tools: &ToolSuite,
        event: TurnEventKind,
    ) -> anyhow::Result<()> {
        let (thread_id, turn_id) = self.route()?;
        tools.publish_turn_event(thread_id, turn_id, event).await?;
        Ok(())
    }

    async fn record_activity_once(
        &self,
        tools: &ToolSuite,
        kind: &str,
        data: Value,
    ) -> anyhow::Result<()> {
        let (thread_id, turn_id) = self.route()?;
        let (activity, inserted) = tools
            .storage()
            .append_thread_activity_once(
                &self.history_id,
                &format!("turn:{turn_id}:{kind}"),
                thread_id,
                Some(turn_id),
                kind,
                &data,
            )
            .await?;
        if inserted {
            tools.publish_thread_activity(activity).await;
        }
        Ok(())
    }

    fn publish_activity(&self, tools: &ToolSuite, state: AgentActivityState, text: Option<String>) {
        let Ok((thread_id, turn_id)) = self.route() else {
            return;
        };
        Self::publish_activity_for_route(tools, thread_id, turn_id, state, text);
    }

    pub(super) fn publish_idle(tools: &ToolSuite, thread_id: u64, turn_id: u64) {
        Self::publish_activity_for_route(tools, thread_id, turn_id, AgentActivityState::Idle, None);
    }

    fn publish_activity_for_route(
        tools: &ToolSuite,
        thread_id: u64,
        turn_id: u64,
        state: AgentActivityState,
        text: Option<String>,
    ) {
        tools.broadcast(HostToClient::AgentActivity {
            thread_id,
            turn_id,
            state,
            text,
        });
    }

    fn route(&self) -> anyhow::Result<(u64, u64)> {
        self.thread_id
            .zip(self.turn_id)
            .ok_or_else(|| anyhow::anyhow!("executor event has no owning turn"))
    }

    fn code_event_id(&mut self, source: Option<&str>) -> String {
        match source {
            Some(key) => format!("code:{key}"),
            None => {
                self.code_id_seq += 1;
                format!("code:{}", self.code_id_seq.div_ceil(2))
            }
        }
    }
}

pub(super) fn terminal_projection(
    outcome: ExecutorTerminalOutcome,
    final_text: Option<String>,
    tool_calls: Vec<ToolCallSummary>,
) -> TerminalProjection {
    match outcome {
        ExecutorTerminalOutcome::Done => (
            ThreadTurnState::Completed,
            final_output(final_text, tool_calls, false),
            None,
        ),
        ExecutorTerminalOutcome::Failed { reason } => (
            ThreadTurnState::Failed,
            final_text
                .filter(|text| !text.trim().is_empty())
                .map(|text| (text, tool_calls)),
            Some(reason),
        ),
        ExecutorTerminalOutcome::Interrupted => (
            ThreadTurnState::Interrupted,
            final_output(final_text, tool_calls, true),
            None,
        ),
        ExecutorTerminalOutcome::Cancelled => (
            ThreadTurnState::Cancelled,
            final_output(final_text, tool_calls, true),
            None,
        ),
    }
}

fn final_output(
    final_text: Option<String>,
    tool_calls: Vec<ToolCallSummary>,
    interrupted: bool,
) -> Option<(String, Vec<ToolCallSummary>)> {
    let mut text = final_text.unwrap_or_default();
    if interrupted {
        if text.trim().is_empty() && !tool_calls.is_empty() {
            text = "— interrupted".to_string();
        } else if !text.trim().is_empty() {
            text = format!("{}\n\n— interrupted", text.trim_end());
        }
    }
    (!text.trim().is_empty() || !tool_calls.is_empty()).then_some((text, tool_calls))
}

fn is_scoped_bridge_tool(name: &str) -> bool {
    name.starts_with("mcp__hirsel__")
}

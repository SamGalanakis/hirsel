//! A fresh provider CLI session for one accepted Thread input.
use super::*;
use hirsel_drivers::{ScopedMcpLaunch, SessionHandle, SpawnSpec, SubagentDriver, SubagentEvent};
use hirsel_proto::ThreadTurnState;
use tokio_util::sync::CancellationToken;

fn is_scoped_bridge_tool(name: &str) -> bool {
    name.starts_with("mcp__hirsel__")
}

fn is_execution_diagnostic(summary: &str) -> bool {
    let summary = summary.to_ascii_lowercase();
    [
        "configuration warning",
        "rate limit",
        "unparsed ",
        "unsupported codex server request",
    ]
    .iter()
    .any(|marker| summary.contains(marker))
}

pub(super) struct CliTurn {
    pub(super) turn_id: u64,
    pub(super) cancel: CancellationToken,
    driver: Arc<dyn SubagentDriver>,
    handle: Mutex<Option<SessionHandle>>,
}
impl CliTurn {
    pub(super) fn new(turn_id: u64, driver: Arc<dyn SubagentDriver>) -> Arc<Self> {
        Arc::new(Self {
            turn_id,
            cancel: CancellationToken::new(),
            driver,
            handle: Mutex::new(None),
        })
    }
    pub(super) async fn stop(&self) {
        self.cancel.cancel();
        let handle = self.handle.lock().await.take();
        if let Some(handle) = handle {
            let _ = self.driver.retire(&handle).await;
        }
    }
    pub(super) async fn run(
        &self,
        tools: &ToolSuite,
        request: OwnerTurn,
        execution: crate::storage::ThreadExecution,
        capacity: Arc<tokio::sync::Semaphore>,
    ) -> anyhow::Result<()> {
        let mut output = None;
        let mut tool_calls = Vec::new();
        let result = self
            .execute(
                tools,
                &request,
                execution,
                capacity,
                &mut output,
                &mut tool_calls,
            )
            .await;
        // Provider lifetime ends independently of terminal storage availability.
        self.stop().await;
        let integrity_failure = tools.turn_timeline_integrity_failure(self.turn_id);
        let (state, reason) = match integrity_failure.clone() {
            Some(reason) => (ThreadTurnState::Failed, Some(reason)),
            None => match result {
                Ok(TerminalOutcome::Done { .. }) => (ThreadTurnState::Completed, None),
                Ok(TerminalOutcome::Interrupted) => (ThreadTurnState::Cancelled, None),
                Ok(TerminalOutcome::Failed { reason }) => (ThreadTurnState::Failed, Some(reason)),
                Err(error) => (ThreadTurnState::Failed, Some(error.to_string())),
            },
        };
        // Do not synthesize a final assistant message from a process error/summary.
        let output = if integrity_failure.is_some() {
            None
        } else {
            output
                .map(|text| (text, tool_calls.clone()))
                .or_else(|| (!tool_calls.is_empty()).then_some((String::new(), tool_calls)))
        };
        let mut delay = Duration::from_millis(50);
        loop {
            match tools
                .storage()
                .complete_thread_turn_with_failure(
                    &request.history_id,
                    self.turn_id,
                    state,
                    output.clone(),
                    reason.as_deref(),
                )
                .await
            {
                Ok(completion) => {
                    if integrity_failure.is_some() {
                        anyhow::ensure!(
                            completion.turn.state == ThreadTurnState::Failed,
                            "timeline integrity failure lost to an earlier terminal projection"
                        );
                        tools.clear_turn_timeline_integrity_failure(self.turn_id);
                    }
                    if let Some(activity) = completion.failure_activity {
                        tools.publish_thread_activity(activity).await;
                    }
                    if let Some(message) = completion.message {
                        tools.publish_thread_message(message).await;
                    }
                    tools.publish_thread_turn(completion.turn).await;
                    return Ok(());
                }
                Err(error) => {
                    if let Ok(history) = tools.storage().history_id().await {
                        anyhow::ensure!(
                            history == request.history_id,
                            "CLI completion belongs to a previous history"
                        );
                    }
                    tracing::warn!(turn_id=self.turn_id, %error, "Retrying durable CLI terminal delivery");
                    // The history generation owns this task and aborts/drains it
                    // on reset or shutdown. User cancellation still needs its
                    // terminal committed, so the provider cancel flag is not a
                    // reason to discard pending output here.
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(2));
                }
            }
        }
    }
    async fn recover_final_output(
        &self,
        events: &mut hirsel_drivers::EventStream,
        output: &mut Option<String>,
    ) {
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = events.next().await {
                match event {
                    SubagentEvent::AssistantOutput { text } if output.is_none() => {
                        *output = Some(text)
                    }
                    SubagentEvent::Terminal { .. } => break,
                    _ => {}
                }
            }
        })
        .await;
    }
    async fn execute(
        &self,
        tools: &ToolSuite,
        request: &OwnerTurn,
        execution: crate::storage::ThreadExecution,
        capacity: Arc<tokio::sync::Semaphore>,
        output: &mut Option<String>,
        tool_calls: &mut Vec<hirsel_proto::ToolCallSummary>,
    ) -> anyhow::Result<TerminalOutcome> {
        let _permit = tokio::select! {
            _=self.cancel.cancelled()=>return Ok(TerminalOutcome::Interrupted),
            permit=capacity.acquire()=>permit?,
        };
        let accepted = request.stored_turn(&tools.storage()).await?;
        anyhow::ensure!(
            accepted.state == ThreadTurnState::Queued,
            "CLI input is no longer queued"
        );
        let turn = tools.storage().run_thread_turn(self.turn_id).await?;
        tools.publish_thread_turn(turn).await;
        let mut bridge = crate::thread_tool_bridge::ThreadToolBridge::start(
            tools.clone(),
            &request.history_id,
            self.turn_id,
        )
        .await?;
        let context = tools.storage().thread_context(&bridge.caller).await?;
        let detail = tools
            .storage()
            .scoped_thread_read(
                &bridge.caller,
                &crate::storage::ThreadRef::default(),
                None,
                60,
            )
            .await?;
        let history = detail
            .messages
            .into_iter()
            .filter(|m| request.message_id.is_none_or(|id| m.id < id))
            .collect::<Vec<_>>();
        let current_artifacts = tools
            .storage()
            .accepted_message_references(&request.history_id, self.turn_id)
            .await?;
        let mut prompt = format!(
            "You execute one accepted Hirsel Thread turn. Use the supplied scoped Thread tools for coordination. Only this Thread and its descendants are visible; create/delegate focused children and report upward. Parent/peer transcripts are not available. Artifacts require explicit creation and references.\n\nIdentity and accepted brief:\n{}\n\nThis Thread's recent conversation:\n{}\n\nAccepted input:\n{}",
            serde_json::to_string(&context)?,
            serde_json::to_string(&history)?,
            request.body
        );
        prompt.push_str(&format!(
            "\n[Current accepted message artifact references]\n{}",
            serde_json::to_string(&current_artifacts)?
        ));
        let crate::storage::ThreadExecution::Cli {
            agent,
            model,
            variant,
            cwd,
        } = execution
        else {
            anyhow::bail!("CLI execution settings required")
        };
        let spec = SpawnSpec {
            agent,
            model: Some(model),
            variant: Some(variant),
            prompt,
            cwd,
            fake_fixture: tools.driver_fixture(),
            scoped_mcp: ScopedMcpLaunch {
                host_executable: std::env::current_exe()?,
                socket_path: bridge.socket_path.clone(),
                capability_file: bridge.capability_file.clone(),
                expected_tools: bridge.expected_tools.clone(),
            },
        };
        let mut started_tools = HashMap::<String, (String, Value)>::new();
        let mut completed_tools = HashSet::<String>::new();
        let mut cli_tool_calls = Vec::<hirsel_proto::ToolCallSummary>::new();
        let mut persisted_diagnostic = false;
        let result=async {
        let handle = self.driver.spawn(spec).await?;
        *self.handle.lock().await = Some(handle.clone());
        let mut events = self.driver.events(&handle)?;
        loop {
            let event = tokio::select! {
                _=self.cancel.cancelled()=>{let _=self.driver.interrupt(&handle).await;self.recover_final_output(&mut events,output).await;return Ok(TerminalOutcome::Interrupted);},
                _=bridge.invalidated.cancelled()=>{let _=self.driver.interrupt(&handle).await;self.recover_final_output(&mut events,output).await;return Ok(TerminalOutcome::Failed{reason:"Thread tool bridge restarted after an uncertain invocation".into()});},
                event=events.next()=>event,
            };
            match event {
                Some(SubagentEvent::AssistantOutput { text }) => {
                    anyhow::ensure!(output.is_none(), "duplicate final CLI output");
                    *output = Some(text);
                }
                Some(SubagentEvent::Terminal { outcome }) => return Ok(outcome),
                Some(SubagentEvent::Started { external_id }) => {
                    let activity = tools
                        .storage()
                        .append_thread_activity(
                            request.thread_id,
                            Some(self.turn_id),
                            "execution_started",
                            &json!({"agent":agent,"external_id":external_id}),
                        )
                        .await?;
                    tools.publish_thread_activity(activity).await;
                }
                Some(SubagentEvent::ToolStarted {
                    call_id,
                    name,
                    args,
                }) => {
                    if is_scoped_bridge_tool(&name) {
                        continue;
                    }
                    if let Some((previous_name, previous_args)) = started_tools.get(&call_id) {
                        anyhow::ensure!(
                            previous_name == &name && previous_args == &args,
                            "CLI reused a tool call id with different input"
                        );
                        continue;
                    }
                    tools
                        .publish_turn_event(
                            request.thread_id,
                            self.turn_id,
                            hirsel_proto::TurnEventKind::ToolStart {
                                id: call_id.clone(),
                                name: name.clone(),
                                summary: condense_args(&name, &args),
                                input: Some(bounded_turn_payload(&args)),
                            },
                        )
                        .await?;
                    started_tools.insert(call_id, (name, args));
                }
                Some(SubagentEvent::ToolCompleted {
                    call_id,
                    name,
                    ok,
                    output: tool_output,
                }) => {
                    if is_scoped_bridge_tool(&name) || completed_tools.contains(&call_id) {
                        continue;
                    }
                    let args = match started_tools.get(&call_id) {
                        Some((started_name, args)) => {
                            anyhow::ensure!(
                                started_name == &name,
                                "CLI completed a tool call with another name"
                            );
                            args.clone()
                        }
                        None => Value::Null,
                    };
                    tools
                        .publish_turn_event(
                            request.thread_id,
                            self.turn_id,
                            hirsel_proto::TurnEventKind::ToolDone {
                                id: call_id.clone(),
                                name: name.clone(),
                                ok,
                                summary: condense_result_with_status(
                                    &name,
                                    &args,
                                    &tool_output,
                                    ok,
                                ),
                                result: Some(bounded_turn_payload(&tool_output)),
                            },
                        )
                        .await?;
                    let summary = hirsel_proto::ToolCallSummary {
                        id: call_id.clone(),
                        name,
                        ok,
                    };
                    persist_tool_call_summaries(
                        tools,
                        request.thread_id,
                        self.turn_id,
                        std::slice::from_ref(&summary),
                    )
                    .await?;
                    completed_tools.insert(call_id);
                    cli_tool_calls.push(summary);
                }
                Some(SubagentEvent::Progress { summary }) => {
                    if persisted_diagnostic || !is_execution_diagnostic(&summary) {
                        continue;
                    }
                    let activity = tools
                        .storage()
                        .append_thread_activity(
                            request.thread_id,
                            Some(self.turn_id),
                            "execution_progress",
                            &json!({"summary":summary}),
                        )
                        .await?;
                    tools.publish_thread_activity(activity).await;
                    persisted_diagnostic = true;
                }
                None => {
                    return Ok(TerminalOutcome::Failed {
                        reason: "CLI closed without a terminal outcome".into(),
                    });
                }
            }
        }
        }.await;
        bridge.finish().await;
        let bridge_tool_calls = bridge.tool_calls().await;
        persist_tool_call_summaries(tools, request.thread_id, self.turn_id, &bridge_tool_calls)
            .await?;
        cli_tool_calls.extend(
            bridge_tool_calls
                .into_iter()
                .filter(|call| completed_tools.insert(call.id.clone())),
        );
        *tool_calls = cli_tool_calls;
        result
    }
}

#[cfg(test)]
#[path = "cli_delivery_tests.rs"]
mod delivery_tests;

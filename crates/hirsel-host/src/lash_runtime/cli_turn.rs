//! A fresh provider CLI session for one accepted Thread input.
use super::*;
use hirsel_drivers::{ScopedMcpLaunch, SessionHandle, SpawnSpec, SubagentDriver, SubagentEvent};
use hirsel_proto::ThreadTurnState;
use tokio_util::sync::CancellationToken;

fn is_execution_diagnostic(summary: &str) -> bool {
    let summary = summary.to_ascii_lowercase();
    [
        "configuration warning",
        "rate limit",
        "unparsed ",
        "unsupported codex server request",
        "unknown call",
        "reused id",
    ]
    .iter()
    .any(|marker| summary.contains(marker))
}

pub(super) fn cli_executor_event(event: SubagentEvent) -> Option<ExecutorEvent> {
    match event {
        SubagentEvent::Started { external_id } => Some(ExecutorEvent::Started {
            external_id: Some(external_id),
        }),
        SubagentEvent::ProseDelta { text, block_id } => {
            Some(ExecutorEvent::Prose { text, block_id })
        }
        SubagentEvent::ReasoningDelta { text, block_id } => {
            Some(ExecutorEvent::Reasoning { text, block_id })
        }
        SubagentEvent::Progress { summary } if is_execution_diagnostic(&summary) => {
            Some(ExecutorEvent::Diagnostic { text: summary })
        }
        SubagentEvent::Progress { .. } => None,
        SubagentEvent::ToolStarted {
            call_id,
            name,
            args,
        } => Some(ExecutorEvent::ToolStart {
            id: call_id,
            name,
            args,
        }),
        SubagentEvent::ToolCompleted {
            call_id,
            name,
            ok,
            output,
        } => Some(ExecutorEvent::ToolDone {
            id: call_id,
            name,
            ok,
            output,
        }),
        SubagentEvent::AssistantOutput { text } => Some(ExecutorEvent::Final { text }),
        SubagentEvent::Terminal { outcome } => Some(ExecutorEvent::Terminal {
            outcome: match outcome {
                hirsel_drivers::TerminalOutcome::Done { .. } => ExecutorTerminalOutcome::Done,
                hirsel_drivers::TerminalOutcome::Failed { reason } => {
                    ExecutorTerminalOutcome::Failed { reason }
                }
                hirsel_drivers::TerminalOutcome::Interrupted => {
                    ExecutorTerminalOutcome::Interrupted
                }
            },
        }),
    }
}

struct CliProjection {
    outcome: ExecutorTerminalOutcome,
    final_text: Option<String>,
    tool_calls: Vec<ToolCallSummary>,
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
        let projection = self.execute(tools, &request, execution, capacity).await;
        self.stop().await;
        let projection = match projection {
            Ok(projection) => projection,
            Err(error) => CliProjection {
                outcome: ExecutorTerminalOutcome::Failed {
                    reason: error.to_string(),
                },
                final_text: None,
                tool_calls: Vec::new(),
            },
        };

        let mut delay = Duration::from_millis(50);
        loop {
            match TurnIngest::complete(
                tools,
                &request.history_id,
                self.turn_id,
                projection.outcome.clone(),
                projection.final_text.clone(),
                projection.tool_calls.clone(),
            )
            .await
            {
                Ok(_) => return Ok(()),
                Err(error) => {
                    if let Ok(history) = tools.storage().history_id().await {
                        anyhow::ensure!(
                            history == request.history_id,
                            "CLI completion belongs to a previous history"
                        );
                    }
                    tracing::warn!(turn_id=self.turn_id, %error, "Retrying durable CLI terminal delivery");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(2));
                }
            }
        }
    }

    async fn recover_final_output(
        &self,
        events: &mut hirsel_drivers::EventStream,
    ) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = events.next().await {
                match event {
                    SubagentEvent::AssistantOutput { text } => return Some(text),
                    SubagentEvent::Terminal { .. } => break,
                    _ => {}
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    async fn execute(
        &self,
        tools: &ToolSuite,
        request: &OwnerTurn,
        execution: crate::storage::ThreadExecution,
        capacity: Arc<tokio::sync::Semaphore>,
    ) -> anyhow::Result<CliProjection> {
        let _permit = tokio::select! {
            _ = self.cancel.cancelled() => return Ok(CliProjection {
                outcome: ExecutorTerminalOutcome::Cancelled,
                final_text: None,
                tool_calls: Vec::new(),
            }),
            permit = capacity.acquire() => permit?,
        };
        let accepted = request.stored_turn(&tools.storage()).await?;
        anyhow::ensure!(
            accepted.state == ThreadTurnState::Queued,
            "CLI input is no longer queued"
        );
        let accepted_context = tools
            .storage()
            .accepted_turn_context(&request.history_id, self.turn_id)
            .await?;
        let turn = tools.storage().run_thread_turn(self.turn_id).await?;
        tools.publish_thread_turn(turn).await;
        let mut bridge = crate::thread_tool_bridge::ThreadToolBridge::start(
            tools.clone(),
            &request.history_id,
            self.turn_id,
        )
        .await?;
        // The same block the Native system prompt opens with, ahead of the
        // machine-readable context: a CLI executor is told where it is in the
        // Owner's vocabulary before it is handed any JSON.
        let identity = tools
            .storage()
            .thread_identity(request.thread_id)
            .await?
            .block();
        let prompt = format!(
            "You execute one accepted Hirsel Thread turn. Use the supplied scoped Thread tools for coordination. Only this Thread and its descendants are visible; create/delegate focused children and report upward. Parent/peer transcripts are not available. Artifacts require explicit creation and references.\n\n{identity}\n{}",
            owner_turn_text_with_context(request, &tools.storage(), &accepted_context)?
        );
        let crate::storage::ThreadExecution::Cli {
            agent,
            model,
            variant,
            cwd,
            ..
        } = execution
        else {
            anyhow::bail!("CLI execution settings required")
        };
        let spec = SpawnSpec {
            agent,
            model: Some(model.clone()),
            variant: Some(variant.clone()),
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
        let mut ingest = TurnIngest::new(
            &request.history_id,
            request.thread_id,
            self.turn_id,
            json!({"agent":agent,"model":model,"variant":variant}),
        );
        let result = async {
            let handle = self.driver.spawn(spec).await?;
            *self.handle.lock().await = Some(handle.clone());
            let mut events = self.driver.events(&handle)?;
            loop {
                let event = tokio::select! {
                    _ = self.cancel.cancelled() => {
                        let _ = self.driver.interrupt(&handle).await;
                        if let Some(text) = self.recover_final_output(&mut events).await {
                            ingest.accept(tools, ExecutorEvent::Final { text }).await?;
                        }
                        ingest.accept(tools, ExecutorEvent::Terminal {
                            outcome: ExecutorTerminalOutcome::Cancelled,
                        }).await?;
                        break;
                    },
                    _ = bridge.invalidated.cancelled() => {
                        let _ = self.driver.interrupt(&handle).await;
                        if let Some(text) = self.recover_final_output(&mut events).await {
                            ingest.accept(tools, ExecutorEvent::Final { text }).await?;
                        }
                        ingest.accept(tools, ExecutorEvent::Terminal {
                            outcome: ExecutorTerminalOutcome::Failed {
                                reason: "Thread tool bridge restarted after an uncertain invocation".into(),
                            },
                        }).await?;
                        break;
                    },
                    event = events.next() => event,
                };
                let Some(event) = event else {
                    ingest.accept(tools, ExecutorEvent::Terminal {
                        outcome: ExecutorTerminalOutcome::Failed {
                            reason: "CLI closed without a terminal outcome".into(),
                        },
                    }).await?;
                    break;
                };
                if let Some(event) = cli_executor_event(event) {
                    let terminal = matches!(event, ExecutorEvent::Terminal { .. });
                    ingest.accept(tools, event).await?;
                    if terminal {
                        break;
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        bridge.finish().await;
        result?;
        let mut tool_calls = ingest.tool_calls().to_vec();
        let mut ids = tool_calls
            .iter()
            .map(|call| call.id.clone())
            .collect::<HashSet<_>>();
        tool_calls.extend(
            bridge
                .tool_calls()
                .await
                .into_iter()
                .filter(|call| ids.insert(call.id.clone())),
        );
        Ok(CliProjection {
            outcome: ingest.terminal().cloned().ok_or_else(|| {
                anyhow::anyhow!("CLI event stream ended without a terminal outcome")
            })?,
            final_text: ingest.final_text().map(str::to_string),
            tool_calls,
        })
    }
}

#[cfg(test)]
#[path = "cli_delivery_tests.rs"]
mod delivery_tests;

use std::path::PathBuf;

use hirsel_drivers::{AgentKind, TerminalOutcome};
use hirsel_proto::{AgentActivityState, HostToClient, QuickReply};
use tokio::sync::{broadcast, mpsc};

use crate::{config::DriverMode, tools::ToolSuite};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub has_anthropic_key: bool,
    pub model: String,
    pub data_dir: PathBuf,
    pub driver_mode: DriverMode,
}

#[derive(Clone)]
pub struct AgentRuntime {
    tx: mpsc::Sender<OwnerTurn>,
}

#[derive(Debug)]
pub struct OwnerTurn {
    pub message_id: u64,
    pub client_id: String,
    pub body: String,
    pub anchor: Option<u64>,
}

impl AgentRuntime {
    pub fn start(
        config: RuntimeConfig,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let worker = AgentWorker {
            config,
            tools,
            broadcaster,
            rx,
        };
        tokio::spawn(worker.run());
        Self { tx }
    }

    pub async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        self.tx
            .send(turn)
            .await
            .map_err(|_| anyhow::anyhow!("Agent runtime queue is closed"))
    }
}

struct AgentWorker {
    config: RuntimeConfig,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    rx: mpsc::Receiver<OwnerTurn>,
}

impl AgentWorker {
    async fn run(mut self) {
        tracing::info!(
            model = %self.config.model,
            data_dir = %self.config.data_dir.display(),
            has_anthropic_key = self.config.has_anthropic_key,
            "Agent runtime opened session agent"
        );
        while let Some(turn) = self.rx.recv().await {
            if let Err(error) = self.handle_turn(turn).await {
                tracing::error!(%error, "Agent turn failed");
            }
        }
    }

    async fn handle_turn(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("processing owner message".to_string()),
        });
        let result = self.handle_turn_inner(&turn).await;
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
        });
        result
    }

    async fn handle_turn_inner(&self, turn: &OwnerTurn) -> anyhow::Result<()> {
        let lower = turn.body.to_lowercase();
        if self.config.driver_mode == DriverMode::Fake && lower.contains("delegate") {
            return self.handle_fake_delegation(turn).await;
        }
        if turn.anchor.is_some() {
            self.tools
                .chat_send(
                    "Acknowledged. I will continue from that Inbox reply.",
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        if lower.contains("pong") {
            self.tools.chat_send("pong", Some(turn.message_id)).await?;
            return Ok(());
        }
        if !self.config.has_anthropic_key {
            tracing::warn!("ANTHROPIC_API_KEY is not set; Agent turn degraded to chat error");
            self.tools
                .chat_send(
                    "Agent turn failed: ANTHROPIC_API_KEY is not set, so the lash-backed Agent session cannot call the model in this scaffold.",
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        self.tools
            .chat_send(
                "I received the Owner message. The lash-backed RLM pump is isolated behind `lash_runtime` and is stubbed in this scaffold build.",
                Some(turn.message_id),
            )
            .await?;
        Ok(())
    }

    async fn handle_fake_delegation(&self, turn: &OwnerTurn) -> anyhow::Result<()> {
        let anchor = self
            .tools
            .chat_send(
                "I delegated the repo fix to a Sub-agent and will file the result in the Inbox.",
                Some(turn.message_id),
            )
            .await?;
        let mut terminal_events = self.tools.terminal_events();
        let cwd = std::env::current_dir()?;
        let process = self
            .tools
            .subagents_spawn(
                AgentKind::Claude,
                "Make the trivial repo fix and report back.",
                cwd,
            )
            .await?;
        let tools = self.tools.clone();
        tokio::spawn(async move {
            loop {
                match terminal_events.recv().await {
                    Ok(event) if event.process_id == process.process_id => {
                        let content = terminal_content(&event.outcome);
                        if let Err(error) = tools
                            .inbox_file(
                                content,
                                anchor.id,
                                true,
                                vec![QuickReply {
                                    value: "ship it".to_string(),
                                    label: "Ship it".to_string(),
                                }],
                            )
                            .await
                        {
                            tracing::warn!(%error, "failed to file Sub-agent terminal Inbox Item");
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(())
    }
}

fn terminal_content(outcome: &TerminalOutcome) -> String {
    match outcome {
        TerminalOutcome::Done { summary } => {
            format!("Sub-agent completed: {summary}\n\nHow should I proceed?")
        }
        TerminalOutcome::Failed { reason } => {
            format!("Sub-agent failed: {reason}\n\nHow should I proceed?")
        }
        TerminalOutcome::Interrupted => {
            "Sub-agent was interrupted.\n\nHow should I proceed?".to_string()
        }
    }
}

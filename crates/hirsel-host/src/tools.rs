use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_util::StreamExt;
use hirsel_drivers::{
    AgentKind, ClaudeCodeDriver, CodexDriver, FakeDriver, SessionHandle, SpawnSpec, SubagentDriver,
    SubagentEvent, TerminalOutcome,
};
use hirsel_proto::{ChatAuthor, ChatMessage, HostToClient, InboxItem, QuickReply};
use serde::Serialize;
use tokio::{
    process::Command,
    sync::broadcast,
    time::{Duration, timeout},
};

use crate::{config::DriverMode, processes::ProcessStore, storage::Storage};

#[derive(Clone, Debug)]
pub struct ToolsConfig {
    pub driver_mode: DriverMode,
    pub fake_fixture: Option<PathBuf>,
}

#[derive(Clone)]
pub struct ToolSuite {
    config: ToolsConfig,
    storage: Storage,
    broadcaster: broadcast::Sender<HostToClient>,
    processes: ProcessStore,
    fake: Arc<FakeDriver>,
    claude: Arc<ClaudeCodeDriver>,
    codex: Arc<CodexDriver>,
    terminal_tx: broadcast::Sender<ProcessTerminal>,
    visible_sends: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessTerminal {
    pub process_id: String,
    pub handle: SessionHandle,
    pub outcome: TerminalOutcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpawnedProcess {
    pub process_id: String,
    pub handle: SessionHandle,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellRunOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ToolSuite {
    pub fn new(
        config: ToolsConfig,
        storage: Storage,
        broadcaster: broadcast::Sender<HostToClient>,
        processes: ProcessStore,
    ) -> Self {
        let (terminal_tx, _) = broadcast::channel(128);
        Self {
            config,
            storage,
            broadcaster,
            processes,
            fake: Arc::new(FakeDriver::default()),
            claude: Arc::new(ClaudeCodeDriver::default()),
            codex: Arc::new(CodexDriver::default()),
            terminal_tx,
            visible_sends: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn terminal_events(&self) -> broadcast::Receiver<ProcessTerminal> {
        self.terminal_tx.subscribe()
    }

    /// Count of Owner-visible sends (chat messages + inbox items) since boot.
    /// The turn pump compares before/after a turn to detect replies the Agent
    /// produced as bare prose instead of delivering through a tool.
    pub fn visible_sends(&self) -> u64 {
        self.visible_sends.load(Ordering::Relaxed)
    }

    pub async fn chat_send(
        &self,
        body_md: impl Into<String>,
        anchor: Option<u64>,
    ) -> anyhow::Result<ChatMessage> {
        let message = self
            .storage
            .append_chat(ChatAuthor::Agent, body_md.into(), anchor)
            .await?;
        let _ = self.broadcaster.send(HostToClient::Msg {
            message: message.clone(),
        });
        self.visible_sends.fetch_add(1, Ordering::Relaxed);
        Ok(message)
    }

    pub async fn inbox_file(
        &self,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<InboxItem> {
        let item = self
            .storage
            .create_inbox_item(content_md.into(), anchor, requires_response, quick_replies)
            .await?;
        let _ = self
            .broadcaster
            .send(HostToClient::InboxUpsert { item: item.clone() });
        self.visible_sends.fetch_add(1, Ordering::Relaxed);
        Ok(item)
    }

    pub async fn inbox_archive(&self, item_id: u64) -> anyhow::Result<Option<InboxItem>> {
        let item = self.storage.archive_inbox_item(item_id).await?;
        if let Some(item) = &item {
            let _ = self
                .broadcaster
                .send(HostToClient::InboxUpsert { item: item.clone() });
        }
        Ok(item)
    }

    pub async fn subagents_spawn(
        &self,
        agent: AgentKind,
        prompt: impl Into<String>,
        cwd: PathBuf,
    ) -> anyhow::Result<SpawnedProcess> {
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        self.subagents_spawn_with_process_id(agent, prompt, cwd, process_id)
            .await
    }

    pub async fn subagents_spawn_with_process_id(
        &self,
        agent: AgentKind,
        prompt: impl Into<String>,
        cwd: PathBuf,
        process_id: String,
    ) -> anyhow::Result<SpawnedProcess> {
        let prompt = prompt.into();
        let driver = self.driver_for(agent);
        let handle = driver
            .spawn(SpawnSpec {
                agent,
                prompt: prompt.clone(),
                cwd: cwd.clone(),
                fake_fixture: self.config.fake_fixture.clone(),
            })
            .await?;
        let process_id = self.processes.insert_with_id(
            process_id,
            agent,
            handle.clone(),
            prompt,
            cwd.to_string_lossy().into_owned(),
        )?;
        let mut events = driver.events(&handle)?;
        let processes = self.processes.clone();
        let terminal_tx = self.terminal_tx.clone();
        let process_id_for_task = process_id.clone();
        let handle_for_task = handle.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let terminal = match &event {
                    SubagentEvent::Terminal { outcome } => Some(outcome.clone()),
                    _ => None,
                };
                if let Err(error) = processes.push_event(&process_id_for_task, event) {
                    tracing::warn!(%error, "failed to record Sub-agent event");
                }
                if let Some(outcome) = terminal {
                    let _ = terminal_tx.send(ProcessTerminal {
                        process_id: process_id_for_task.clone(),
                        handle: handle_for_task.clone(),
                        outcome,
                    });
                    break;
                }
            }
        });
        Ok(SpawnedProcess { process_id, handle })
    }

    pub async fn subagents_prompt(
        &self,
        handle: &SessionHandle,
        text: String,
    ) -> anyhow::Result<()> {
        self.driver_for(handle.agent).prompt(handle, text).await?;
        Ok(())
    }

    pub async fn subagents_prompt_process(
        &self,
        process_id: &str,
        text: String,
    ) -> anyhow::Result<()> {
        let record = self
            .processes
            .get(process_id)?
            .ok_or_else(|| anyhow::anyhow!("Sub-agent process not found: {process_id}"))?;
        self.subagents_prompt(&record.handle, text).await
    }

    pub async fn subagents_interrupt(&self, handle: &SessionHandle) -> anyhow::Result<()> {
        self.driver_for(handle.agent).interrupt(handle).await?;
        Ok(())
    }

    pub async fn subagents_interrupt_process(&self, process_id: &str) -> anyhow::Result<()> {
        let record = self
            .processes
            .get(process_id)?
            .ok_or_else(|| anyhow::anyhow!("Sub-agent process not found: {process_id}"))?;
        self.subagents_interrupt(&record.handle).await
    }

    pub fn subagents_list(&self) -> anyhow::Result<Vec<crate::processes::ProcessRecord>> {
        self.processes.list()
    }

    pub fn subagents_progress(&self, process_id: &str) -> anyhow::Result<Vec<SubagentEvent>> {
        self.processes.recent_events(process_id)
    }

    pub async fn shell_run(
        &self,
        cmd: String,
        cwd: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<ShellRunOutput> {
        let mut command = Command::new("bash");
        command.arg("-lc").arg(cmd);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let duration = Duration::from_secs(timeout_secs.unwrap_or(30).min(600));
        let child = command.output();
        match timeout(duration, child).await {
            Ok(output) => {
                let output = output?;
                Ok(ShellRunOutput {
                    status: output.status.code(),
                    stdout: truncate_output(String::from_utf8_lossy(&output.stdout)),
                    stderr: truncate_output(String::from_utf8_lossy(&output.stderr)),
                    timed_out: false,
                })
            }
            Err(_) => Ok(ShellRunOutput {
                status: None,
                stdout: String::new(),
                stderr: "command timed out".to_string(),
                timed_out: true,
            }),
        }
    }

    fn driver_for(&self, agent: AgentKind) -> Arc<dyn SubagentDriver> {
        match (self.config.driver_mode, agent) {
            (DriverMode::Fake, _) => self.fake.clone(),
            (DriverMode::Real, AgentKind::Claude) => self.claude.clone(),
            (DriverMode::Real, AgentKind::Codex) => self.codex.clone(),
        }
    }
}

fn truncate_output(output: impl AsRef<str>) -> String {
    const MAX_BYTES: usize = 16 * 1024;
    let output = output.as_ref();
    if output.len() <= MAX_BYTES {
        return output.to_string();
    }
    let mut truncated = output
        .char_indices()
        .take_while(|(idx, _)| *idx < MAX_BYTES)
        .map(|(_, ch)| ch)
        .collect::<String>();
    truncated.push_str("\n[truncated]");
    truncated
}

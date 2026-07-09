use std::{path::PathBuf, sync::Arc};

use futures_util::StreamExt;
use hirsel_drivers::{
    AgentKind, ClaudeCodeDriver, CodexDriver, FakeDriver, SessionHandle, SpawnSpec, SubagentDriver,
    SubagentEvent, TerminalOutcome,
};
use hirsel_proto::{
    ChatAuthor, ChatMessage, HostToClient, InboxItem, ProcessInfo, QuickReply, ToolCallSummary,
};
use serde::Serialize;
use tokio::{sync::broadcast, time::Duration};

use crate::storage::{MonitorRecord, MonitorWakeOn, monitor_process_info};
use crate::{
    BroadcastLog, config::DriverMode, process_run::run_bash_command, processes::ProcessStore,
    storage::Storage,
};

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
    broadcast_log: BroadcastLog,
    processes: ProcessStore,
    fake: Arc<FakeDriver>,
    claude: Arc<ClaudeCodeDriver>,
    codex: Arc<CodexDriver>,
    terminal_tx: broadcast::Sender<ProcessTerminal>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
        broadcast_log: BroadcastLog,
        processes: ProcessStore,
    ) -> Self {
        let (terminal_tx, _) = broadcast::channel(128);
        Self {
            config,
            storage,
            broadcaster,
            broadcast_log,
            processes,
            fake: Arc::new(FakeDriver::default()),
            claude: Arc::new(ClaudeCodeDriver::default()),
            codex: Arc::new(CodexDriver::default()),
            terminal_tx,
        }
    }

    pub fn terminal_events(&self) -> broadcast::Receiver<ProcessTerminal> {
        self.terminal_tx.subscribe()
    }

    pub async fn restore_subagent_processes_after_restart(&self) -> anyhow::Result<Vec<String>> {
        let restored = self
            .storage
            .restore_subagent_processes_after_restart()
            .await?;
        for record in restored.records {
            self.processes.restore(record.clone())?;
            if matches!(record.status, crate::processes::ProcessStatus::Abandoned) {
                self.broadcast_process_upsert(crate::processes::process_info(&record));
            }
        }
        Ok(restored.abandoned)
    }

    pub async fn chat_send(
        &self,
        body_md: impl Into<String>,
        anchor: Option<u64>,
    ) -> anyhow::Result<ChatMessage> {
        self.chat_send_with_tool_calls(body_md, anchor, Vec::new())
            .await
    }

    pub async fn chat_send_with_tool_calls(
        &self,
        body_md: impl Into<String>,
        anchor: Option<u64>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<ChatMessage> {
        let message = self
            .storage
            .append_chat_with_tool_calls(ChatAuthor::Agent, body_md.into(), anchor, tool_calls)
            .await?;
        self.broadcast(HostToClient::Msg {
            message: message.clone(),
            sc: None,
        });
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
        self.broadcast(HostToClient::InboxUpsert { item: item.clone() });
        Ok(item)
    }

    pub async fn inbox_archive(&self, item_id: u64) -> anyhow::Result<Option<InboxItem>> {
        let item = self.storage.archive_inbox_item(item_id).await?;
        if let Some(item) = &item {
            self.broadcast(HostToClient::InboxUpsert { item: item.clone() });
        }
        Ok(item)
    }

    fn broadcast(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }

    fn broadcast_process_upsert(&self, process: ProcessInfo) {
        publish_process_upsert(&self.broadcast_log, &self.broadcaster, process);
    }

    pub async fn subagents_spawn(
        &self,
        agent: AgentKind,
        model: Option<String>,
        prompt: impl Into<String>,
        cwd: PathBuf,
    ) -> anyhow::Result<SpawnedProcess> {
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        self.subagents_spawn_with_process_id(agent, model, prompt, cwd, process_id)
            .await
    }

    pub async fn subagents_spawn_with_process_id(
        &self,
        agent: AgentKind,
        model: Option<String>,
        prompt: impl Into<String>,
        cwd: PathBuf,
        process_id: String,
    ) -> anyhow::Result<SpawnedProcess> {
        let prompt = prompt.into();
        let driver = self.driver_for(agent);
        let handle = driver
            .spawn(SpawnSpec {
                agent,
                model: model.clone(),
                prompt: prompt.clone(),
                cwd: cwd.clone(),
                fake_fixture: self.config.fake_fixture.clone(),
            })
            .await?;
        let process_id = self.processes.insert_with_id(
            process_id,
            agent,
            model.clone(),
            handle.clone(),
            prompt,
            cwd.to_string_lossy().into_owned(),
        )?;
        if let Some(record) = self.processes.get(&process_id)? {
            self.storage.upsert_subagent_process(&record).await?;
        }
        if let Some(process) = self.processes.info(&process_id)? {
            self.broadcast_process_upsert(process);
        }
        let mut events = driver.events(&handle)?;
        let processes = self.processes.clone();
        let storage = self.storage.clone();
        let broadcaster = self.broadcaster.clone();
        let broadcast_log = self.broadcast_log.clone();
        let terminal_tx = self.terminal_tx.clone();
        let driver_for_task = driver.clone();
        let process_id_for_task = process_id.clone();
        let handle_for_task = handle.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let terminal = match &event {
                    SubagentEvent::Terminal { outcome } => Some(outcome.clone()),
                    _ => None,
                };
                match processes.push_event(&process_id_for_task, event) {
                    Ok(Some(update)) => {
                        if let Err(error) = storage.upsert_subagent_process(&update.record).await {
                            tracing::warn!(%error, "failed to persist Sub-agent process");
                        }
                        if update.should_broadcast {
                            publish_process_upsert(&broadcast_log, &broadcaster, update.info);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "failed to record Sub-agent event"),
                }
                if let Some(outcome) = terminal {
                    if let Err(error) = driver_for_task.retire(&handle_for_task).await {
                        tracing::warn!(%error, "failed to retire Sub-agent driver session");
                    }
                    let _ = terminal_tx.send(ProcessTerminal {
                        process_id: process_id_for_task.clone(),
                        handle: handle_for_task.clone(),
                        outcome,
                    });
                    break;
                }
            }
        });
        Ok(SpawnedProcess {
            process_id,
            model,
            handle,
        })
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

    pub async fn subagents_abandon_process(&self, process_id: &str) -> anyhow::Result<()> {
        let record = self
            .processes
            .get(process_id)?
            .ok_or_else(|| anyhow::anyhow!("Sub-agent process not found: {process_id}"))?;
        let driver = self.driver_for(record.handle.agent);
        if let Err(error) = driver.interrupt(&record.handle).await {
            tracing::debug!(%error, process_id, "Sub-agent interrupt during abandon failed");
        }
        driver.retire(&record.handle).await?;
        if let Some(update) = self.processes.abandon(process_id)? {
            self.storage.upsert_subagent_process(&update.record).await?;
            if update.should_broadcast {
                self.broadcast_process_upsert(update.info);
            }
        }
        Ok(())
    }

    pub fn subagents_list(&self) -> anyhow::Result<Vec<crate::processes::ProcessRecord>> {
        self.processes.list()
    }

    pub fn subagents_progress(&self, process_id: &str) -> anyhow::Result<Vec<SubagentEvent>> {
        self.processes.recent_events(process_id)
    }

    pub fn subagents_process(
        &self,
        process_id: &str,
    ) -> anyhow::Result<Option<crate::processes::ProcessRecord>> {
        self.processes.get(process_id)
    }

    pub async fn monitors_create(
        &self,
        cmd: String,
        every_secs: u64,
        wake_on: MonitorWakeOn,
        pattern: Option<String>,
        label: String,
    ) -> anyhow::Result<MonitorRecord> {
        let record = self
            .storage
            .create_monitor(cmd, every_secs, wake_on, pattern, label)
            .await?;
        self.broadcast_monitor_upsert(&record);
        Ok(record)
    }

    pub async fn monitors_list(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        self.storage.monitors_list().await
    }

    pub async fn active_monitors(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        self.storage.active_monitors().await
    }

    pub async fn monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        self.storage.monitor(monitor_id).await
    }

    pub async fn monitors_cancel(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let record = self.storage.cancel_monitor(monitor_id).await?;
        if let Some(record) = &record {
            self.broadcast_monitor_upsert(record);
        }
        Ok(record)
    }

    pub async fn record_monitor_tick(
        &self,
        monitor_id: &str,
        last_output: String,
        summary: String,
    ) -> anyhow::Result<Option<MonitorRecord>> {
        let record = self
            .storage
            .record_monitor_tick(monitor_id, last_output, summary)
            .await?;
        if let Some(record) = &record {
            self.broadcast_monitor_upsert(record);
        }
        Ok(record)
    }

    pub fn broadcast_monitor_upsert(&self, record: &MonitorRecord) {
        self.broadcast_process_upsert(monitor_process_info(record));
    }

    pub async fn shell_run(
        &self,
        cmd: String,
        cwd: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<ShellRunOutput> {
        let duration = Duration::from_secs(timeout_secs.unwrap_or(30).min(600));
        let output = run_bash_command(cmd, cwd, duration).await?;
        Ok(ShellRunOutput {
            status: output.status,
            stdout: truncate_output(String::from_utf8_lossy(&output.stdout)),
            stderr: if output.timed_out {
                "command timed out".to_string()
            } else {
                truncate_output(String::from_utf8_lossy(&output.stderr))
            },
            timed_out: output.timed_out,
        })
    }

    fn driver_for(&self, agent: AgentKind) -> Arc<dyn SubagentDriver> {
        match (self.config.driver_mode, agent) {
            (DriverMode::Fake, _) => self.fake.clone(),
            (DriverMode::Real, AgentKind::Claude) => self.claude.clone(),
            (DriverMode::Real, AgentKind::Codex) => self.codex.clone(),
        }
    }
}

fn publish_process_upsert(
    broadcast_log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
    process: ProcessInfo,
) {
    let event = HostToClient::ProcessUpsert { process };
    broadcast_log.record(event.clone());
    let _ = broadcaster.send(event);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processes::ProcessStatus;

    #[tokio::test]
    async fn subagent_abandon_retires_driver_session_and_stays_abandoned() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            fixture.path(),
            serde_json::to_string(&serde_json::json!({
                "external_id": "fake-running",
                "progress": ["still running"],
                "delay_ms": 5_000,
                "terminal": { "status": "done", "summary": "should not happen" }
            }))
            .unwrap(),
        )
        .unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let (broadcaster, _) = broadcast::channel(128);
        let tools = ToolSuite::new(
            ToolsConfig {
                driver_mode: DriverMode::Fake,
                fake_fixture: Some(fixture.path().to_path_buf()),
            },
            storage.clone(),
            broadcaster,
            BroadcastLog::default(),
            ProcessStore::default(),
        );
        let spawned = tools
            .subagents_spawn(
                AgentKind::Codex,
                None,
                "keep running",
                dir.path().to_path_buf(),
            )
            .await
            .unwrap();

        tools
            .subagents_abandon_process(&spawned.process_id)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let record = tools
            .subagents_process(&spawned.process_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, ProcessStatus::Abandoned);
        assert!(
            tools
                .subagents_prompt_process(&spawned.process_id, "after abandon".to_string())
                .await
                .is_err(),
            "abandoned Sub-agent driver session should be retired"
        );

        let reopened = Storage::open(dir.path()).await.unwrap();
        let restored = reopened
            .restore_subagent_processes_after_restart()
            .await
            .unwrap();
        assert_eq!(restored.records.len(), 1);
        assert_eq!(restored.records[0].status, ProcessStatus::Abandoned);
    }
}

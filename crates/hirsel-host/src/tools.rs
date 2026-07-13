use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use futures_util::StreamExt;
use hirsel_drivers::{
    AgentKind, ClaudeCodeDriver, CodexDriver, FakeDriver, SessionHandle, SpawnSpec, SubagentDriver,
    SubagentEvent, TerminalOutcome,
};
use hirsel_proto::{
    ChatAuthor, ChatMessage, HostToClient, Ping, ProcessInfo, QuickReply, ToolCallSummary,
};
use serde::Serialize;
use tokio::{sync::broadcast, time::Duration};

use crate::storage::{MonitorRecord, MonitorWakeOn, monitor_process_info};
use crate::{
    BroadcastLog, config::DriverMode, process_run::run_bash_command, processes::ProcessStore,
    storage::Storage, subagent_models::SubagentModelState,
};

#[derive(Clone)]
pub struct ToolsConfig {
    pub driver_mode: DriverMode,
    pub fake_fixture: Option<PathBuf>,
    pub subagent_models: SubagentModelState,
}

#[derive(Clone)]
pub struct ToolSuite {
    config: ToolsConfig,
    storage: Storage,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    processes: ProcessStore,
    pushes: crate::push::PushGateway,
    views: crate::templates::ViewManager,
    subagent_models: SubagentModelState,
    fake: Arc<FakeDriver>,
    claude: Arc<ClaudeCodeDriver>,
    codex: Arc<CodexDriver>,
    terminal_events: TerminalEventBus,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessTerminal {
    pub process_id: String,
    pub handle: SessionHandle,
    pub outcome: TerminalOutcome,
}

#[derive(Clone)]
struct TerminalEventBus {
    tx: broadcast::Sender<ProcessTerminal>,
    retained: Arc<Mutex<HashMap<String, ProcessTerminal>>>,
}

pub(crate) struct TerminalEventReceiver {
    rx: broadcast::Receiver<ProcessTerminal>,
    retained: Arc<Mutex<HashMap<String, ProcessTerminal>>>,
    pending: VecDeque<ProcessTerminal>,
    seen: HashSet<String>,
}

impl TerminalEventBus {
    fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            retained: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn subscribe(&self) -> TerminalEventReceiver {
        let rx = self.tx.subscribe();
        let pending = self.retained_events();
        TerminalEventReceiver {
            rx,
            retained: Arc::clone(&self.retained),
            pending,
            seen: HashSet::new(),
        }
    }

    fn publish(&self, event: ProcessTerminal) {
        self.retained
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(event.process_id.clone(), event.clone());
        let _ = self.tx.send(event);
    }

    fn retained_events(&self) -> VecDeque<ProcessTerminal> {
        let mut events = self
            .retained
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(|left, right| left.process_id.cmp(&right.process_id));
        events.into()
    }
}

impl TerminalEventReceiver {
    pub(crate) async fn recv(&mut self) -> Result<ProcessTerminal, broadcast::error::RecvError> {
        loop {
            while let Some(event) = self.pending.pop_front() {
                if self.seen.insert(event.process_id.clone()) {
                    return Ok(event);
                }
            }
            match self.rx.recv().await {
                Ok(event) if self.seen.insert(event.process_id.clone()) => return Ok(event),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.pending = self
                        .retained
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .values()
                        .filter(|event| !self.seen.contains(&event.process_id))
                        .cloned()
                        .collect();
                }
                Err(error) => return Err(error),
            }
        }
    }
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
        pushes: crate::push::PushGateway,
        views: crate::templates::ViewManager,
    ) -> Self {
        let subagent_models = config.subagent_models.clone();
        Self {
            config,
            storage,
            broadcaster,
            broadcast_log,
            processes,
            pushes,
            views,
            subagent_models,
            fake: Arc::new(FakeDriver::default()),
            claude: Arc::new(ClaudeCodeDriver::default()),
            codex: Arc::new(CodexDriver::default()),
            terminal_events: TerminalEventBus::new(128),
        }
    }

    pub(crate) fn terminal_events(&self) -> TerminalEventReceiver {
        self.terminal_events.subscribe()
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

    pub async fn pings_send(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<Ping> {
        let ping = self
            .storage
            .create_ping(
                name,
                description,
                content_md,
                anchor,
                requires_response,
                quick_replies,
            )
            .await?;
        self.broadcast(HostToClient::PingUpsert { ping: ping.clone() });
        self.pushes.enqueue_ping(&ping).await;
        Ok(ping)
    }

    pub async fn pings_resolve(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let ping = self.storage.resolve_ping(ping_id).await?;
        if let Some(ping) = &ping {
            self.broadcast(HostToClient::PingUpsert { ping: ping.clone() });
        }
        Ok(ping)
    }

    pub async fn views_show(
        &self,
        template_id: Option<String>,
        spec: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
        instance_id: Option<String>,
        placement: String,
    ) -> anyhow::Result<hirsel_proto::ViewInstance> {
        self.views
            .show(template_id, spec, params, instance_id, placement)
            .await
    }

    pub async fn views_update(
        &self,
        instance_id: &str,
        params: Option<serde_json::Value>,
        patch: Option<serde_json::Value>,
    ) -> anyhow::Result<hirsel_proto::ViewInstance> {
        self.views.update(instance_id, params, patch).await
    }

    pub async fn views_clear(&self, instance_id: &str) -> anyhow::Result<()> {
        self.views.clear(instance_id).await
    }

    pub async fn views_list_templates(
        &self,
    ) -> anyhow::Result<Vec<crate::templates::TemplateSummary>> {
        self.views.templates().list().await
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
        variant: Option<String>,
        prompt: impl Into<String>,
        cwd: PathBuf,
    ) -> anyhow::Result<SpawnedProcess> {
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        self.subagents_spawn_with_process_id(agent, model, variant, prompt, cwd, process_id)
            .await
    }

    pub async fn subagents_spawn_with_process_id(
        &self,
        agent: AgentKind,
        model: Option<String>,
        variant: Option<String>,
        prompt: impl Into<String>,
        cwd: PathBuf,
        process_id: String,
    ) -> anyhow::Result<SpawnedProcess> {
        let resolved = self
            .subagent_models
            .resolve(agent, model.as_deref(), variant.as_deref())?;
        let model = Some(resolved.model_id);
        let variant = Some(resolved.variant);
        let prompt = prompt.into();
        let driver = self.driver_for(agent);
        let handle = driver
            .spawn(SpawnSpec {
                agent,
                model: model.clone(),
                variant,
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
        let terminal_events = self.terminal_events.clone();
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
                    terminal_events.publish(ProcessTerminal {
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
    async fn terminal_events_are_retained_for_late_subscribers() {
        let bus = TerminalEventBus::new(2);
        bus.publish(ProcessTerminal {
            process_id: "proc-finished".to_string(),
            handle: SessionHandle {
                id: "session-finished".to_string(),
                agent: AgentKind::Codex,
            },
            outcome: TerminalOutcome::Done {
                summary: "complete".to_string(),
            },
        });

        let mut late = bus.subscribe();
        let event = late.recv().await.unwrap();
        assert_eq!(event.process_id, "proc-finished");
        assert!(matches!(event.outcome, TerminalOutcome::Done { .. }));
    }

    #[tokio::test]
    async fn requires_response_ping_records_one_push_and_unregister_stops_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .register_push_token(hirsel_proto::PushPlatform::Android, "token-1")
            .await
            .unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Owner, "Need a decision", None)
            .await
            .unwrap();
        let (broadcaster, _) = broadcast::channel(16);
        let (pushes, recording) = crate::push::PushGateway::recording(storage.clone());
        let broadcast_log = BroadcastLog::default();
        let templates =
            crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
                .await
                .unwrap();
        let views = crate::templates::ViewManager::new(
            templates,
            broadcaster.clone(),
            broadcast_log.clone(),
        );
        let tools = ToolSuite::new(
            ToolsConfig {
                driver_mode: DriverMode::Fake,
                fake_fixture: None,
                subagent_models: test_subagent_models(dir.path()).await,
            },
            storage.clone(),
            broadcaster,
            broadcast_log,
            ProcessStore::default(),
            pushes,
            views,
        );

        let requiring_response = tools
            .pings_send(
                "release-choice",
                "Choose the release channel",
                "Stable or beta?",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
        wait_for_recorded_pushes(&recording, 1).await;
        let recorded = recording.pushes();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].tokens, vec!["token-1"]);
        assert_eq!(recorded[0].payload.title, "Hirsel");
        assert_eq!(recorded[0].payload.body, "Choose the release channel");
        assert_eq!(recorded[0].payload.data.ping_id, requiring_response.id);
        assert_eq!(recorded[0].payload.data.name, "release-choice");

        tools.pushes.enqueue_ping(&requiring_response).await;
        tools
            .pings_send(
                "informational",
                "No answer needed",
                "FYI",
                anchor.id,
                false,
                Vec::new(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(recording.pushes().len(), 1);

        assert!(storage.unregister_push_token("token-1").await.unwrap());
        tools
            .pings_send(
                "second-choice",
                "Choose again",
                "A or B?",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(recording.pushes().len(), 1);
    }

    async fn wait_for_recorded_pushes(sender: &crate::push::RecordingPushSender, count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while sender.pushes().len() < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("recorded push count reached");
    }

    async fn test_subagent_models(path: &std::path::Path) -> SubagentModelState {
        let store = crate::host_config::ConfigStore::load(
            path.join("hirsel.toml"),
            path,
            std::path::Path::new("/docs/hirsel-config.md"),
        )
        .await
        .unwrap();
        SubagentModelState::load(store)
    }

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
        let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
        let broadcast_log = BroadcastLog::default();
        let templates =
            crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
                .await
                .unwrap();
        let views = crate::templates::ViewManager::new(
            templates,
            broadcaster.clone(),
            broadcast_log.clone(),
        );
        let tools = ToolSuite::new(
            ToolsConfig {
                driver_mode: DriverMode::Fake,
                fake_fixture: Some(fixture.path().to_path_buf()),
                subagent_models: test_subagent_models(dir.path()).await,
            },
            storage.clone(),
            broadcaster,
            broadcast_log,
            ProcessStore::default(),
            pushes,
            views,
        );
        let spawned = tools
            .subagents_spawn(
                AgentKind::Codex,
                None,
                None,
                "keep running",
                dir.path().to_path_buf(),
            )
            .await
            .unwrap();
        assert_eq!(spawned.model.as_deref(), Some("gpt-5.5"));
        let specs = tools.fake.spawned_specs().unwrap();
        assert_eq!(specs[0].model.as_deref(), Some("gpt-5.5"));
        assert_eq!(specs[0].variant.as_deref(), Some("high"));

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

        tools
            .subagent_models
            .set("codex", "gpt-5.5", false, "high")
            .await
            .unwrap();
        let disabled = tools
            .subagents_spawn(
                AgentKind::Codex,
                Some("gpt-5.5".to_string()),
                None,
                "must be rejected",
                dir.path().to_path_buf(),
            )
            .await
            .unwrap_err();
        assert!(disabled.to_string().contains("enabled models: none"));
        let unknown = tools
            .subagents_spawn(
                AgentKind::Claude,
                Some("unknown".to_string()),
                None,
                "must be rejected",
                dir.path().to_path_buf(),
            )
            .await
            .unwrap_err();
        assert!(unknown.to_string().contains("unknown or disabled"));
    }
}

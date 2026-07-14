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
    ChatAuthor, ChatMessage, Event, EventKind, EventSource, EventSourceKind, EventStatus,
    HostToClient, Ping, ProcessInfo, QuickReply, ToolCallSummary,
};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSessionBootstrap {
    pub session_id: String,
    pub handoff_seed: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct JudgmentOptionInput {
    #[serde(default)]
    pub key: Option<String>,
    pub label: String,
    pub detail: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone)]
struct JudgmentOption {
    key: String,
    label: String,
    detail: String,
    recommended: bool,
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

    pub(crate) async fn prepare_agent_session(
        &self,
        tool_surface_fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionBootstrap> {
        let state = self
            .storage
            .reconcile_agent_tool_surface(tool_surface_fingerprint, tool_names)
            .await?;
        if !state.rotated {
            return Ok(AgentSessionBootstrap {
                session_id: state.session_id,
                handoff_seed: None,
            });
        }

        let handoff_seed = self.session_handoff_seed(&state.added_tools).await?;
        self.emit_session_rotated(&state.session_id, &state.added_tools)
            .await?;
        Ok(AgentSessionBootstrap {
            session_id: state.session_id,
            handoff_seed: Some(handoff_seed),
        })
    }

    async fn session_handoff_seed(&self, added_tools: &[String]) -> anyhow::Result<String> {
        let messages = self.storage.recent_chat(30).await?;
        let events = self
            .storage
            .all_pings()
            .await?
            .into_iter()
            .filter(|event| event.status == EventStatus::Open && !event.archived)
            .collect::<Vec<_>>();
        let added_tools = display_added_tools(added_tools);
        let mut seed = format!(
            "Session rotated by the host to pick up new tools: {added_tools}. Prior conversation summary follows.\n\n## Recent chat\n"
        );
        if messages.is_empty() {
            seed.push_str("(none)\n");
        } else {
            for message in messages {
                let author = match message.author {
                    ChatAuthor::Owner => "owner",
                    ChatAuthor::Agent => "agent",
                };
                seed.push_str(&format!(
                    "- {author}: {}\n",
                    indent_continuation_lines(&message.body)
                ));
            }
        }
        seed.push_str("\n## Open events\n");
        if events.is_empty() {
            seed.push_str("(none)\n");
        } else {
            for event in events {
                seed.push_str(&format!(
                    "- [{}] {}: {}\n",
                    event_kind_name(event.kind),
                    event.name,
                    indent_continuation_lines(&event.description)
                ));
            }
        }
        Ok(seed)
    }

    async fn emit_session_rotated(
        &self,
        session_id: &str,
        added_tools: &[String],
    ) -> anyhow::Result<Event> {
        let added_tools = display_added_tools(added_tools);
        let description = format!(
            "Opened {session_id} after the tool surface changed. New tools: {added_tools}."
        );
        let anchor = self
            .storage
            .append_chat(
                ChatAuthor::Agent,
                format!("Host rotated the Agent session to `{session_id}`."),
                None,
            )
            .await?
            .id;
        let event = self
            .storage
            .create_event(
                EventKind::Info,
                EventSource {
                    kind: EventSourceKind::Scheduled,
                    r#ref: Some(session_id.to_string()),
                },
                "session-rotated",
                &description,
                info_ui(&description),
                anchor,
                false,
                Vec::new(),
            )
            .await?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
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
        self.pings_send_with_view(
            name,
            description,
            content_md,
            anchor,
            requires_response,
            quick_replies,
            None,
            None,
        )
        .await
    }

    pub async fn events_judgment(
        &self,
        question: impl Into<String>,
        context: impl Into<String>,
        anchor: u64,
        options: Vec<JudgmentOptionInput>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        let question = question.into();
        let name = judgment_event_name(&question);
        self.events_judgment_named(name, question, context, anchor, options, view, unblocks)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn events_judgment_named(
        &self,
        name: impl Into<String>,
        question: impl Into<String>,
        context: impl Into<String>,
        anchor: u64,
        options: Vec<JudgmentOptionInput>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        let question = question.into();
        let context = context.into();
        validate_judgment_context(&question, &context)?;
        let options = normalize_judgment_options(options)?;
        let quick_replies = options
            .iter()
            .map(|option| QuickReply {
                value: option.key.clone(),
                label: option.label.clone(),
            })
            .collect();
        let ui = blessed_judgment_ui_from_options(&question, &context, &options, view, unblocks);
        self.create_agent_event(
            EventKind::Judgment,
            name,
            question,
            ui,
            anchor,
            true,
            quick_replies,
        )
        .await
    }

    pub async fn events_notify(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: Option<String>,
        anchor: u64,
    ) -> anyhow::Result<Event> {
        let description = description.into();
        let content = content_md.unwrap_or_else(|| description.clone());
        self.create_agent_event(
            EventKind::Info,
            name,
            description,
            info_ui(&content),
            anchor,
            false,
            Vec::new(),
        )
        .await
    }

    pub async fn events_summary(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: Option<String>,
        ui: Option<serde_json::Value>,
        anchor: u64,
    ) -> anyhow::Result<Event> {
        let ui = match (content_md, ui) {
            (Some(content), None) => summary_ui(&content),
            (None, Some(ui)) => {
                crate::templates::validate(&ui)?;
                ui
            }
            _ => anyhow::bail!("provide exactly one of `content_md` or `ui`"),
        };
        self.create_agent_event(
            EventKind::Summary,
            name,
            description,
            ui,
            anchor,
            false,
            Vec::new(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_agent_event(
        &self,
        kind: EventKind,
        name: impl Into<String>,
        description: impl Into<String>,
        ui: serde_json::Value,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<Event> {
        let event = self
            .storage
            .create_event(
                kind,
                EventSource {
                    kind: EventSourceKind::Agent,
                    r#ref: None,
                },
                name,
                description,
                ui,
                anchor,
                requires_response,
                quick_replies,
            )
            .await?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        self.pushes.enqueue_event(&event).await;
        Ok(event)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn pings_send_with_view(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        let name = name.into();
        let description = description.into();
        let content = content_md.into();
        if requires_response {
            let options = quick_replies
                .into_iter()
                .map(|reply| JudgmentOptionInput {
                    key: None,
                    label: reply.label,
                    detail: reply.value,
                    recommended: false,
                })
                .collect();
            self.events_judgment_named(name, description, content, anchor, options, view, unblocks)
                .await
        } else {
            self.events_notify(name, description, Some(content), anchor)
                .await
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn pings_send_with_options(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        options: Vec<JudgmentOptionInput>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        if !requires_response && !options.is_empty() {
            anyhow::bail!("info events cannot carry judgment options");
        }
        if requires_response {
            self.events_judgment_named(
                name,
                description,
                content_md,
                anchor,
                options,
                view,
                unblocks,
            )
            .await
        } else {
            self.events_notify(name, description, Some(content_md.into()), anchor)
                .await
        }
    }

    pub async fn pings_resolve(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let ping = self.storage.resolve_ping(ping_id).await?;
        if let Some(ping) = &ping {
            self.broadcast(HostToClient::EventUpsert {
                event: ping.clone(),
            });
        }
        Ok(ping)
    }

    pub async fn events_archive(&self, event_id: u64) -> anyhow::Result<Option<Event>> {
        let event = self.storage.archive_event(event_id).await?;
        if let Some(event) = &event {
            self.broadcast(HostToClient::EventUpsert {
                event: event.clone(),
            });
        }
        Ok(event)
    }

    pub async fn events_clear(&self) -> anyhow::Result<usize> {
        let event_ids = self
            .storage
            .all_pings()
            .await?
            .into_iter()
            .filter(|event| {
                !event.archived
                    && (event.status == EventStatus::Done
                        || (event.read && !event.requires_response))
            })
            .map(|event| event.id)
            .collect::<Vec<_>>();
        let count = self.storage.archive_finished_events().await?;
        for event_id in event_ids {
            if let Some(event) = self.storage.ping(event_id).await? {
                self.broadcast(HostToClient::EventUpsert { event });
            }
        }
        Ok(count)
    }

    pub async fn emit_scheduled_digest(
        &self,
        job_id: impl Into<String>,
        text: impl Into<String>,
        status: impl Into<String>,
    ) -> anyhow::Result<Event> {
        let job_id = job_id.into();
        let text = text.into();
        let status = status.into();
        let anchor = self
            .storage
            .append_chat(
                ChatAuthor::Agent,
                format!("Scheduled lash job `{job_id}` emitted a digest."),
                None,
            )
            .await?
            .id;
        let event = self
            .storage
            .create_event(
                EventKind::Summary,
                EventSource {
                    kind: EventSourceKind::Scheduled,
                    r#ref: Some(job_id),
                },
                "morning-digest",
                "Scheduled fleet digest",
                digest_ui(&text, &status),
                anchor,
                false,
                Vec::new(),
            )
            .await?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
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

fn blessed_judgment_ui_from_options(
    heading: &str,
    context: &str,
    options: &[JudgmentOption],
    view: Option<serde_json::Value>,
    unblocks: Option<u64>,
) -> serde_json::Value {
    let options = options
        .iter()
        .map(|option| {
            serde_json::json!({
                "key": option.key,
                "label": option.label,
                "detail": option.detail,
                "recommended": option.recommended,
            })
        })
        .collect::<Vec<_>>();
    let mut children = Vec::new();
    // Eyebrow: the boundary accent stripe stays, but the fixed "fleet stopped"
    // copy becomes the human unblocks fact — what deciding actually frees. When a
    // decision unblocks no one, no eyebrow is emitted at all (the heading leads).
    if let Some(unblocks) = unblocks {
        if unblocks > 0 {
            let agents = if unblocks == 1 { "agent" } else { "agents" };
            children.push(serde_json::json!({
                "type": "eyebrow",
                "tone": "accent",
                "boundary": true,
                "text": format!("Deciding unblocks {unblocks} {agents}")
            }));
        }
    }
    children.push(serde_json::json!({ "type": "heading", "text": heading }));
    if !context.trim().is_empty() {
        children.push(serde_json::json!({ "type": "text", "text": context }));
    }
    children.push(serde_json::json!({ "type": "optionList", "options": options }));
    if let Some(view) = view {
        children.push(serde_json::json!({ "type": "viewSlot", "view": view }));
    }
    serde_json::json!({ "type": "card", "children": children })
}

fn normalize_judgment_options(
    options: Vec<JudgmentOptionInput>,
) -> anyhow::Result<Vec<JudgmentOption>> {
    validate_judgment_option_count(options.len())?;
    let recommended_count = options.iter().filter(|option| option.recommended).count();
    if recommended_count > 1 {
        anyhow::bail!("judgment events require exactly one recommended option");
    }

    let mut keys = HashSet::new();
    let mut normalized = Vec::with_capacity(options.len());
    for (index, option) in options.into_iter().enumerate() {
        let key = option.key.unwrap_or_else(|| option_key(index));
        if key.len() != 1 || !key.as_bytes()[0].is_ascii_uppercase() {
            anyhow::bail!("judgment option keys must be one uppercase ASCII letter");
        }
        if !keys.insert(key.clone()) {
            anyhow::bail!("judgment option keys must be unique");
        }
        if option.label.trim().is_empty() || option.detail.trim().is_empty() {
            anyhow::bail!("judgment option labels and details must not be empty");
        }
        normalized.push(JudgmentOption {
            key,
            label: option.label,
            detail: option.detail,
            recommended: option.recommended || (recommended_count == 0 && index == 0),
        });
    }
    Ok(normalized)
}

fn validate_judgment_option_count(count: usize) -> anyhow::Result<()> {
    if !(2..=4).contains(&count) {
        anyhow::bail!("judgment events require 2–4 options");
    }
    Ok(())
}

fn validate_judgment_context(heading: &str, context: &str) -> anyhow::Result<()> {
    let heading = normalize_judgment_text(heading);
    let context = normalize_judgment_text(context);
    let heading_tokens = heading.split_whitespace().collect::<HashSet<_>>();
    let context_tokens = context.split_whitespace().collect::<HashSet<_>>();
    let shared_tokens = context_tokens.intersection(&heading_tokens).count();
    let context_is_heading_paraphrase =
        !context_tokens.is_empty() && shared_tokens * 5 >= context_tokens.len() * 4;
    if !context.is_empty()
        && (heading == context
            || heading.starts_with(&context)
            || context.starts_with(&heading)
            || context_is_heading_paraphrase)
    {
        anyhow::bail!(
            "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
        );
    }
    Ok(())
}

fn normalize_judgment_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character.is_whitespace() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn info_ui(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "card",
        "children": [{ "type": "text", "text": text }]
    })
}

fn display_added_tools(added_tools: &[String]) -> String {
    if added_tools.is_empty() {
        "none".to_string()
    } else {
        added_tools.join(", ")
    }
}

fn indent_continuation_lines(value: &str) -> String {
    value.replace('\n', "\n  ")
}

fn event_kind_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Judgment => "judgment",
        EventKind::Info => "info",
        EventKind::Summary => "summary",
    }
}

fn summary_ui(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "card",
        "children": [{ "type": "text", "text": text }]
    })
}

fn digest_ui(text: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "card",
        "children": [
            { "type": "text", "text": text },
            { "type": "status", "state": "success", "label": status },
            {
                "type": "keyValue",
                "items": [{ "label": "producer", "value": "scheduled lash job" }]
            }
        ]
    })
}

fn option_key(index: usize) -> String {
    u8::try_from(index)
        .ok()
        .and_then(|index| b'A'.checked_add(index))
        .filter(u8::is_ascii_uppercase)
        .map(char::from)
        .map(String::from)
        .unwrap_or_else(|| (index + 1).to_string())
}

fn judgment_event_name(question: &str) -> String {
    const MAX_LEN: usize = 32;
    // A lowercase, hyphen-joined slug of the question's words, truncated on a
    // WORD boundary so the name never ends mid-word: a trailing word that would
    // overflow the cap is dropped whole rather than sliced (the live bug sliced
    // it, producing `…-digestincl`). A single word longer than the cap is
    // hard-clipped so the slug still stays bounded.
    let mut name = String::new();
    for word in question
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
    {
        let projected = if name.is_empty() {
            word.chars().count()
        } else {
            name.chars().count() + 1 + word.chars().count()
        };
        if projected > MAX_LEN {
            if name.is_empty() {
                name = word.chars().take(MAX_LEN).collect();
            }
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(&word);
    }
    if name.is_empty() {
        "judgment".to_string()
    } else {
        name
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

    fn judgment_options(count: usize) -> Vec<JudgmentOptionInput> {
        (0..count)
            .map(|index| JudgmentOptionInput {
                key: Some(option_key(index)),
                label: format!("Option {}", index + 1),
                detail: format!("Choose option {}", index + 1),
                recommended: index == 0,
            })
            .collect()
    }

    #[test]
    fn judgment_context_rejects_exact_and_prefix_echoes() {
        let error = validate_judgment_context(
            "Which release channel should we use?",
            "  WHICH RELEASE CHANNEL SHOULD WE USE. ",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
        );

        for (heading, context) in [
            (
                "Which release channel should we use?",
                "Which release channel",
            ),
            (
                "Choose stable",
                "Choose stable because it has the smaller blast radius.",
            ),
        ] {
            assert_eq!(
                validate_judgment_context(heading, context)
                    .unwrap_err()
                    .to_string(),
                "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
            );
        }
    }

    #[test]
    fn judgment_context_rejects_paraphrases_but_allows_additive_context() {
        assert_eq!(
            validate_judgment_context(
                "Where should canvas view state persist?",
                "Choose where canvas view state should persist.",
            )
            .unwrap_err()
            .to_string(),
            "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
        );

        validate_judgment_context(
            "Where should canvas view state persist?",
            "resolve_ping is terminal on the wire; the reopen affordance needs a real op",
        )
        .unwrap();
    }

    #[test]
    fn empty_judgment_context_is_allowed_and_omitted_from_ui() {
        validate_judgment_context("Which release channel?", "  ").unwrap();

        let ui = blessed_judgment_ui_from_options(
            "Which release channel?",
            "  ",
            &normalize_judgment_options(judgment_options(2)).unwrap(),
            None,
            Some(3),
        );
        assert_eq!(ui["children"].as_array().unwrap().len(), 3);
        assert_eq!(ui["children"][0]["type"], "eyebrow");
        assert_eq!(ui["children"][0]["text"], "Deciding unblocks 3 agents");
        assert_eq!(ui["children"][0]["boundary"], true);
        assert_eq!(ui["children"][1]["type"], "heading");
        assert_eq!(ui["children"][2]["type"], "optionList");
    }

    #[test]
    fn judgment_without_unblocks_omits_the_eyebrow_and_leads_with_the_heading() {
        let ui = blessed_judgment_ui_from_options(
            "Which release channel?",
            "  ",
            &normalize_judgment_options(judgment_options(2)).unwrap(),
            None,
            None,
        );
        let children = ui["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["type"], "heading");
        assert_eq!(children[1]["type"], "optionList");
    }

    #[test]
    fn judgment_options_require_two_to_four_choices() {
        for count in [1, 5] {
            assert_eq!(
                normalize_judgment_options(judgment_options(count))
                    .unwrap_err()
                    .to_string(),
                "judgment events require 2–4 options"
            );
        }
        normalize_judgment_options(judgment_options(2)).unwrap();
        normalize_judgment_options(judgment_options(4)).unwrap();
    }

    #[test]
    fn keyless_options_get_ordered_keys_and_first_recommendation() {
        let options = (0..3)
            .map(|index| JudgmentOptionInput {
                key: None,
                label: format!("Option {}", index + 1),
                detail: format!("Tradeoff {}", index + 1),
                recommended: false,
            })
            .collect();

        let normalized = normalize_judgment_options(options).unwrap();
        assert_eq!(
            normalized
                .iter()
                .map(|option| option.key.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        assert!(normalized[0].recommended);
        assert!(!normalized[1].recommended);
        assert!(!normalized[2].recommended);
    }

    #[test]
    fn supplied_keys_and_recommendations_keep_existing_validation() {
        let invalid_key = vec![
            JudgmentOptionInput {
                key: Some("a".to_string()),
                label: "Alpha".to_string(),
                detail: "First tradeoff".to_string(),
                recommended: true,
            },
            JudgmentOptionInput {
                key: Some("B".to_string()),
                label: "Beta".to_string(),
                detail: "Second tradeoff".to_string(),
                recommended: false,
            },
        ];
        assert_eq!(
            normalize_judgment_options(invalid_key)
                .unwrap_err()
                .to_string(),
            "judgment option keys must be one uppercase ASCII letter"
        );

        let mut duplicate_key = judgment_options(2);
        duplicate_key[1].key = Some("A".to_string());
        assert_eq!(
            normalize_judgment_options(duplicate_key)
                .unwrap_err()
                .to_string(),
            "judgment option keys must be unique"
        );

        let mut duplicate_recommendation = judgment_options(2);
        duplicate_recommendation[1].recommended = true;
        assert_eq!(
            normalize_judgment_options(duplicate_recommendation)
                .unwrap_err()
                .to_string(),
            "judgment events require exactly one recommended option"
        );
    }

    #[test]
    fn populated_judgment_keeps_the_blessed_layout_and_optional_unblocks() {
        let ui = blessed_judgment_ui_from_options(
            "Which release channel?",
            "Stable reduces rollout risk; edge gets feedback sooner.",
            &normalize_judgment_options(judgment_options(2)).unwrap(),
            Some(serde_json::json!({ "type": "text", "text": "release diff" })),
            Some(2),
        );
        let children = ui["children"].as_array().unwrap();
        assert_eq!(children[0]["type"], "eyebrow");
        assert_eq!(children[0]["text"], "Deciding unblocks 2 agents");
        assert_eq!(children[1]["type"], "heading");
        assert_eq!(children[2]["type"], "text");
        assert_eq!(children[3]["type"], "optionList");
        assert_eq!(children[4]["type"], "viewSlot");
        assert_eq!(children.len(), 5);
    }

    #[test]
    fn judgment_event_name_truncates_on_a_word_boundary() {
        // The live bug sliced mid-word at 32 chars ("…-digestincl"); the name must
        // instead drop the overflowing trailing word whole.
        let name = judgment_event_name("Should the morning digest include overnight fleet output?");
        assert_eq!(name, "should-the-morning-digest");
        assert!(name.chars().count() <= 32);
        assert!(!name.ends_with('-'));

        // Punctuation collapses to single hyphens and never leaves a trailing one.
        assert_eq!(
            judgment_event_name("Reopen a resolved Ping — how?"),
            "reopen-a-resolved-ping-how"
        );

        // A single word longer than the cap is hard-clipped, still bounded.
        let long = judgment_event_name("supercalifragilisticexpialidocioussummary");
        assert_eq!(long.chars().count(), 32);

        // No alphanumerics at all falls back to a stable default.
        assert_eq!(judgment_event_name("—!?"), "judgment");
    }

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
                vec![
                    QuickReply {
                        value: "stable".to_string(),
                        label: "Stable".to_string(),
                    },
                    QuickReply {
                        value: "beta".to_string(),
                        label: "Beta".to_string(),
                    },
                ],
            )
            .await
            .unwrap();
        wait_for_recorded_pushes(&recording, 1).await;
        let recorded = recording.pushes();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].tokens, vec!["token-1"]);
        assert_eq!(recorded[0].payload.title, "Hirsel");
        assert_eq!(recorded[0].payload.body, "Choose the release channel");
        assert_eq!(recorded[0].payload.data.event_id, requiring_response.id);
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
                vec![
                    QuickReply {
                        value: "a".to_string(),
                        label: "A".to_string(),
                    },
                    QuickReply {
                        value: "b".to_string(),
                        label: "B".to_string(),
                    },
                ],
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

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use hirsel_drivers::{AgentKind, TerminalOutcome};
use hirsel_proto::{
    AgentActivityState, HostToClient, Ping, QuickReply, SendMode, ToolCallSummary, TurnEventKind,
};
use lash::{
    InputItem, PromptLayerSink, TurnInput,
    observe::RemoteSessionObservationStreamItem,
    plugins::{
        PluginError, PluginExtensionContribution, PluginFactory, PluginRegistrar,
        PluginSessionContext, SessionPlugin,
    },
    process::{
        ProcessAwaitOutput, ProcessAwaiter, ProcessCompletionAuthority, ProcessEventAppendRequest,
        ProcessEventType, ProcessIdentity, ProcessInput, ProcessStartRequest, ProcessTerminalState,
        ProcessWakeDedupeKey, ProcessWakeDelivery, ProcessWakeSpec, RecoveryDisposition,
        SessionScope,
    },
    provider::{ProviderHandle, ProviderOptions, ReasoningSelection},
    remote::{
        observations::{RemoteSessionCursor, RemoteSessionObservationEventPayload},
        usage::RemoteTurnEvent,
    },
    runtime::{QueuedWorkDriver, QueuedWorkRunHandle, QueuedWorkRunRequest},
    tools::{
        LashlangToolBinding, StaticToolExecute, StaticToolProvider, ToolCall, ToolDefinition,
        ToolDefinitionLashlangExt, ToolResult, ToolScheduling,
    },
    triggers::LashSchema,
};
use lash_core::{
    DurabilityTier, ProcessEngine, ProcessEngineRunContext, ProcessEngineValidationContext,
    ProcessEventSemanticsSpec, ProcessOriginator, ProcessRunOutcome, ProcessTerminalSpec,
    ProcessValueSelector, SessionPolicy, TriggerStore, TriggerSubscriptionFilter,
    TurnInputCheckpointBoundary, TurnInputIngress, plugin::ProcessEngineContributionContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast};
use uuid::Uuid;

use crate::{
    BroadcastLog,
    config::{AgentMode, DriverMode, ProviderMode},
    monitors::{output_tail, run_monitor_tick},
    storage::{MonitorRecord, MonitorWakeOn, StoredBlob},
    tools::ToolSuite,
};

const AGENT_SESSION_ID: &str = "agent";
const HIRSEL_SUBAGENT_ENGINE: &str = "hirsel_subagent";
const HIRSEL_MONITOR_ENGINE: &str = "hirsel_monitor";
const SUBAGENT_COMPLETED: &str = "subagent.completed";
const SUBAGENT_FAILED: &str = "subagent.failed";
const SUBAGENT_CANCELLED: &str = "subagent.cancelled";
const SUBAGENT_ABANDONED: &str = "subagent.abandoned";
const MONITOR_WAKE_EVENT: &str = "monitor.wake";
const TIMER_SOURCE_TYPE: &str = "timer.Schedule";
const TIMER_EVENT_TYPE: &str = "timer.Tick";
const TIMER_MIN_RECURRING_SECS: u64 = 60;
pub(crate) const AGENT_PROMPT: &str = include_str!("../../../prompts/agent.md");

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub agent_mode: AgentMode,
    pub provider_mode: ProviderMode,
    pub anthropic_api_key: Option<String>,
    pub model: String,
    pub data_dir: PathBuf,
    pub driver_mode: DriverMode,
}

#[derive(Clone)]
pub struct AgentRuntime {
    backend: Arc<AgentBackend>,
}

#[derive(Debug)]
pub struct OwnerTurn {
    pub message_id: u64,
    pub client_id: String,
    pub body: String,
    pub anchor: Option<u64>,
    pub attachments: Vec<StoredBlob>,
    pub mentioned_pings: Vec<Ping>,
    pub mode: SendMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelQueuedResult {
    Cancelled,
    AlreadyClaimed,
}

enum AgentBackend {
    Scripted(Arc<ScriptedAgentRuntime>),
    Lash(Arc<LashAgentRuntime>),
    Degraded(Arc<DegradedAgentRuntime>),
}

impl AgentRuntime {
    pub fn readiness(&self) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(_) | AgentBackend::Lash(_) => Ok(()),
            AgentBackend::Degraded(_) => anyhow::bail!("Lash store is unavailable"),
        }
    }

    pub(crate) fn side_chat_backend(&self) -> crate::side_chat::SideChatBackend {
        match self.backend.as_ref() {
            AgentBackend::Scripted(_) => crate::side_chat::SideChatBackend::Scripted,
            AgentBackend::Lash(runtime) => {
                crate::side_chat::SideChatBackend::Lash(Arc::new(runtime.core.clone()))
            }
            AgentBackend::Degraded(runtime) => {
                crate::side_chat::SideChatBackend::Degraded(runtime.reason.clone())
            }
        }
    }

    pub async fn start(
        config: RuntimeConfig,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
    ) -> anyhow::Result<Self> {
        match config.agent_mode {
            AgentMode::Scripted => Ok(Self {
                backend: Arc::new(AgentBackend::Scripted(start_scripted_runtime(
                    config,
                    tools,
                    broadcaster,
                    broadcast_log,
                ))),
            }),
            AgentMode::Lash => {
                match LashAgentRuntime::start(config, tools, broadcaster, broadcast_log).await? {
                    LashStartup::Ready(runtime) => Ok(Self {
                        backend: Arc::new(AgentBackend::Lash(runtime)),
                    }),
                    LashStartup::Unavailable(runtime) => Ok(Self {
                        backend: Arc::new(AgentBackend::Degraded(runtime)),
                    }),
                }
            }
        }
    }

    pub async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(runtime) => runtime.enqueue(turn).await,
            AgentBackend::Lash(runtime) => runtime.enqueue_inner(turn).await,
            AgentBackend::Degraded(runtime) => runtime.enqueue(turn).await,
        }
    }

    pub async fn cancel_turn(&self) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(runtime) => runtime.cancel_turn().await,
            AgentBackend::Lash(runtime) => runtime.cancel_turn().await,
            AgentBackend::Degraded(runtime) => runtime.cancel_turn().await,
        }
    }

    pub async fn cancel_queued(&self, client_id: &str) -> anyhow::Result<CancelQueuedResult> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(runtime) => runtime.cancel_queued(client_id).await,
            AgentBackend::Lash(runtime) => runtime.cancel_queued(client_id).await,
            AgentBackend::Degraded(runtime) => runtime.cancel_queued(client_id).await,
        }
    }

    pub async fn start_monitor_process(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Lash(runtime) => runtime.start_monitor_process(record).await,
            AgentBackend::Scripted(runtime) => {
                runtime.spawn_standalone_monitor(record.id.clone());
                Ok(())
            }
            AgentBackend::Degraded(_) => Ok(()),
        }
    }

    pub async fn cancel_monitor_process(&self, monitor_id: &str) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Lash(runtime) => runtime.cancel_monitor_process(monitor_id).await,
            AgentBackend::Scripted(_) | AgentBackend::Degraded(_) => Ok(()),
        }
    }

    pub async fn deliver_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(runtime) => runtime.deliver_monitor_wake(text).await,
            AgentBackend::Lash(runtime) => runtime.enqueue_monitor_wake(text).await,
            AgentBackend::Degraded(runtime) => runtime.deliver_monitor_wake(text).await,
        }
    }
}

fn start_scripted_runtime(
    config: RuntimeConfig,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
) -> Arc<ScriptedAgentRuntime> {
    let runtime = Arc::new(ScriptedAgentRuntime {
        config,
        tools,
        broadcaster,
        broadcast_log,
        state: Arc::new(Mutex::new(ScriptedQueueState::default())),
        notify: Arc::new(Notify::new()),
    });
    let worker = Arc::clone(&runtime);
    tokio::spawn(async move {
        worker.run().await;
    });
    let restore_worker = Arc::clone(&runtime);
    tokio::spawn(async move {
        if let Err(error) = restore_worker
            .tools
            .restore_subagent_processes_after_restart()
            .await
        {
            tracing::warn!(%error, "failed to restore scripted Sub-agent processes after restart");
        }
    });
    let monitor_worker = Arc::clone(&runtime);
    tokio::spawn(async move {
        monitor_worker.spawn_active_standalone_monitors().await;
    });
    runtime
}

enum LashStartup {
    Ready(Arc<LashAgentRuntime>),
    Unavailable(Arc<DegradedAgentRuntime>),
}

struct LashAgentRuntime {
    core: lash::LashCore,
    session: lash::LashSession,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    notify: Arc<Notify>,
    pump_lock: Mutex<()>,
    anchors: Arc<Mutex<TurnAnchorState>>,
    active_turn_id: Arc<Mutex<Option<String>>>,
    drain_seq: AtomicU64,
    drain_boot_ms: u64,
    drain_retry_scheduled: AtomicBool,
    drain_retry_attempts: AtomicU64,
}

#[derive(Debug, Clone)]
struct TurnAnchors {
    owner_message_id: u64,
}

#[derive(Debug, Default)]
struct TurnAnchorState {
    pending_by_source_key: HashMap<String, TurnAnchors>,
    active: Option<TurnAnchors>,
}

impl LashAgentRuntime {
    async fn start(
        config: RuntimeConfig,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
    ) -> anyhow::Result<LashStartup> {
        let provider = match build_provider(&config).await {
            Ok(provider) => provider,
            Err(ProviderUnavailable { message }) => {
                tracing::warn!(%message, "Lash Agent provider unavailable; using degraded runtime");
                return Ok(LashStartup::Unavailable(Arc::new(DegradedAgentRuntime {
                    reason: message,
                    tools,
                    broadcaster,
                    broadcast_log,
                })));
            }
        };

        tokio::fs::create_dir_all(&config.data_dir).await?;
        let lash_dir = config.data_dir.join("lash");
        tokio::fs::create_dir_all(&lash_dir).await?;
        let store_factory = Arc::new(lash_sqlite_store::SqliteSessionStoreFactory::new(
            lash_dir.join("sessions"),
        ));
        let artifact_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("artifacts.db")).await?)
                as Arc<dyn lash::persistence::LashlangArtifactStore>;
        let process_env_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("process-env.db")).await?);
        let trigger_store = Arc::new(
            lash_sqlite_store::SqliteTriggerStore::open(&lash_dir.join("triggers.db")).await?,
        ) as Arc<dyn TriggerStore>;
        let process_registry = Arc::new(
            lash_sqlite_store::SqliteProcessRegistry::open(&lash_dir.join("processes.db")).await?,
        ) as Arc<dyn lash::process::ProcessRegistry>;
        let model_spec = lash::ModelSpec::from_token_limits(
            config.model.clone(),
            ReasoningSelection::ProviderDefault,
            200_000,
            None,
        )
        .map_err(|error| anyhow::anyhow!("invalid HIRSEL_MODEL metadata: {error}"))?;
        let rlm_config = lash_protocol_rlm::RlmProtocolPluginConfig::default()
            .with_lashlang_abilities(
                lash_protocol_rlm::LashlangAbilities::default()
                    .with_processes()
                    .with_triggers(),
            );
        let rlm_factory =
            lash_protocol_rlm::RlmProtocolPluginFactory::new(rlm_config, artifact_store);
        let tool_provider = Arc::new(StaticToolProvider::new(
            hirsel_tool_definitions(),
            HirselToolExecutor {
                tools: tools.clone(),
                anchors: Arc::new(Mutex::new(TurnAnchorState::default())),
            },
        ));
        let anchors = tool_provider.executor().anchors.clone();
        let notify = Arc::new(Notify::new());
        let queued_work_driver = QueuedWorkDriver::new(Arc::new(HirselQueuedWorkNotifier {
            notify: Arc::clone(&notify),
        }));
        let core = lash::LashCore::rlm_builder(rlm_factory)
            .provider(provider)
            .model(model_spec)
            .store_factory(store_factory.clone())
            .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
                lash_dir.join("attachments"),
            )))
            .process_env_store(process_env_store)
            .effect_host(Arc::new(lash::durability::InlineEffectHost::default()))
            .process_registry(process_registry)
            .trigger_store(Arc::clone(&trigger_store))
            .tools(tool_provider)
            .plugin(Arc::new(HirselProcessPluginFactory {
                tools: tools.clone(),
                notify: Arc::clone(&notify),
            }))
            .queued_work_driver(queued_work_driver)
            .build()?;
        let process_registry = core
            .process_registry()
            .ok_or_else(|| anyhow::anyhow!("Lash process registry was not configured"))?;
        let session = core
            .session(AGENT_SESSION_ID)
            // A liveness-aware lease identity lets a rebooted host reclaim the
            // session execution lease immediately when the previous holder was
            // a now-dead process on this same host+boot (e.g. after SIGKILL),
            // instead of waiting out the lease TTL.
            .session_execution_owner(lash_core::LeaseOwnerIdentity::local_process(
                "hirsel-host:agent",
                Uuid::new_v4().to_string(),
                local_host_id(),
            ))
            .prompt_contribution(lash::prompt::PromptContribution::guidance(
                "Hirsel Agent",
                AGENT_PROMPT,
            ))
            .open()
            .await?;

        let runtime = Arc::new(Self {
            core: core.clone(),
            session,
            tools: tools.clone(),
            broadcaster: broadcaster.clone(),
            broadcast_log,
            notify,
            pump_lock: Mutex::new(()),
            anchors,
            active_turn_id: Arc::new(Mutex::new(None)),
            drain_seq: AtomicU64::new(0),
            drain_boot_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            drain_retry_scheduled: AtomicBool::new(false),
            drain_retry_attempts: AtomicU64::new(0),
        });
        runtime.spawn_observation_bridge();
        runtime.spawn_turn_pump();
        runtime.spawn_process_terminal_bridge(process_registry.clone(), store_factory.clone());
        runtime.spawn_subagent_control_bridge(process_registry.clone());
        runtime.spawn_timer_trigger_source(trigger_store);
        runtime
            .restore_subagent_processes_after_restart(
                process_registry.clone(),
                store_factory.clone(),
            )
            .await;
        runtime
            .abandon_recovered_subagent_runtime_processes(process_registry, store_factory)
            .await;
        runtime.resume_active_monitors().await;
        runtime.notify_if_work_pending().await;
        tracing::info!(
            model = %config.model,
            provider = ?config.provider_mode,
            data_dir = %config.data_dir.display(),
            "Lash Agent runtime opened session agent"
        );
        Ok(LashStartup::Ready(runtime))
    }

    async fn enqueue_inner(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let source_key = owner_turn_source_key(&turn.client_id);
        {
            let mut anchors = self.anchors.lock().await;
            anchors.pending_by_source_key.insert(
                source_key.clone(),
                TurnAnchors {
                    owner_message_id: turn.message_id,
                },
            );
        }
        let ingress = self.ingress_for_mode(turn.mode).await;
        let input = match owner_turn_input(&turn).await {
            Ok(input) => input,
            Err(error) => {
                self.anchors
                    .lock()
                    .await
                    .pending_by_source_key
                    .remove(&source_key);
                return Err(error);
            }
        };
        if let Err(error) = self
            .session
            .enqueue(input)
            .id(turn.client_id)
            .ingress(ingress)
            .send()
            .await
        {
            self.anchors
                .lock()
                .await
                .pending_by_source_key
                .remove(&source_key);
            return Err(error.into());
        }
        self.notify.notify_one();
        Ok(())
    }

    async fn ingress_for_mode(&self, mode: SendMode) -> TurnInputIngress {
        match mode {
            SendMode::NextTurn => TurnInputIngress::next_turn(),
            SendMode::Send => {
                let active_turn_id = self.active_turn_id.lock().await.clone();
                match active_turn_id {
                    Some(turn_id) => TurnInputIngress::active_turn(
                        turn_id,
                        TurnInputCheckpointBoundary::AfterWork,
                    ),
                    None => TurnInputIngress::next_turn(),
                }
            }
        }
    }

    async fn notify_if_work_pending(&self) {
        if self.work_pending().await {
            self.notify.notify_one();
        }
    }

    async fn work_pending(&self) -> bool {
        let pending_inputs = match self.session.pending_turn_inputs().await {
            Ok(inputs) => !inputs.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending Lash turn inputs");
                false
            }
        };
        let queued_work = match self.session.queued_work().await {
            Ok(work) => !work.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending Lash queued work");
                false
            }
        };
        pending_inputs || queued_work
    }

    /// Schedule a single delayed pump re-notify with exponential backoff
    /// (2s doubling to a 30s cap). Used when a queued-work drain came back
    /// empty while work is still pending: the session execution lease is held
    /// elsewhere, and without a retry the pending work would sit unclaimed
    /// forever (no other code path re-notifies the pump).
    fn schedule_drain_retry(self: &Arc<Self>) {
        if self.drain_retry_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let attempt = self.drain_retry_attempts.fetch_add(1, Ordering::AcqRel);
        let delay = Duration::from_secs((2u64 << attempt.min(4)).min(30));
        tracing::info!(
            attempt = attempt + 1,
            delay_secs = delay.as_secs(),
            "queued work is pending but the drain claimed nothing (session \
             execution lease busy); scheduling a delayed drain retry"
        );
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            runtime
                .drain_retry_scheduled
                .store(false, Ordering::Release);
            runtime.notify.notify_one();
        });
    }

    async fn restore_subagent_processes_after_restart(
        &self,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        let abandoned = match self.tools.restore_subagent_processes_after_restart().await {
            Ok(abandoned) => abandoned,
            Err(error) => {
                tracing::warn!(%error, "failed to restore Sub-agent process metadata at boot");
                return;
            }
        };
        for process_id in abandoned {
            if self
                .append_subagent_abandoned_event(
                    process_registry.as_ref(),
                    store_factory.as_ref(),
                    &process_id,
                )
                .await
            {
                self.notify.notify_one();
            }
        }
    }

    async fn abandon_recovered_subagent_runtime_processes(
        &self,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        let records = match process_registry.list_non_terminal().await {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(%error, "failed to list non-terminal Lash processes at boot");
                return;
            }
        };
        for record in records {
            if !is_hirsel_subagent_process_record(&record) {
                continue;
            }
            if self
                .append_subagent_abandoned_event(
                    process_registry.as_ref(),
                    store_factory.as_ref(),
                    &record.id,
                )
                .await
            {
                self.notify.notify_one();
            }
        }
    }

    async fn append_subagent_abandoned_event(
        &self,
        process_registry: &dyn lash::process::ProcessRegistry,
        store_factory: &dyn lash::persistence::SessionStoreFactory,
        process_id: &str,
    ) -> bool {
        let request =
            ProcessEventAppendRequest::new(SUBAGENT_ABANDONED, subagent_abandoned_payload())
                .with_replay_key(format!("hirsel-subagent:{process_id}:{SUBAGENT_ABANDONED}"));
        match process_registry.append_event(process_id, request).await {
            Ok(result) => match enqueue_process_wake(store_factory, result.wake_delivery).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        process_id = %process_id,
                        "failed to enqueue abandoned Sub-agent process wake"
                    );
                    false
                }
            },
            Err(error) => {
                tracing::warn!(
                    %error,
                    process_id = %process_id,
                    "failed to append abandoned Sub-agent process event"
                );
                match process_registry
                    .complete_process(
                        process_id,
                        subagent_abandoned_output(),
                        ProcessCompletionAuthority::ReconciledAbandon,
                    )
                    .await
                {
                    Ok(_) => true,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            process_id = %process_id,
                            "failed to complete recovered Sub-agent process as abandoned"
                        );
                        false
                    }
                }
            }
        }
    }

    async fn resume_active_monitors(&self) {
        let monitors = match self.tools.active_monitors().await {
            Ok(monitors) => monitors,
            Err(error) => {
                tracing::warn!(%error, "failed to list active monitors at boot");
                return;
            }
        };
        for monitor in monitors {
            let existing = match self.core.processes().get(&monitor.id).await {
                Ok(existing) => existing,
                Err(error) => {
                    tracing::warn!(%error, monitor_id = %monitor.id, "failed to inspect monitor process at boot");
                    continue;
                }
            };
            if existing.as_ref().is_some_and(|process| !process.terminal) {
                continue;
            }
            let scope = inline_trigger_scope(format!("monitor-resume:{}", monitor.id));
            if let Err(error) = self
                .core
                .processes()
                .start(monitor_start_request(&monitor, AGENT_SESSION_ID), scope)
                .await
            {
                tracing::warn!(%error, monitor_id = %monitor.id, "failed to resume monitor process");
            } else {
                self.tools.broadcast_monitor_upsert(&monitor);
            }
        }
        match self.core.durable_process_worker_config() {
            Ok(config) => {
                if let Err(error) = lash_core::DurableProcessWorker::new(config)
                    .drive_pending_processes()
                    .await
                {
                    tracing::warn!(%error, "failed to drive recovered monitor processes at boot");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to build monitor recovery worker");
            }
        }
    }

    fn spawn_turn_pump(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                runtime.notify.notified().await;
                let _guard = runtime.pump_lock.lock().await;
                loop {
                    let drain_id = runtime.next_drain_id();
                    runtime.activate_anchor_for_next_drain().await;
                    runtime.set_active_turn_id(Some(drain_id.clone())).await;
                    let result = runtime
                        .session
                        .queued_turn()
                        .drain_id(drain_id.clone())
                        .run()
                        .await;
                    runtime.clear_active_turn_id(&drain_id).await;
                    runtime.clear_active_anchor_and_prune().await;
                    match result {
                        Ok(Some(output)) => {
                            runtime.drain_retry_attempts.store(0, Ordering::Release);
                            let tool_calls = tool_call_summaries(&output);
                            let text = output
                                .assistant_message()
                                .map(str::to_owned)
                                .or_else(|| output.final_value().map(render_final_value));
                            if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                                runtime.deliver_turn_chat(text, tool_calls).await;
                            }
                            continue;
                        }
                        Ok(None) => {
                            // An empty drain while durable work is still queued
                            // means another owner's session execution lease is
                            // blocking the claim (e.g. a stale lease after an
                            // unclean shutdown). Nothing else re-notifies the
                            // pump, so schedule a bounded delayed retry until
                            // the lease expires or is reclaimed.
                            if runtime.work_pending().await {
                                runtime.schedule_drain_retry();
                            }
                            break;
                        }
                        Err(error) => {
                            runtime.handle_turn_error(error).await;
                            break;
                        }
                    }
                }
            }
        });
    }

    fn next_drain_id(&self) -> String {
        let seq = self.drain_seq.fetch_add(1, Ordering::Relaxed) + 1;
        // The boot epoch keeps drain replay keys unique across restarts:
        // a per-boot counter alone collides with drains already committed in
        // a persistent session store (store_commit_failed on first turn).
        format!("host-queue-drain:{}:{seq}", self.drain_boot_ms)
    }

    async fn set_active_turn_id(&self, id: Option<String>) {
        *self.active_turn_id.lock().await = id;
    }

    async fn clear_active_turn_id(&self, id: &str) {
        let mut active = self.active_turn_id.lock().await;
        if active.as_deref() == Some(id) {
            *active = None;
        }
    }

    async fn activate_anchor_for_next_drain(&self) {
        let pending = match self.session.pending_turn_inputs().await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending turn inputs for Ping anchor");
                self.anchors.lock().await.active = None;
                return;
            }
        };
        let mut anchors = self.anchors.lock().await;
        anchors.active = pending
            .iter()
            .filter_map(|input| input.source_key.as_ref())
            .find_map(|source_key| anchors.pending_by_source_key.get(source_key).cloned());
    }

    async fn clear_active_anchor_and_prune(&self) {
        let live_source_keys = match self.session.pending_turn_inputs().await {
            Ok(pending) => Some(
                pending
                    .into_iter()
                    .filter_map(|input| input.source_key)
                    .collect::<HashSet<_>>(),
            ),
            Err(error) => {
                tracing::warn!(%error, "failed to prune pending turn anchors");
                None
            }
        };
        let mut anchors = self.anchors.lock().await;
        anchors.active = None;
        if let Some(live_source_keys) = live_source_keys {
            anchors
                .pending_by_source_key
                .retain(|source_key, _| live_source_keys.contains(source_key));
        }
    }

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.session.cancel_running_turns();
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    async fn cancel_queued(&self, client_id: &str) -> anyhow::Result<CancelQueuedResult> {
        let target =
            lash::PendingTurnInputCancelTarget::source_key(owner_turn_source_key(client_id));
        let mut results = self.session.cancel_pending_turn_inputs([target]).await?;
        let outcome = results
            .pop()
            .map(|result| result.outcome)
            .unwrap_or(lash::PendingTurnInputCancelOutcome::NotFound);
        match outcome {
            lash::PendingTurnInputCancelOutcome::Cancelled(_) => Ok(CancelQueuedResult::Cancelled),
            lash::PendingTurnInputCancelOutcome::AlreadyClaimed { .. }
            | lash::PendingTurnInputCancelOutcome::AlreadyCompleted(_)
            | lash::PendingTurnInputCancelOutcome::AlreadyCancelled(_)
            | lash::PendingTurnInputCancelOutcome::NotFound => {
                Ok(CancelQueuedResult::AlreadyClaimed)
            }
        }
    }

    async fn start_monitor_process(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        self.core
            .processes()
            .start(
                monitor_start_request(record, AGENT_SESSION_ID),
                inline_trigger_scope(format!("monitor-debug-create:{}", record.id)),
            )
            .await?;
        self.notify.notify_one();
        Ok(())
    }

    async fn cancel_monitor_process(&self, monitor_id: &str) -> anyhow::Result<()> {
        match self
            .core
            .processes()
            .cancel(
                monitor_id,
                inline_trigger_scope(format!("monitor-debug-cancel:{monitor_id}")),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::debug!(%error, monitor_id = %monitor_id, "monitor process cancel returned an error");
                Ok(())
            }
        }
    }

    async fn enqueue_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        self.session
            .enqueue(TurnInput::text(text))
            .id(format!("monitor-wake-{}", Uuid::new_v4()))
            .ingress(TurnInputIngress::next_turn())
            .send()
            .await?;
        self.notify.notify_one();
        Ok(())
    }

    async fn deliver_turn_chat(&self, text: String, tool_calls: Vec<ToolCallSummary>) {
        match self
            .tools
            .chat_send_with_tool_calls(text, None, tool_calls)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "failed to deliver Agent turn output to Chat");
            }
        }
    }

    async fn handle_turn_error(&self, error: lash::EmbedError) {
        tracing::warn!(%error, "Lash queued turn failed");
        // No ref: an error right under the Owner's message renders as a noisy
        // self-quote in the client.
        match self
            .tools
            .chat_send(format!("Agent turn failed: {error}"), None)
            .await
        {
            Ok(_) => {}
            Err(chat_error) => {
                tracing::warn!(%chat_error, "failed to write Agent turn error to Chat");
            }
        }
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
    }

    fn spawn_observation_bridge(self: &Arc<Self>) {
        let session = self.session.clone();
        let broadcaster = self.broadcaster.clone();
        let broadcast_log = self.broadcast_log.clone();
        tokio::spawn(async move {
            let observable = session.observe();
            let current = observable.current_remote_observation();
            let mut cursor = RemoteSessionCursor::new(current.cursor);
            let mut timeline = TurnTimelineBridge::default();
            let mut retry = ObservationRetryBackoff::default();
            loop {
                let mut stream = match observable.subscribe_and_recover_remote(cursor.clone()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let delay = retry.next_delay();
                        tracing::warn!(%error, ?delay, "failed to subscribe to Lash observation stream; retrying");
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                };
                loop {
                    let keep_stream = if timeline.has_pending() {
                        let flush_delay = timeline.flush_delay();
                        tokio::select! {
                            item = stream.next() => {
                                handle_observation_stream_item(
                                    item,
                                    &broadcast_log,
                                    &broadcaster,
                                    &mut timeline,
                                )
                            }
                            () = tokio::time::sleep(flush_delay) => {
                                timeline.flush_pending(&broadcast_log, &broadcaster);
                                true
                            }
                        }
                    } else {
                        handle_observation_stream_item(
                            stream.next().await,
                            &broadcast_log,
                            &broadcaster,
                            &mut timeline,
                        )
                    };
                    cursor = stream.cursor();
                    if !keep_stream {
                        break;
                    }
                    retry.reset();
                }
                let delay = retry.next_delay();
                tracing::warn!(?delay, "Lash observation stream ended; resubscribing");
                tokio::time::sleep(delay).await;
            }
        });
    }

    fn spawn_process_terminal_bridge(
        self: &Arc<Self>,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        let mut events = self.tools.terminal_events();
        let notify = Arc::clone(&self.notify);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let (event_type, payload) = terminal_event_payload(&event.outcome);
                        let request =
                            ProcessEventAppendRequest::new(event_type, payload).with_replay_key(
                                format!("hirsel-subagent:{}:{event_type}", event.process_id),
                            );
                        match process_registry
                            .append_event(&event.process_id, request)
                            .await
                        {
                            Ok(result) => {
                                if let Err(error) = enqueue_process_wake(
                                    store_factory.as_ref(),
                                    result.wake_delivery,
                                )
                                .await
                                {
                                    tracing::warn!(%error, "failed to enqueue Lash process wake");
                                } else {
                                    notify.notify_one();
                                }
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %event.process_id,
                                    "failed to append Lash process terminal event"
                                );
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn spawn_subagent_control_bridge(
        self: &Arc<Self>,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
    ) {
        let tools = self.tools.clone();
        tokio::spawn(async move {
            let mut cancelled = HashSet::new();
            let mut abandoned = HashSet::new();
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let processes = match tools.subagents_list() {
                    Ok(processes) => processes,
                    Err(error) => {
                        tracing::warn!(%error, "failed to list Sub-agent processes for control bridge");
                        continue;
                    }
                };
                for process in processes {
                    if !matches!(process.status, crate::processes::ProcessStatus::Running) {
                        continue;
                    }
                    let process_id = process.id.clone();
                    let Some(record) = process_registry.get_process(&process_id).await else {
                        continue;
                    };
                    if record.abandon_request.is_some() && !abandoned.contains(&process_id) {
                        match tools.subagents_abandon_process(&process_id).await {
                            Ok(()) => {
                                abandoned.insert(process_id.clone());
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %process_id,
                                    "failed to abandon Sub-agent after Lash abandon request"
                                );
                            }
                        }
                        continue;
                    }
                    if cancelled.contains(&process_id) {
                        continue;
                    }
                    let events = match process_registry.events_after(&process_id, 0).await {
                        Ok(events) => events,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                process_id = %process_id,
                                "failed to read Sub-agent process events for control bridge"
                            );
                            continue;
                        }
                    };
                    if events
                        .iter()
                        .any(|event| event.event_type == "process.cancel_requested")
                    {
                        match tools.subagents_interrupt_process(&process_id).await {
                            Ok(()) => {
                                cancelled.insert(process_id.clone());
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %process_id,
                                    "failed to interrupt Sub-agent after Lash cancel request"
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    fn spawn_timer_trigger_source(self: &Arc<Self>, trigger_store: Arc<dyn TriggerStore>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = runtime.fire_due_timers(Arc::clone(&trigger_store)).await {
                    tracing::warn!(%error, "timer trigger source poll failed");
                }
            }
        });
    }

    async fn fire_due_timers(&self, trigger_store: Arc<dyn TriggerStore>) -> anyhow::Result<()> {
        let mut filter = TriggerSubscriptionFilter::for_session(AGENT_SESSION_ID);
        filter.source_type = Some(TIMER_SOURCE_TYPE.to_string());
        filter.enabled = Some(true);
        let records = trigger_store.list_subscriptions(filter).await?;
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        for record in records {
            let schedule = match TimerSchedule::from_registration(&record) {
                Ok(schedule) => schedule,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        handle = %record.handle,
                        source_key = %record.source_key,
                        "invalid timer trigger registration"
                    );
                    continue;
                }
            };
            let Some(occurrence) = schedule.due_occurrence(&record, now_ms) else {
                continue;
            };
            if let Err(error) = self
                .emit_timer_occurrence(Arc::clone(&trigger_store), &record, occurrence)
                .await
            {
                tracing::warn!(
                    %error,
                    handle = %record.handle,
                    source_key = %record.source_key,
                    "failed to emit timer trigger occurrence"
                );
            }
        }
        Ok(())
    }

    async fn emit_timer_occurrence(
        &self,
        trigger_store: Arc<dyn TriggerStore>,
        record: &lash_core::TriggerSubscriptionRecord,
        occurrence: TimerOccurrence,
    ) -> anyhow::Result<()> {
        let fired_at = Utc::now();
        let payload = json!({
            "label": occurrence.label,
            "fired_at": fired_at.to_rfc3339(),
            "scheduled_at": timestamp_ms_rfc3339(occurrence.scheduled_at_ms),
            "source_key": record.source_key,
            "handle": record.handle,
        });
        let report = self
            .core
            .triggers()
            .emit(
                lash::triggers::TriggerOccurrenceRequest::new(
                    TIMER_SOURCE_TYPE,
                    record.source_key.clone(),
                    payload,
                    occurrence.idempotency_key,
                )
                .with_source(record.source.clone()),
                inline_trigger_scope(format!(
                    "timer:{}:{}",
                    record.source_key, occurrence.scheduled_at_ms
                )),
            )
            .await?;
        if !report.deliveries.is_empty() {
            self.notify.notify_one();
        }
        if occurrence.one_shot {
            trigger_store
                .cancel_subscription(&record.registrant_scope_id(), &record.handle)
                .await?;
        }
        Ok(())
    }
}

struct HirselQueuedWorkNotifier {
    notify: Arc<Notify>,
}

#[async_trait]
impl QueuedWorkRunHandle for HirselQueuedWorkNotifier {
    async fn run_queued_work(&self, _request: QueuedWorkRunRequest) -> Result<(), PluginError> {
        self.notify.notify_one();
        Ok(())
    }
}

/// A stable per-machine id for lease owner liveness. Lease reclaim only
/// compares it between processes that already share the same session store
/// (a local sqlite file), so the hostname is plenty; the boot id and pid
/// carried alongside it do the real liveness discrimination.
fn local_host_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "local".to_string())
}

fn inline_trigger_scope(scope_id: impl Into<String>) -> lash_core::ScopedEffectController<'static> {
    lash_core::ScopedEffectController::shared(
        Arc::new(lash_core::InlineRuntimeEffectController),
        lash_core::ExecutionScope::runtime_operation(scope_id.into()),
    )
    .expect("inline timer trigger occurrence execution scope")
}

#[derive(Debug, Clone)]
struct TimerSchedule {
    label: String,
    at_ms: Option<u64>,
    in_secs: Option<u64>,
    every_secs: Option<u64>,
}

#[derive(Debug, Clone)]
struct TimerOccurrence {
    label: String,
    scheduled_at_ms: u64,
    idempotency_key: String,
    one_shot: bool,
}

impl TimerSchedule {
    fn from_registration(record: &lash_core::TriggerSubscriptionRecord) -> Result<Self, String> {
        let descriptor_type = record
            .source
            .get("$lash_host_descriptor_type")
            .and_then(Value::as_str);
        if descriptor_type != Some(TIMER_SOURCE_TYPE) {
            return Err(format!(
                "expected descriptor type `{TIMER_SOURCE_TYPE}`, got `{}`",
                descriptor_type.unwrap_or("<missing>")
            ));
        }
        let value = record
            .source
            .get("$lash_host_descriptor_value")
            .ok_or_else(|| "missing timer schedule descriptor value".to_string())?;
        let label = value
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|label| !label.trim().is_empty())
            .or_else(|| record.name.clone())
            .ok_or_else(|| "timer.Schedule requires non-empty `label`".to_string())?;
        let at_ms = value.get("at").map(parse_timer_at_ms).transpose()?;
        let in_secs = value.get("in_secs").map(parse_timer_secs).transpose()?;
        let every_secs = value
            .get("every_secs")
            .map(parse_timer_secs)
            .transpose()?
            .map(|secs| secs.max(TIMER_MIN_RECURRING_SECS));
        let configured = [at_ms.is_some(), in_secs.is_some(), every_secs.is_some()]
            .into_iter()
            .filter(|present| *present)
            .count();
        if configured != 1 {
            return Err(
                "timer.Schedule requires exactly one of `at`, `in_secs`, or `every_secs`"
                    .to_string(),
            );
        }
        Ok(Self {
            label,
            at_ms,
            in_secs,
            every_secs,
        })
    }

    fn due_occurrence(
        &self,
        record: &lash_core::TriggerSubscriptionRecord,
        now_ms: u64,
    ) -> Option<TimerOccurrence> {
        if let Some(every_secs) = self.every_secs {
            let interval_ms = every_secs.saturating_mul(1_000);
            let first_due_ms = record.created_at_ms.saturating_add(interval_ms);
            if now_ms < first_due_ms {
                return None;
            }
            let period_index = now_ms
                .saturating_sub(record.created_at_ms)
                .checked_div(interval_ms)
                .unwrap_or(0);
            if period_index == 0 {
                return None;
            }
            let scheduled_at_ms = record
                .created_at_ms
                .saturating_add(period_index.saturating_mul(interval_ms));
            return Some(TimerOccurrence {
                label: self.label.clone(),
                scheduled_at_ms,
                idempotency_key: format!("timer:{}:every:{period_index}", record.source_key),
                one_shot: false,
            });
        }

        let scheduled_at_ms = self
            .at_ms
            .or_else(|| {
                self.in_secs.map(|secs| {
                    record
                        .created_at_ms
                        .saturating_add(secs.saturating_mul(1_000))
                })
            })
            .expect("one-shot schedule was validated");
        (now_ms >= scheduled_at_ms).then(|| TimerOccurrence {
            label: self.label.clone(),
            scheduled_at_ms,
            idempotency_key: format!("timer:{}:once:{scheduled_at_ms}", record.source_key),
            one_shot: true,
        })
    }
}

fn parse_timer_secs(value: &Value) -> Result<u64, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .filter(|secs| *secs > 0)
            .ok_or_else(|| "timer seconds must be a positive integer".to_string()),
        other => Err(format!("timer seconds must be an integer, got {other}")),
    }
}

fn parse_timer_at_ms(value: &Value) -> Result<u64, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "`at` must be an RFC3339 timestamp string".to_string())?;
    let ts = DateTime::parse_from_rfc3339(text)
        .map_err(|error| format!("invalid `at` timestamp `{text}`: {error}"))?
        .with_timezone(&Utc)
        .timestamp_millis();
    Ok(ts.max(0) as u64)
}

fn timestamp_ms_rfc3339(timestamp_ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms as i64)
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| timestamp_ms.to_string())
}

async fn enqueue_process_wake(
    store_factory: &dyn lash::persistence::SessionStoreFactory,
    wake_delivery: Option<ProcessWakeDelivery>,
) -> anyhow::Result<()> {
    let Some(wake) = wake_delivery else {
        return Ok(());
    };
    let request = lash::persistence::SessionStoreCreateRequest {
        session_id: wake.target_session_id.clone(),
        relation: lash::persistence::SessionRelation::default(),
        policy: SessionPolicy::default(),
    };
    let Some(store) = store_factory
        .open_existing_store(&request)
        .await
        .map_err(anyhow::Error::msg)?
    else {
        return Ok(());
    };
    let draft = lash::persistence::QueuedWorkBatchDraft::new(
        wake.target_session_id.clone(),
        lash::persistence::DeliveryPolicy::EarliestSafeBoundary,
        lash::persistence::SlotPolicy::Exclusive,
        vec![lash::persistence::QueuedWorkPayload::process_wake(
            wake.clone(),
        )],
    )
    .with_source_key(format!(
        "process:{}:event:{}:wake",
        wake.process_id, wake.sequence
    ));
    store.enqueue_queued_work(draft).await?;
    Ok(())
}

fn render_final_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(other).unwrap_or_default()
        ),
    }
}

async fn owner_turn_input(turn: &OwnerTurn) -> anyhow::Result<TurnInput> {
    let mut items = vec![InputItem::text(owner_turn_text(turn))];
    let mut image_blobs = Vec::new();

    for attachment in &turn.attachments {
        if !attachment.blob.mime.starts_with("image/") {
            continue;
        }
        let id = attachment.blob.id.clone();
        let bytes = tokio::fs::read(&attachment.path)
            .await
            .with_context(|| format!("read image attachment {}", attachment.path.display()))?;
        items.push(InputItem::image_ref(id.clone()));
        image_blobs.push((id, bytes));
    }

    let mut input = TurnInput::items(items);
    for (id, bytes) in image_blobs {
        input = input.with_image_blob(id, bytes);
    }
    Ok(input)
}

fn owner_turn_source_key(client_id: &str) -> String {
    format!("host:{client_id}")
}

fn owner_turn_text(turn: &OwnerTurn) -> String {
    let mut text = match turn.anchor {
        Some(anchor) => format!("Owner replied to Ping anchor {anchor}.\n\n{}", turn.body),
        None => turn.body.clone(),
    };
    for attachment in &turn.attachments {
        text.push('\n');
        text.push_str(&format!(
            "[attachment stored at {}: {} ({}, {} bytes)]",
            attachment.path.display(),
            attachment.blob.name,
            attachment.blob.mime,
            attachment.blob.size
        ));
    }
    append_mentioned_ping_context(&mut text, &turn.mentioned_pings);
    text
}

pub(crate) fn append_mentioned_ping_context(text: &mut String, pings: &[Ping]) {
    for ping in pings {
        text.push('\n');
        text.push_str(&format!(
            "[mentioned ping @{} (ping_id {}, {}, requires_response={}, anchor {}): {}]",
            ping.name,
            ping.id,
            match ping.status {
                hirsel_proto::PingStatus::Open => "open",
                hirsel_proto::PingStatus::Done => "done",
            },
            ping.requires_response,
            ping.anchor,
            ping.description
        ));
    }
}

fn slow_turn_duration(body: &str) -> anyhow::Result<Option<Duration>> {
    let Some(rest) = body.trim_start().strip_prefix("slow:") else {
        return Ok(None);
    };
    let seconds_text = rest
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("slow turn hook requires seconds after `slow:`"))?;
    let seconds: f64 = seconds_text
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid slow turn seconds `{seconds_text}`: {error}"))?;
    if !(0.0..=600.0).contains(&seconds) {
        anyhow::bail!("slow turn seconds must be between 0 and 600");
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
}

async fn sleep_until_done_or_cancelled(
    duration: Duration,
    cancel: &lash::CancellationToken,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => true,
        () = cancel.cancelled() => false,
    }
}

fn handle_observation_stream_item<E>(
    item: Option<Result<RemoteSessionObservationStreamItem, E>>,
    broadcast_log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
    timeline: &mut TurnTimelineBridge,
) -> bool
where
    E: std::fmt::Display,
{
    match item {
        Some(Ok(RemoteSessionObservationStreamItem::Event(event))) => {
            if matches!(
                &event.event,
                RemoteSessionObservationEventPayload::Committed
            ) {
                timeline.observe(&event.event, broadcast_log, broadcaster);
                if let Some(activity) = activity_from_observation(&event.event) {
                    publish(broadcast_log, broadcaster, activity);
                }
            } else {
                if let Some(activity) = activity_from_observation(&event.event) {
                    publish(broadcast_log, broadcaster, activity);
                }
                timeline.observe(&event.event, broadcast_log, broadcaster);
            }
            true
        }
        Some(Ok(RemoteSessionObservationStreamItem::Gap { .. })) => {
            timeline.finish_turn(broadcast_log, broadcaster);
            publish(
                broadcast_log,
                broadcaster,
                HostToClient::AgentActivity {
                    state: AgentActivityState::Idle,
                    text: None,
                    sc: None,
                },
            );
            true
        }
        Some(Err(error)) => {
            timeline.finish_turn(broadcast_log, broadcaster);
            tracing::warn!(%error, "Lash observation stream failed");
            false
        }
        None => {
            timeline.finish_turn(broadcast_log, broadcaster);
            false
        }
    }
}

#[derive(Default)]
struct ObservationRetryBackoff {
    failures: u32,
}

impl ObservationRetryBackoff {
    fn next_delay(&mut self) -> Duration {
        let exponent = self.failures.min(5);
        self.failures = self.failures.saturating_add(1);
        Duration::from_millis(100).saturating_mul(1 << exponent)
    }

    fn reset(&mut self) {
        self.failures = 0;
    }
}

fn activity_from_observation(event: &RemoteSessionObservationEventPayload) -> Option<HostToClient> {
    match event {
        RemoteSessionObservationEventPayload::TurnActivity { activity } => match &activity.event {
            RemoteTurnEvent::ModelRequestStarted { .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some("thinking".to_string()),
            )),
            RemoteTurnEvent::AssistantProseDelta { text }
            | RemoteTurnEvent::ReasoningDelta { text } => Some(agent_activity(
                AgentActivityState::Thinking,
                latest_line(text),
            )),
            RemoteTurnEvent::ToolCallStarted { name, .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some(format!("tool {name}")),
            )),
            RemoteTurnEvent::ToolCallCompleted { name, .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some(format!("tool {name} completed")),
            )),
            RemoteTurnEvent::Error { message } => Some(agent_activity(
                AgentActivityState::Thinking,
                latest_line(message),
            )),
            _ => None,
        },
        RemoteSessionObservationEventPayload::Committed => {
            Some(agent_activity(AgentActivityState::Idle, None))
        }
        _ => None,
    }
}

const TURN_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(250);
const TURN_EVENT_BATCH_CHARS: usize = 400;
const TURN_EVENT_SUMMARY_CHARS: usize = 120;

#[derive(Default)]
struct TurnTimelineBridge {
    seq: u64,
    in_turn: bool,
    pending: Option<PendingTimelineText>,
    tool_id_seq: u64,
}

struct PendingTimelineText {
    kind: TimelineTextKind,
    text: String,
    started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineTextKind {
    Prose,
    Reasoning,
}

impl TurnTimelineBridge {
    fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn flush_delay(&self) -> Duration {
        self.pending
            .as_ref()
            .map(|pending| TURN_EVENT_BATCH_INTERVAL.saturating_sub(pending.started_at.elapsed()))
            .unwrap_or(TURN_EVENT_BATCH_INTERVAL)
    }

    fn observe(
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

    fn start_turn_if_needed(&mut self) {
        if !self.in_turn {
            self.seq = 0;
            self.pending = None;
            self.in_turn = true;
            self.tool_id_seq = 0;
        }
    }

    /// lash supplies call_id on native tool events; RLM cell executions may
    /// omit it, so fall back to a per-turn ordinal. Started/Completed arrive
    /// serially per call in RLM mode, so name+ordinal pairs stay aligned.
    fn tool_event_id(&mut self, call_id: Option<&str>, name: &str) -> String {
        match call_id {
            Some(id) => id.to_string(),
            None => {
                self.tool_id_seq += 1;
                format!("{name}:{}", self.tool_id_seq.div_ceil(2))
            }
        }
    }

    fn finish_turn(
        &mut self,
        broadcast_log: &BroadcastLog,
        broadcaster: &broadcast::Sender<HostToClient>,
    ) {
        self.flush_pending(broadcast_log, broadcaster);
        self.in_turn = false;
    }

    fn push_text(
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

    fn flush_pending(
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

    fn publish_event(
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

fn tool_call_summaries(output: &lash::TurnOutput) -> Vec<ToolCallSummary> {
    let summaries = output
        .result
        .tool_calls
        .iter()
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
            lash::TurnEvent::ToolCallCompleted { name, output, .. } => Some(ToolCallSummary {
                name: name.clone(),
                ok: output.is_success(),
            }),
            _ => None,
        })
        .collect()
}

fn condense_args(name: &str, payload: &Value) -> Option<String> {
    let summary = match name {
        "shell_run" => labeled_scalar(payload, "cmd", "cmd"),
        "pings_send" => {
            labeled_first_scalar(payload, &["content_md", "content", "body"], "content")
        }
        "pings_resolve" => scalar_any(payload, &["ping_id", "id"]).map(|id| format!("ping {id}")),
        "subagents_spawn" => {
            let agent = scalar_field(payload, "agent").unwrap_or_else(|| "subagent".to_string());
            scalar_any(payload, &["prompt", "task"]).map(|prompt| format!("{agent}: {prompt}"))
        }
        "subagents_prompt" => process_summary(payload).map(|process| {
            match scalar_any(payload, &["text", "prompt", "message"]) {
                Some(text) => format!("{process}: {text}"),
                None => process,
            }
        }),
        "subagents_interrupt" | "subagents_progress" | "subagents_wait" => process_summary(payload),
        "monitors_create" => labeled_first_scalar(payload, &["label", "cmd"], "monitor"),
        "monitors_cancel" => scalar_any(payload, &["monitor_id", "process_id", "id"])
            .map(|id| format!("monitor {}", tail_identifier(&id))),
        "monitors_list" | "subagents_list" => None,
        _ => first_string_field(payload).map(|(key, value)| format!("{key}: {value}")),
    };
    clean_summary(summary)
}

fn condense_result(name: &str, args: &Value, output: &Value) -> Option<String> {
    let ok = tool_output_ok(output);
    let prefix = if ok { "ok" } else { "err" };
    let payload = tool_output_payload(output).unwrap_or(output);
    let detail = match name {
        "shell_run" => shell_result_summary(payload),
        "pings_send" => scalar_field(payload, "ping_id").map(|id| format!("ping {id}")),
        "pings_resolve" => payload
            .get("ping")
            .and_then(|ping| scalar_field(ping, "ping_id"))
            .or_else(|| scalar_any(args, &["ping_id", "id"]))
            .map(|id| format!("ping {id}")),
        "subagents_spawn" => scalar_any(payload, &["process_id"])
            .or_else(|| {
                payload
                    .get("handle")
                    .and_then(|handle| scalar_field(handle, "process_id"))
            })
            .map(|id| format!("process {}", tail_identifier(&id))),
        "subagents_prompt" => process_summary(args),
        "subagents_interrupt" => process_summary(args),
        "subagents_progress" => process_summary(args),
        "subagents_wait" => scalar_any(payload, &["process_id"])
            .or_else(|| scalar_any(args, &["process_id"]))
            .map(|id| format!("process {}", tail_identifier(&id))),
        "monitors_create" => scalar_any(payload, &["monitor_id", "process_id"])
            .map(|id| format!("monitor {}", tail_identifier(&id))),
        "monitors_cancel" => scalar_any(payload, &["monitor_id"])
            .or_else(|| scalar_any(args, &["monitor_id", "process_id", "id"]))
            .map(|id| format!("monitor {}", tail_identifier(&id))),
        "monitors_list" => {
            scalar_count(payload, "monitors").map(|count| format!("{count} monitors"))
        }
        "subagents_list" => {
            scalar_count(payload, "processes").map(|count| format!("{count} processes"))
        }
        _ => first_scalar_field(payload).map(|(_, value)| value),
    }
    .or_else(|| failure_message(output));

    clean_summary(Some(match detail {
        Some(detail) if !detail.trim().is_empty() => format!("{prefix} {detail}"),
        _ => prefix.to_string(),
    }))
}

fn tool_output_ok(output: &Value) -> bool {
    output
        .pointer("/outcome/status")
        .and_then(Value::as_str)
        .map(|status| status == "success")
        .or_else(|| {
            output
                .get("status")
                .and_then(Value::as_str)
                .map(|status| status == "success" || status == "ok")
        })
        .or_else(|| output.get("ok").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn tool_output_payload(output: &Value) -> Option<&Value> {
    output.pointer("/outcome/payload")
}

fn shell_result_summary(payload: &Value) -> Option<String> {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("timed out".to_string());
    }
    scalar_field(payload, "status")
        .map(|status| format!("status {status}"))
        .or_else(|| {
            first_non_empty_string(payload, &["stderr", "stdout"])
                .map(|text| format!("output {text}"))
        })
}

fn failure_message(output: &Value) -> Option<String> {
    output
        .pointer("/outcome/payload/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            output
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn scalar_count(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.len().to_string())
}

fn process_summary(value: &Value) -> Option<String> {
    scalar_any(value, &["process_id", "id"]).map(|id| format!("process {}", tail_identifier(&id)))
}

fn labeled_scalar(value: &Value, key: &str, label: &str) -> Option<String> {
    scalar_field(value, key).map(|text| format!("{label}: {text}"))
}

fn labeled_first_scalar(value: &Value, keys: &[&str], label: &str) -> Option<String> {
    scalar_any(value, keys).map(|text| format!("{label}: {text}"))
}

fn scalar_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| scalar_field(value, key))
}

fn scalar_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(scalar_value)
}

fn scalar_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn first_non_empty_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .find_map(latest_line)
}

fn first_string_field(value: &Value) -> Option<(String, String)> {
    value.as_object()?.iter().find_map(|(key, value)| {
        value
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(|text| (key.clone(), text.to_string()))
    })
}

fn first_scalar_field(value: &Value) -> Option<(String, String)> {
    value.as_object()?.iter().find_map(|(key, value)| {
        scalar_value(value)
            .filter(|text| !text.trim().is_empty())
            .map(|text| (key.clone(), text))
    })
}

fn tail_identifier(value: &str) -> String {
    const MAX_ID_CHARS: usize = 24;
    if value.chars().count() <= MAX_ID_CHARS {
        return value.to_string();
    }
    let tail = value
        .chars()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}

fn clean_summary(summary: Option<String>) -> Option<String> {
    let without_braces = summary?
        .chars()
        .map(|ch| match ch {
            '{' | '}' => ' ',
            _ => ch,
        })
        .collect::<String>();
    let compact = without_braces
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        None
    } else {
        Some(truncate_chars(&compact, TURN_EVENT_SUMMARY_CHARS))
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut truncated = text.chars().take(max_chars - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

fn agent_activity(state: AgentActivityState, text: Option<String>) -> HostToClient {
    HostToClient::AgentActivity {
        state,
        text,
        sc: None,
    }
}

fn publish(
    broadcast_log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
    event: HostToClient,
) {
    broadcast_log.record(event.clone());
    let _ = broadcaster.send(event);
}

fn latest_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            const MAX_CHARS: usize = 180;
            if line.chars().count() <= MAX_CHARS {
                line.to_string()
            } else {
                let mut truncated = line.chars().take(MAX_CHARS - 3).collect::<String>();
                truncated.push_str("...");
                truncated
            }
        })
}

#[derive(Clone)]
struct DegradedAgentRuntime {
    reason: String,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
}

impl DegradedAgentRuntime {
    async fn enqueue(&self, _turn: OwnerTurn) -> anyhow::Result<()> {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                text: Some("provider unavailable".to_string()),
                sc: None,
            },
        );
        self.tools
            .chat_send(format!("Agent turn failed: {}", self.reason), None)
            .await?;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    async fn deliver_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        self.tools.chat_send(text, None).await?;
        Ok(())
    }

    async fn cancel_queued(&self, _client_id: &str) -> anyhow::Result<CancelQueuedResult> {
        Ok(CancelQueuedResult::AlreadyClaimed)
    }
}

struct ProviderUnavailable {
    message: String,
}

async fn build_provider(config: &RuntimeConfig) -> Result<ProviderHandle, ProviderUnavailable> {
    match config.provider_mode {
        ProviderMode::Anthropic => {
            let Some(api_key) = config.anthropic_api_key.clone() else {
                return Err(ProviderUnavailable {
                    message: "ANTHROPIC_API_KEY is not set for HIRSEL_PROVIDER=anthropic"
                        .to_string(),
                });
            };
            Ok(ProviderHandle::new(
                lash_provider_anthropic::AnthropicProvider::new(api_key)
                    .with_options(ProviderOptions {
                        expose_thinking: true,
                        ..ProviderOptions::default()
                    })
                    .into_components(),
            ))
        }
        ProviderMode::Codex => {
            let tokens = load_codex_tokens()
                .await
                .map_err(|message| ProviderUnavailable { message })?;
            Ok(ProviderHandle::new(
                lash_provider_openai::CodexProvider::new(
                    tokens.access_token,
                    tokens.refresh_token,
                    tokens.expires_at,
                )
                .with_account_id(tokens.account_id)
                .with_options(ProviderOptions {
                    expose_thinking: true,
                    ..ProviderOptions::default()
                })
                .into_components(),
            ))
        }
    }
}

#[derive(Debug)]
struct CodexTokens {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    tokens: CodexAuthTokens,
}

#[derive(Debug, Deserialize)]
struct CodexAuthTokens {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
}

async fn load_codex_tokens() -> Result<CodexTokens, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; cannot locate ~/.codex/auth.json".to_string())?;
    let auth_path = home.join(".codex").join("auth.json");
    let text = tokio::fs::read_to_string(&auth_path)
        .await
        .map_err(|error| format!("failed to read {}: {error}", auth_path.display()))?;
    let auth: CodexAuthFile =
        serde_json::from_str(&text).map_err(|error| format!("invalid Codex auth JSON: {error}"))?;
    if auth.tokens.access_token.is_empty() || auth.tokens.refresh_token.is_empty() {
        return Err("Codex OAuth tokens are missing access_token or refresh_token".to_string());
    }
    Ok(CodexTokens {
        access_token: auth.tokens.access_token,
        refresh_token: auth.tokens.refresh_token,
        expires_at: auth.tokens.expires_at.unwrap_or(0),
        account_id: auth.tokens.account_id,
    })
}

#[derive(Clone)]
struct HirselToolExecutor {
    tools: ToolSuite,
    anchors: Arc<Mutex<TurnAnchorState>>,
}

#[async_trait]
impl StaticToolExecute for HirselToolExecutor {
    async fn execute(&self, call: ToolCall<'_>) -> ToolResult {
        match self.execute_inner(call).await {
            Ok(value) => ToolResult::ok(value),
            Err(error) => ToolResult::err_fmt(error),
        }
    }
}

impl HirselToolExecutor {
    async fn execute_inner(&self, call: ToolCall<'_>) -> Result<Value, String> {
        match call.name {
            "pings_send" => self.pings_send(call.args).await,
            "pings_resolve" => self.pings_resolve(call.args).await,
            "subagents_spawn" => self.subagents_spawn(call.args, call.context).await,
            "subagents_prompt" => self.subagents_prompt(call.args).await,
            "subagents_interrupt" => self.subagents_interrupt(call.args).await,
            "subagents_list" => self.subagents_list().await,
            "subagents_progress" => self.subagents_progress(call.args).await,
            "subagents_wait" => self.subagents_wait(call.args, call.context).await,
            "monitors_create" => self.monitors_create(call.args, call.context).await,
            "monitors_list" => self.monitors_list().await,
            "monitors_cancel" => self.monitors_cancel(call.args, call.context).await,
            "shell_run" => self.shell_run(call.args).await,
            other => Err(format!("Unknown tool: {other}")),
        }
    }

    async fn pings_send(&self, args: &Value) -> Result<Value, String> {
        let name = required_string_any(args, &["name"])?;
        let description = required_string_any(args, &["description"])?;
        let content = required_string_any(args, &["content_md", "content", "body"])?;
        let requires_response = args
            .get("requires_response")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let quick_replies = args
            .get("quick_replies")
            .cloned()
            .map(serde_json::from_value::<Vec<QuickReply>>)
            .transpose()
            .map_err(|error| format!("invalid quick_replies: {error}"))?
            .unwrap_or_default();
        let anchor = self
            .current_anchor()
            .await
            .ok_or_else(|| "pings.send requires an active Owner turn anchor".to_string())?;
        let ping = self
            .tools
            .pings_send(
                name,
                description,
                content,
                anchor,
                requires_response,
                quick_replies,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(pings_send_result(&ping))
    }

    async fn pings_resolve(&self, args: &Value) -> Result<Value, String> {
        let ping_id = required_u64_any(args, &["ping_id", "id"])?;
        let ping = self
            .tools
            .pings_resolve(ping_id)
            .await
            .map_err(|error| error.to_string())?;
        pings_resolve_result(ping.as_ref())
    }

    async fn subagents_spawn(
        &self,
        args: &Value,
        context: &lash::tools::ToolContext<'_>,
    ) -> Result<Value, String> {
        let agent = parse_agent_kind(
            args.get("agent")
                .and_then(Value::as_str)
                .unwrap_or("claude"),
        )?;
        let model = optional_string(args, "model")?;
        let prompt = required_string_any(args, &["prompt", "task"])?;
        let cwd = optional_path(args, "cwd")?
            .map(Ok)
            .unwrap_or_else(std::env::current_dir)
            .map_err(|error| format!("failed to resolve cwd: {error}"))?;
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        let request = ProcessStartRequest::external(
            process_id.clone(),
            ProcessOriginator::session(SessionScope::new(context.session_id())),
            json!({
                "kind": HIRSEL_SUBAGENT_ENGINE,
                "agent": agent,
                "model": model,
                "prompt": prompt,
                "cwd": cwd,
            }),
        )
        .with_wake_target(Some(SessionScope::new(AGENT_SESSION_ID)))
        .with_event_types(subagent_event_types());
        let handle = context
            .processes()
            .start(request)
            .await
            .map_err(|error| error.to_string())?;
        if let Err(error) = self
            .tools
            .subagents_spawn_with_process_id(agent, model.clone(), prompt, cwd, process_id.clone())
            .await
        {
            let _ = context
                .processes()
                .complete_external(
                    &process_id,
                    cancelled_await_output(format!("failed to start Sub-agent Driver: {error}")),
                )
                .await;
            return Err(format!("failed to start Sub-agent Driver: {error}"));
        }
        Ok(subagent_spawn_result(&handle.process_id))
    }

    async fn subagents_prompt(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let text = required_string_any(args, &["text", "prompt", "message"])?;
        self.tools
            .subagents_prompt_process(&process_id, text)
            .await
            .map_err(|error| error.to_string())?;
        Ok(acknowledgement_result())
    }

    async fn subagents_interrupt(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        self.tools
            .subagents_interrupt_process(&process_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(acknowledgement_result())
    }

    async fn subagents_list(&self) -> Result<Value, String> {
        let processes = self
            .tools
            .subagents_list()
            .map_err(|error| error.to_string())?;
        subagents_list_result(&processes)
    }

    async fn subagents_progress(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let process = self
            .tools
            .subagents_process(&process_id)
            .map_err(|error| error.to_string())?;
        let events = self
            .tools
            .subagents_progress(&process_id)
            .map_err(|error| error.to_string())?;
        subagents_progress_result(process.as_ref(), &events)
    }

    async fn subagents_wait(
        &self,
        args: &Value,
        context: &lash::tools::ToolContext<'_>,
    ) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let outcome = context
            .processes()
            .await_process(&process_id)
            .await
            .map_err(|error| error.to_string())?;
        subagents_wait_result(&process_id, &outcome)
    }

    async fn shell_run(&self, args: &Value) -> Result<Value, String> {
        let cmd = required_string(args, "cmd")?;
        let cwd = optional_path(args, "cwd")?;
        let timeout_secs = args.get("timeout_secs").and_then(Value::as_u64);
        let output = self
            .tools
            .shell_run(cmd, cwd, timeout_secs)
            .await
            .map_err(|error| error.to_string())?;
        shell_run_result(&output)
    }

    async fn monitors_create(
        &self,
        args: &Value,
        context: &lash::tools::ToolContext<'_>,
    ) -> Result<Value, String> {
        let cmd = required_string(args, "cmd")?;
        let every_secs = args
            .get("every_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .max(30);
        let wake_on = parse_monitor_wake_on(required_string(args, "wake_on")?.as_str())?;
        let pattern = optional_string(args, "pattern")?;
        if matches!(wake_on, MonitorWakeOn::Regex) && pattern.is_none() {
            return Err("pattern is required when wake_on is regex".to_string());
        }
        let label = required_string(args, "label")?;
        let record = self
            .tools
            .monitors_create(cmd, every_secs, wake_on, pattern, label)
            .await
            .map_err(|error| error.to_string())?;
        let request = monitor_start_request(&record, context.session_id());
        let start = context.processes().start(request).await;
        if let Err(error) = start {
            let _ = self.tools.monitors_cancel(&record.id).await;
            return Err(error.to_string());
        }
        monitors_create_result(&record)
    }

    async fn monitors_list(&self) -> Result<Value, String> {
        let monitors = self
            .tools
            .monitors_list()
            .await
            .map_err(|error| error.to_string())?;
        monitors_list_result(&monitors)
    }

    async fn monitors_cancel(
        &self,
        args: &Value,
        context: &lash::tools::ToolContext<'_>,
    ) -> Result<Value, String> {
        let monitor_id = required_string_any(args, &["monitor_id", "process_id", "id"])?;
        let record = self
            .tools
            .monitors_cancel(&monitor_id)
            .await
            .map_err(|error| error.to_string())?;
        if record.is_none() {
            return Err(format!("monitor not found: {monitor_id}"));
        }
        let cancel = context.processes().cancel(&monitor_id).await;
        if let Err(error) = cancel {
            tracing::debug!(%error, monitor_id = %monitor_id, "monitor process cancel returned an error");
        }
        Ok(monitors_cancel_result(&monitor_id))
    }

    async fn current_anchor(&self) -> Option<u64> {
        self.anchors
            .lock()
            .await
            .active
            .as_ref()
            .map(|anchors| anchors.owner_message_id)
    }
}

fn pings_send_result(ping: &hirsel_proto::Ping) -> Value {
    json!({
        "ping_id": ping.id,
        "anchor": ping.anchor,
        "requires_response": ping.requires_response,
    })
}

fn pings_resolve_result(ping: Option<&hirsel_proto::Ping>) -> Result<Value, String> {
    let ping = ping.map(ping_result).transpose()?;
    Ok(json!({ "ping": ping }))
}

fn ping_result(ping: &hirsel_proto::Ping) -> Result<Value, String> {
    let mut value = serde_json::to_value(ping).map_err(|error| error.to_string())?;
    rename_result_id(&mut value, "ping_id")?;
    Ok(value)
}

fn subagent_spawn_result(process_id: &str) -> Value {
    json!({ "process_id": process_id })
}

fn acknowledgement_result() -> Value {
    json!({ "ok": true })
}

fn subagents_list_result(processes: &[crate::processes::ProcessRecord]) -> Result<Value, String> {
    let processes = processes
        .iter()
        .map(subagent_process_result)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "processes": processes }))
}

fn subagents_progress_result(
    process: Option<&crate::processes::ProcessRecord>,
    events: &[hirsel_drivers::SubagentEvent],
) -> Result<Value, String> {
    let process = process.map(subagent_process_result).transpose()?;
    Ok(json!({
        "process": process,
        "events": events,
    }))
}

fn subagent_process_result(process: &crate::processes::ProcessRecord) -> Result<Value, String> {
    let mut value = serde_json::to_value(process).map_err(|error| error.to_string())?;
    rename_result_id(&mut value, "process_id")?;
    Ok(value)
}

fn subagents_wait_result(process_id: &str, outcome: &ProcessAwaitOutput) -> Result<Value, String> {
    serde_json::to_value(json!({
        "process_id": process_id,
        "outcome": outcome,
    }))
    .map_err(|error| error.to_string())
}

fn shell_run_result(output: &crate::tools::ShellRunOutput) -> Result<Value, String> {
    serde_json::to_value(output).map_err(|error| error.to_string())
}

fn monitors_create_result(record: &MonitorRecord) -> Result<Value, String> {
    Ok(json!({
        "monitor_id": record.id,
        "monitor": monitor_result(record)?,
    }))
}

fn monitors_list_result(monitors: &[MonitorRecord]) -> Result<Value, String> {
    let monitors = monitors
        .iter()
        .map(monitor_result)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "monitors": monitors }))
}

fn monitor_result(record: &MonitorRecord) -> Result<Value, String> {
    let mut value = serde_json::to_value(record).map_err(|error| error.to_string())?;
    rename_result_id(&mut value, "monitor_id")?;
    Ok(value)
}

fn monitors_cancel_result(monitor_id: &str) -> Value {
    json!({ "ok": true, "monitor_id": monitor_id })
}

fn rename_result_id(value: &mut Value, result_name: &str) -> Result<(), String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "tool result record must serialize as an object".to_string())?;
    let id = object
        .remove("id")
        .ok_or_else(|| "tool result record is missing its id".to_string())?;
    object.insert(result_name.to_string(), id);
    Ok(())
}

#[derive(Clone)]
struct HirselProcessPluginFactory {
    tools: ToolSuite,
    notify: Arc<Notify>,
}

impl PluginFactory for HirselProcessPluginFactory {
    fn id(&self) -> &'static str {
        "hirsel_processes"
    }

    fn extension_contributions(&self) -> Vec<PluginExtensionContribution> {
        match PluginExtensionContribution::new(
            lash::rlm::LASHLANG_SURFACE_EXTENSION_ID,
            hirsel_lashlang_surface(),
        ) {
            Ok(contribution) => vec![contribution],
            Err(error) => {
                tracing::warn!(%error, "failed to encode Hirsel lashlang surface contribution");
                Vec::new()
            }
        }
    }

    fn process_engine_contributions(
        &self,
        _ctx: &ProcessEngineContributionContext<'_>,
    ) -> Result<Vec<Arc<dyn ProcessEngine>>, PluginError> {
        Ok(vec![
            Arc::new(HirselSubagentEngine {
                tools: self.tools.clone(),
            }),
            Arc::new(HirselMonitorEngine {
                tools: self.tools.clone(),
                notify: Arc::clone(&self.notify),
            }),
        ])
    }

    fn build(&self, _ctx: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        Ok(Arc::new(EmptyHirselSessionPlugin))
    }
}

struct EmptyHirselSessionPlugin;

impl SessionPlugin for EmptyHirselSessionPlugin {
    fn id(&self) -> &'static str {
        "hirsel_processes"
    }

    fn register(&self, _reg: &mut PluginRegistrar) -> Result<(), PluginError> {
        Ok(())
    }
}

fn hirsel_lashlang_surface() -> lash::rlm::LashlangSurfaceContribution {
    let mut resources = lash::rlm::LashlangHostCatalog::new();
    resources
        .add_trigger_source_constructor(
            ["timer", "Schedule"],
            lash::rlm::TypeExpr::Object(vec![
                lash::rlm::TypeField {
                    name: "label".into(),
                    ty: lash::rlm::TypeExpr::Str,
                    optional: false,
                },
                lash::rlm::TypeField {
                    name: "at".into(),
                    ty: lash::rlm::TypeExpr::Str,
                    optional: true,
                },
                lash::rlm::TypeField {
                    name: "in_secs".into(),
                    ty: lash::rlm::TypeExpr::Int,
                    optional: true,
                },
                lash::rlm::TypeField {
                    name: "every_secs".into(),
                    ty: lash::rlm::TypeExpr::Int,
                    optional: true,
                },
            ]),
            lash::rlm::NamedDataType::object(
                TIMER_EVENT_TYPE,
                vec![
                    lash::rlm::TypeField {
                        name: "label".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "fired_at".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "scheduled_at".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "source_key".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                    lash::rlm::TypeField {
                        name: "handle".into(),
                        ty: lash::rlm::TypeExpr::Str,
                        optional: false,
                    },
                ],
            )
            .expect("valid timer.Tick type"),
        )
        .expect("valid timer.Schedule trigger source");
    lash::rlm::LashlangSurfaceContribution::new(
        lash::rlm::LashlangAbilities::default(),
        lash::rlm::LashlangLanguageFeatures::default(),
        resources,
    )
}

#[derive(Clone)]
struct HirselSubagentEngine {
    tools: ToolSuite,
}

#[async_trait]
impl ProcessEngine for HirselSubagentEngine {
    fn kind(&self) -> &'static str {
        HIRSEL_SUBAGENT_ENGINE
    }

    async fn validate_start(
        &self,
        _context: ProcessEngineValidationContext<'_>,
        payload: &Value,
        _env_spec: Option<&lash::process::ProcessExecutionEnvSpec>,
    ) -> Result<(), PluginError> {
        SubagentProcessPayload::from_value(payload).map(|_| ())
    }

    async fn run(&self, context: ProcessEngineRunContext<'_>, payload: Value) -> ProcessRunOutcome {
        let process_id = context.registration().id.to_string();
        let payload = match SubagentProcessPayload::from_value(&payload) {
            Ok(payload) => payload,
            Err(error) => return cancelled_await_output(error.to_string()).into(),
        };
        if let Err(error) = self
            .tools
            .subagents_spawn_with_process_id(
                payload.agent,
                payload.model,
                payload.prompt,
                payload.cwd,
                process_id.clone(),
            )
            .await
        {
            return cancelled_await_output(format!("failed to start Sub-agent Driver: {error}"))
                .into();
        }
        match ProcessAwaiter::polling(context.registry())
            .await_terminal(&process_id)
            .await
        {
            Ok(output) => output.into(),
            Err(error) => {
                cancelled_await_output(format!("failed to await Sub-agent: {error}")).into()
            }
        }
    }

    fn identity(&self, payload: &Value) -> ProcessIdentity {
        let label = SubagentProcessPayload::from_value(payload)
            .ok()
            .map(|payload| format!("{:?}: {}", payload.agent, short_label(&payload.prompt)));
        ProcessIdentity::new(HIRSEL_SUBAGENT_ENGINE).with_label(label)
    }

    fn durability_tier(&self) -> DurabilityTier {
        DurabilityTier::Durable
    }
}

#[derive(Clone)]
struct HirselMonitorEngine {
    tools: ToolSuite,
    notify: Arc<Notify>,
}

#[async_trait]
impl ProcessEngine for HirselMonitorEngine {
    fn kind(&self) -> &'static str {
        HIRSEL_MONITOR_ENGINE
    }

    async fn validate_start(
        &self,
        _context: ProcessEngineValidationContext<'_>,
        payload: &Value,
        _env_spec: Option<&lash::process::ProcessExecutionEnvSpec>,
    ) -> Result<(), PluginError> {
        MonitorProcessPayload::from_value(payload).map(|_| ())
    }

    async fn run(&self, context: ProcessEngineRunContext<'_>, payload: Value) -> ProcessRunOutcome {
        let payload = match MonitorProcessPayload::from_value(&payload) {
            Ok(payload) => payload,
            Err(error) => return cancelled_await_output(error.to_string()).into(),
        };
        let process_id = context.registration().id.to_string();
        let cancellation = context.cancellation_token();
        let registry = context.registry();
        let store_factory = context.session_store_factory();
        drop(context);
        loop {
            let record = match self.tools.monitor(&payload.monitor_id).await {
                Ok(Some(record)) if record.cancelled_ts.is_none() => record,
                Ok(Some(_)) => {
                    return cancelled_await_output("monitor cancelled".to_string()).into();
                }
                Ok(None) => {
                    return cancelled_await_output("monitor spec missing".to_string()).into();
                }
                Err(error) => {
                    return cancelled_await_output(format!("monitor lookup failed: {error}"))
                        .into();
                }
            };
            tokio::select! {
                () = cancellation.cancelled() => {
                    return cancelled_await_output("monitor cancelled".to_string()).into();
                }
                () = tokio::time::sleep(Duration::from_secs(record.every_secs)) => {}
            }
            let record = match self.tools.monitor(&payload.monitor_id).await {
                Ok(Some(record)) if record.cancelled_ts.is_none() => record,
                Ok(Some(_)) => {
                    return cancelled_await_output("monitor cancelled".to_string()).into();
                }
                Ok(None) => {
                    return cancelled_await_output("monitor spec missing".to_string()).into();
                }
                Err(error) => {
                    return cancelled_await_output(format!("monitor lookup failed: {error}"))
                        .into();
                }
            };
            let tick = run_monitor_tick(&record).await;
            let updated = match self
                .tools
                .record_monitor_tick(
                    &payload.monitor_id,
                    tick.probe.output.clone(),
                    tick.summary.clone(),
                )
                .await
            {
                Ok(Some(updated)) => updated,
                Ok(None) => {
                    return cancelled_await_output("monitor cancelled".to_string()).into();
                }
                Err(error) => {
                    tracing::warn!(%error, monitor_id = %payload.monitor_id, "failed to persist monitor tick");
                    continue;
                }
            };
            if !tick.wake {
                continue;
            }
            if let Err(error) = append_monitor_wake(
                registry.as_ref(),
                store_factory.as_deref(),
                &process_id,
                &updated,
                &tick,
                &self.notify,
            )
            .await
            {
                tracing::warn!(%error, monitor_id = %payload.monitor_id, "failed to append monitor wake event");
            }
        }
    }

    fn identity(&self, payload: &Value) -> ProcessIdentity {
        let label = MonitorProcessPayload::from_value(payload)
            .ok()
            .map(|payload| payload.label);
        ProcessIdentity::new(HIRSEL_MONITOR_ENGINE).with_label(label)
    }

    fn durability_tier(&self) -> DurabilityTier {
        DurabilityTier::Durable
    }
}

struct MonitorProcessPayload {
    monitor_id: String,
    label: String,
}

impl MonitorProcessPayload {
    fn from_value(value: &Value) -> Result<Self, PluginError> {
        let monitor_id = required_string(value, "monitor_id").map_err(PluginError::Session)?;
        let label = required_string(value, "label").map_err(PluginError::Session)?;
        Ok(Self { monitor_id, label })
    }
}

struct SubagentProcessPayload {
    agent: AgentKind,
    model: Option<String>,
    prompt: String,
    cwd: PathBuf,
}

impl SubagentProcessPayload {
    fn from_value(value: &Value) -> Result<Self, PluginError> {
        let agent = parse_agent_kind(value.get("agent").and_then(Value::as_str).unwrap_or(""))
            .map_err(PluginError::Session)?;
        let model = optional_string(value, "model").map_err(PluginError::Session)?;
        let prompt = required_string(value, "prompt").map_err(PluginError::Session)?;
        let cwd = optional_path(value, "cwd")
            .map_err(PluginError::Session)?
            .ok_or_else(|| PluginError::Session("cwd is required".to_string()))?;
        Ok(Self {
            agent,
            model,
            prompt,
            cwd,
        })
    }
}

fn is_hirsel_subagent_process_record(record: &lash_core::ProcessRecord) -> bool {
    match record.input.as_ref() {
        ProcessInput::Engine { kind, .. } => kind == HIRSEL_SUBAGENT_ENGINE,
        ProcessInput::External { metadata } => metadata
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == HIRSEL_SUBAGENT_ENGINE),
        _ => false,
    }
}

fn cancelled_await_output(message: String) -> ProcessAwaitOutput {
    ProcessAwaitOutput::Cancelled {
        message,
        raw: None,
        control: None,
    }
}

fn subagent_event_types() -> Vec<ProcessEventType> {
    vec![
        terminal_event_type(SUBAGENT_COMPLETED, ProcessTerminalState::Completed),
        terminal_event_type(SUBAGENT_FAILED, ProcessTerminalState::Failed),
        terminal_event_type(SUBAGENT_CANCELLED, ProcessTerminalState::Cancelled),
        terminal_event_type(SUBAGENT_ABANDONED, ProcessTerminalState::Abandoned),
    ]
}

fn monitor_start_request(record: &MonitorRecord, session_id: &str) -> ProcessStartRequest {
    ProcessStartRequest::new(
        record.id.clone(),
        ProcessInput::Engine {
            kind: HIRSEL_MONITOR_ENGINE.to_string(),
            payload: json!({
                "monitor_id": record.id,
                "label": record.label,
            }),
        },
        RecoveryDisposition::Rerunnable,
        ProcessOriginator::session(SessionScope::new(session_id)),
    )
    .with_wake_target(Some(SessionScope::new(AGENT_SESSION_ID)))
    .with_event_types(monitor_event_types())
}

fn monitor_event_types() -> Vec<ProcessEventType> {
    vec![ProcessEventType {
        name: MONITOR_WAKE_EVENT.to_string(),
        payload_schema: LashSchema::new(json!({
            "type": "object",
            "additionalProperties": true,
            "required": ["text", "label", "output_tail"],
            "properties": {
                "text": { "type": "string" },
                "label": { "type": "string" },
                "output_tail": { "type": "string" }
            }
        })),
        semantics: ProcessEventSemanticsSpec {
            terminal: None,
            wake: Some(ProcessWakeSpec {
                when: None,
                input: ProcessValueSelector::Pointer("/text".to_string()),
                dedupe_key: ProcessWakeDedupeKey::EventIdentity,
            }),
        },
    }]
}

async fn append_monitor_wake(
    registry: &dyn lash::process::ProcessRegistry,
    store_factory: Option<&dyn lash::persistence::SessionStoreFactory>,
    process_id: &str,
    record: &MonitorRecord,
    tick: &crate::monitors::MonitorTick,
    notify: &Notify,
) -> anyhow::Result<()> {
    let text = tick.wake_text.clone().unwrap_or_else(|| {
        format!(
            "Monitor `{}` fired.\n\n{}",
            record.label,
            output_tail(&tick.probe.output, 4 * 1024)
        )
    });
    let run_key = record
        .last_run_ts
        .map(|ts| ts.timestamp_millis().to_string())
        .unwrap_or_else(|| Utc::now().timestamp_millis().to_string());
    let request = ProcessEventAppendRequest::new(
        MONITOR_WAKE_EVENT,
        json!({
            "text": text,
            "label": record.label,
            "output_tail": output_tail(&tick.probe.output, 4 * 1024),
        }),
    )
    .with_replay_key(format!("hirsel-monitor:{}:{run_key}", record.id));
    let result = registry.append_event(process_id, request).await?;
    if let Some(store_factory) = store_factory {
        enqueue_process_wake(store_factory, result.wake_delivery).await?;
        notify.notify_one();
    }
    Ok(())
}

fn terminal_event_type(name: &str, state: ProcessTerminalState) -> ProcessEventType {
    ProcessEventType {
        name: name.to_string(),
        payload_schema: LashSchema::new(json!({
            "type": "object",
            "additionalProperties": true,
            "required": ["text", "await_output"],
            "properties": {
                "text": { "type": "string" },
                "await_output": { "type": "object" }
            }
        })),
        semantics: ProcessEventSemanticsSpec {
            terminal: Some(ProcessTerminalSpec {
                state,
                await_output: Some(ProcessValueSelector::Pointer("/await_output".to_string())),
            }),
            wake: Some(ProcessWakeSpec {
                when: None,
                input: ProcessValueSelector::Pointer("/text".to_string()),
                dedupe_key: ProcessWakeDedupeKey::EventIdentity,
            }),
        },
    }
}

fn terminal_event_payload(outcome: &TerminalOutcome) -> (&'static str, Value) {
    match outcome {
        TerminalOutcome::Done { summary } => (
            SUBAGENT_COMPLETED,
            json!({
                "text": format!("Sub-agent completed: {summary}"),
                "await_output": {
                    "type": "success",
                    "value": { "summary": summary },
                }
            }),
        ),
        TerminalOutcome::Failed { reason } => (
            SUBAGENT_FAILED,
            json!({
                "text": format!("Sub-agent failed: {reason}"),
                "await_output": {
                    "type": "failure",
                    "class": "execution",
                    "code": "subagent_failed",
                    "message": reason,
                    "raw": { "reason": reason },
                }
            }),
        ),
        TerminalOutcome::Interrupted => (
            SUBAGENT_CANCELLED,
            json!({
                "text": "Sub-agent was interrupted.",
                "await_output": {
                    "type": "cancelled",
                    "message": "Sub-agent was interrupted.",
                }
            }),
        ),
    }
}

fn subagent_abandoned_payload() -> Value {
    json!({
        "text": "Sub-agent was abandoned after host restart.",
        "await_output": {
            "type": "abandoned",
            "evidence": {
                "writer": "reconciled_request",
                "owner": null,
                "epoch_ms": Utc::now().timestamp_millis().max(0) as u64,
            }
        }
    })
}

fn subagent_abandoned_output() -> ProcessAwaitOutput {
    ProcessAwaitOutput::Abandoned {
        evidence: Box::new(lash_core::AbandonEvidence {
            writer: lash_core::AbandonWriter::OwnerDrain,
            owner: None,
            epoch_ms: Utc::now().timestamp_millis().max(0) as u64,
        }),
        control: None,
    }
}

fn hirsel_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_definition(
            "hirsel.pings_send",
            "pings_send",
            "Send a Ping anchored to the current Agent turn.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "description", "content_md"],
                "properties": {
                    "name": { "type": "string", "minLength": 1, "maxLength": 32 },
                    "description": { "type": "string", "minLength": 1 },
                    "content_md": { "type": "string" },
                    "requires_response": { "type": "boolean", "default": true },
                    "quick_replies": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["value", "label"],
                            "properties": {
                                "value": { "type": "string" },
                                "label": { "type": "string" }
                            }
                        }
                    }
                }
            }),
            pings_send_output_schema(),
            ["pings"],
            "send",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.pings_resolve",
            "pings_resolve",
            "Resolve a Ping that was overtaken by events.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["ping_id"],
                "properties": {
                    "ping_id": { "type": "integer", "minimum": 1 }
                }
            }),
            pings_resolve_output_schema(),
            ["pings"],
            "resolve",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_spawn",
            "subagents_spawn",
            "Start a Claude or Codex Sub-agent as a Lash Runtime Process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["agent", "prompt"],
                "properties": {
                    "agent": { "type": "string", "enum": ["claude", "codex"] },
                    "model": { "type": "string" },
                    "prompt": { "type": "string" },
                    "cwd": { "type": "string" }
                }
            }),
            subagents_spawn_output_schema(),
            ["subagents"],
            "spawn",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_prompt",
            "subagents_prompt",
            "Send follow-up input to a running Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id", "text"],
                "properties": {
                    "process_id": { "type": "string" },
                    "text": { "type": "string" }
                }
            }),
            acknowledgement_output_schema(),
            ["subagents"],
            "prompt",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_interrupt",
            "subagents_interrupt",
            "Request interruption of a running Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            acknowledgement_output_schema(),
            ["subagents"],
            "interrupt",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_list",
            "subagents_list",
            "List known Sub-agent processes.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            subagents_list_output_schema(),
            ["subagents"],
            "list",
            ToolScheduling::Parallel,
        ),
        tool_definition(
            "hirsel.subagents_progress",
            "subagents_progress",
            "Read recent progress events for a Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            subagents_progress_output_schema(),
            ["subagents"],
            "progress",
            ToolScheduling::Parallel,
        ),
        tool_definition(
            "hirsel.subagents_wait",
            "subagents_wait",
            "Wait for a Sub-agent process to reach a terminal outcome.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            subagents_wait_output_schema(),
            ["subagents"],
            "wait",
            ToolScheduling::Parallel,
        ),
        tool_definition(
            "hirsel.monitors_create",
            "monitors_create",
            "Create a persisted host monitor that wakes the Agent when its condition fires.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["cmd", "wake_on", "label"],
                "properties": {
                    "cmd": { "type": "string" },
                    "every_secs": { "type": "integer", "minimum": 30 },
                    "wake_on": {
                        "type": "string",
                        "enum": ["changed", "exit_zero", "exit_nonzero", "regex"]
                    },
                    "pattern": { "type": "string" },
                    "label": { "type": "string" }
                }
            }),
            monitors_create_output_schema(),
            ["monitors"],
            "create",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.monitors_list",
            "monitors_list",
            "List persisted host monitors.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            monitors_list_output_schema(),
            ["monitors"],
            "list",
            ToolScheduling::Parallel,
        ),
        tool_definition(
            "hirsel.monitors_cancel",
            "monitors_cancel",
            "Cancel a persisted host monitor.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["monitor_id"],
                "properties": {
                    "monitor_id": { "type": "string" }
                }
            }),
            monitors_cancel_output_schema(),
            ["monitors"],
            "cancel",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.shell_run",
            "shell_run",
            "Run a bounded shell command and return stdout, stderr, status, and timeout state.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["cmd"],
                "properties": {
                    "cmd": { "type": "string" },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 600 }
                }
            }),
            shell_run_output_schema(),
            ["shell"],
            "run",
            ToolScheduling::Serial,
        ),
    ]
}

fn pings_send_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ping_id", "anchor", "requires_response"],
        "properties": {
            "ping_id": { "type": "integer", "minimum": 1 },
            "anchor": { "type": "integer", "minimum": 1 },
            "requires_response": { "type": "boolean" }
        }
    })
}

fn pings_resolve_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ping"],
        "properties": {
            "ping": {
                "oneOf": [
                    ping_output_schema(),
                    { "type": "null" }
                ]
            }
        }
    })
}

fn ping_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "ping_id",
            "name",
            "description",
            "content",
            "anchor",
            "requires_response",
            "quick_replies",
            "status",
            "read",
            "ts"
        ],
        "properties": {
            "ping_id": { "type": "integer", "minimum": 1 },
            "name": { "type": "string", "minLength": 1, "maxLength": 32 },
            "description": { "type": "string", "minLength": 1 },
            "content": { "type": "string" },
            "anchor": { "type": "integer", "minimum": 1 },
            "requires_response": { "type": "boolean" },
            "quick_replies": {
                "type": "array",
                "items": quick_reply_output_schema()
            },
            "status": { "type": "string", "enum": ["open", "done"] },
            "read": { "type": "boolean" },
            "ts": timestamp_output_schema()
        }
    })
}

fn quick_reply_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["value", "label"],
        "properties": {
            "value": { "type": "string" },
            "label": { "type": "string" }
        }
    })
}

fn subagents_spawn_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["process_id"],
        "properties": {
            "process_id": { "type": "string", "minLength": 1 }
        }
    })
}

fn acknowledgement_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok"],
        "properties": {
            "ok": { "const": true }
        }
    })
}

fn subagents_list_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["processes"],
        "properties": {
            "processes": {
                "type": "array",
                "items": subagent_process_output_schema()
            }
        }
    })
}

fn subagents_progress_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["process", "events"],
        "properties": {
            "process": {
                "oneOf": [
                    subagent_process_output_schema(),
                    { "type": "null" }
                ]
            },
            "events": {
                "type": "array",
                "items": subagent_event_output_schema()
            }
        }
    })
}

fn subagent_process_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "process_id",
            "agent",
            "handle",
            "prompt",
            "cwd",
            "external_id",
            "status",
            "events",
            "started_ts",
            "last_event_ts"
        ],
        "properties": {
            "process_id": { "type": "string", "minLength": 1 },
            "agent": { "type": "string", "enum": ["claude", "codex"] },
            "model": { "type": "string" },
            "handle": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "agent"],
                "properties": {
                    "id": { "type": "string", "minLength": 1 },
                    "agent": { "type": "string", "enum": ["claude", "codex"] }
                }
            },
            "prompt": { "type": "string" },
            "cwd": { "type": "string" },
            "external_id": { "type": ["string", "null"] },
            "status": {
                "type": "string",
                "enum": ["running", "done", "failed", "interrupted", "abandoned"]
            },
            "events": {
                "type": "array",
                "items": subagent_event_output_schema()
            },
            "started_ts": timestamp_output_schema(),
            "last_event_ts": timestamp_output_schema()
        }
    })
}

fn subagent_event_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "external_id"],
                "properties": {
                    "type": { "const": "started" },
                    "external_id": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "summary"],
                "properties": {
                    "type": { "const": "progress" },
                    "summary": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "outcome"],
                "properties": {
                    "type": { "const": "terminal" },
                    "outcome": terminal_outcome_output_schema()
                }
            }
        ]
    })
}

fn terminal_outcome_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["status", "summary"],
                "properties": {
                    "status": { "const": "done" },
                    "summary": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["status", "reason"],
                "properties": {
                    "status": { "const": "failed" },
                    "reason": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["status"],
                "properties": {
                    "status": { "const": "interrupted" }
                }
            }
        ]
    })
}

fn subagents_wait_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["process_id", "outcome"],
        "properties": {
            "process_id": { "type": "string", "minLength": 1 },
            "outcome": process_await_output_schema()
        }
    })
}

fn process_await_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "value"],
                "properties": {
                    "type": { "const": "success" },
                    "value": true
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "class", "code", "message"],
                "properties": {
                    "type": { "const": "failure" },
                    "class": {
                        "type": "string",
                        "enum": [
                            "invalid_request",
                            "unavailable",
                            "permission_denied",
                            "timeout",
                            "execution",
                            "external",
                            "resource_limit",
                            "internal"
                        ]
                    },
                    "code": { "type": "string" },
                    "message": { "type": "string" },
                    "raw": true
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "message"],
                "properties": {
                    "type": { "const": "cancelled" },
                    "message": { "type": "string" },
                    "raw": true
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "evidence"],
                "properties": {
                    "type": { "const": "abandoned" },
                    "evidence": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["writer", "epoch_ms"],
                        "properties": {
                            "writer": {
                                "type": "string",
                                "enum": ["owner_drain", "sweep", "reconciled_request"]
                            },
                            "epoch_ms": { "type": "integer", "minimum": 0 }
                        }
                    }
                }
            }
        ]
    })
}

fn monitors_create_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["monitor_id", "monitor"],
        "properties": {
            "monitor_id": { "type": "string", "minLength": 1 },
            "monitor": monitor_output_schema()
        }
    })
}

fn monitors_list_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["monitors"],
        "properties": {
            "monitors": {
                "type": "array",
                "items": monitor_output_schema()
            }
        }
    })
}

fn monitor_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "monitor_id",
            "cmd",
            "every_secs",
            "wake_on",
            "label",
            "created_ts",
            "last_event_ts"
        ],
        "properties": {
            "monitor_id": { "type": "string", "minLength": 1 },
            "cmd": { "type": "string" },
            "every_secs": { "type": "integer", "minimum": 30 },
            "wake_on": {
                "type": "string",
                "enum": ["changed", "exit_zero", "exit_nonzero", "regex"]
            },
            "pattern": { "type": "string" },
            "label": { "type": "string" },
            "created_ts": timestamp_output_schema(),
            "last_event_ts": timestamp_output_schema(),
            "last_run_ts": timestamp_output_schema(),
            "last_output": { "type": "string" },
            "summary": { "type": "string" },
            "cancelled_ts": timestamp_output_schema()
        }
    })
}

fn monitors_cancel_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok", "monitor_id"],
        "properties": {
            "ok": { "const": true },
            "monitor_id": { "type": "string", "minLength": 1 }
        }
    })
}

fn shell_run_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "stdout", "stderr", "timed_out"],
        "properties": {
            "status": { "type": ["integer", "null"] },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "timed_out": { "type": "boolean" }
        }
    })
}

fn timestamp_output_schema() -> Value {
    json!({ "type": "string", "format": "date-time" })
}

#[allow(clippy::too_many_arguments)]
fn tool_definition(
    id: &str,
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
    module_path: impl IntoIterator<Item = &'static str>,
    operation: &str,
    scheduling: ToolScheduling,
) -> ToolDefinition {
    ToolDefinition::raw(id, name, description, input_schema, output_schema)
        .with_lashlang_binding(LashlangToolBinding::new(module_path, operation))
        .with_scheduling(scheduling)
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{key}`"))
}

fn required_string_any(args: &Value, keys: &[&str]) -> Result<String, String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{}`", keys.join("` or `")))
}

fn required_u64_any(args: &Value, keys: &[&str]) -> Result<u64, String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_u64))
        .ok_or_else(|| format!("missing required integer field `{}`", keys.join("` or `")))
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
            .map(Some)
            .ok_or_else(|| format!("field `{key}` must be a non-empty string")),
    }
}

fn optional_path(args: &Value, key: &str) -> Result<Option<PathBuf>, String> {
    args.get(key)
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| format!("field `{key}` must be a string path"))
        })
        .transpose()
}

fn parse_agent_kind(value: &str) -> Result<AgentKind, String> {
    match value {
        "claude" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        other => Err(format!("agent must be claude or codex, got `{other}`")),
    }
}

fn parse_monitor_wake_on(value: &str) -> Result<MonitorWakeOn, String> {
    match value {
        "changed" => Ok(MonitorWakeOn::Changed),
        "exit_zero" => Ok(MonitorWakeOn::ExitZero),
        "exit_nonzero" => Ok(MonitorWakeOn::ExitNonzero),
        "regex" => Ok(MonitorWakeOn::Regex),
        other => Err(format!(
            "wake_on must be changed, exit_zero, exit_nonzero, or regex, got `{other}`"
        )),
    }
}

fn short_label(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 80;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

struct ScriptedAgentRuntime {
    config: RuntimeConfig,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    state: Arc<Mutex<ScriptedQueueState>>,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct ScriptedQueueState {
    queue: VecDeque<OwnerTurn>,
    active: Option<ScriptedActiveTurn>,
}

struct ScriptedActiveTurn {
    cancel: lash::CancellationToken,
}

impl ScriptedAgentRuntime {
    async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        #[cfg(test)]
        if turn.body == "__hirsel_test_enqueue_error__" {
            anyhow::bail!("scripted enqueue failed for test");
        }
        self.state.lock().await.queue.push_back(turn);
        self.notify.notify_one();
        Ok(())
    }

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        if let Some(cancel) = self
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.cancel.clone())
        {
            cancel.cancel();
        }
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    async fn cancel_queued(&self, client_id: &str) -> anyhow::Result<CancelQueuedResult> {
        let mut state = self.state.lock().await;
        if let Some(position) = state
            .queue
            .iter()
            .position(|turn| turn.client_id == client_id)
        {
            state.queue.remove(position);
            return Ok(CancelQueuedResult::Cancelled);
        }
        Ok(CancelQueuedResult::AlreadyClaimed)
    }

    async fn deliver_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                text: Some("monitor wake".to_string()),
                sc: None,
            },
        );
        self.tools.chat_send(text, None).await?;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    async fn spawn_active_standalone_monitors(self: Arc<Self>) {
        match self.tools.active_monitors().await {
            Ok(monitors) => {
                for monitor in monitors {
                    self.spawn_standalone_monitor(monitor.id);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to resume scripted standalone monitors");
            }
        }
    }

    fn spawn_standalone_monitor(self: &Arc<Self>, monitor_id: String) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let record = match runtime.tools.monitor(&monitor_id).await {
                    Ok(Some(record)) if record.cancelled_ts.is_none() => record,
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor lookup failed");
                        break;
                    }
                };
                tokio::time::sleep(Duration::from_secs(record.every_secs)).await;
                let record = match runtime.tools.monitor(&monitor_id).await {
                    Ok(Some(record)) if record.cancelled_ts.is_none() => record,
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor lookup failed");
                        break;
                    }
                };
                let tick = run_monitor_tick(&record).await;
                match runtime
                    .tools
                    .record_monitor_tick(&monitor_id, tick.probe.output.clone(), tick.summary)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor tick persist failed");
                        continue;
                    }
                }
                if tick.wake
                    && let Some(text) = tick.wake_text
                    && let Err(error) = runtime.deliver_monitor_wake(text).await
                {
                    tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor wake delivery failed");
                }
            }
        });
    }

    async fn run(self: Arc<Self>) {
        tracing::info!(
            model = %self.config.model,
            data_dir = %self.config.data_dir.display(),
            "Scripted Agent test double opened session agent"
        );
        loop {
            let (turn, cancel) = loop {
                if let Some(next) = self.claim_next_turn().await {
                    break next;
                }
                self.notify.notified().await;
            };
            if let Err(error) = self.handle_turn(turn, cancel.clone()).await {
                tracing::error!(%error, "scripted Agent turn failed");
            }
            self.clear_active_turn(&cancel).await;
        }
    }

    async fn claim_next_turn(&self) -> Option<(OwnerTurn, lash::CancellationToken)> {
        let mut state = self.state.lock().await;
        let turn = state.queue.pop_front()?;
        let cancel = lash::CancellationToken::new();
        state.active = Some(ScriptedActiveTurn {
            cancel: cancel.clone(),
        });
        Some((turn, cancel))
    }

    async fn clear_active_turn(&self, _cancel: &lash::CancellationToken) {
        self.state.lock().await.active = None;
    }

    async fn handle_turn(
        &self,
        turn: OwnerTurn,
        cancel: lash::CancellationToken,
    ) -> anyhow::Result<()> {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                text: Some("processing owner message".to_string()),
                sc: None,
            },
        );
        let result = self.handle_turn_inner(&turn, &cancel).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        result
    }

    async fn handle_turn_inner(
        &self,
        turn: &OwnerTurn,
        cancel: &lash::CancellationToken,
    ) -> anyhow::Result<()> {
        if let Some(duration) = slow_turn_duration(&turn.body)?
            && !sleep_until_done_or_cancelled(duration, cancel).await
        {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Ok(());
        }
        self.emit_scripted_timeline().await;
        let turn_text = owner_turn_text(turn);
        let lower = turn_text.to_lowercase();
        if self.config.driver_mode == DriverMode::Fake && lower.contains("delegate") {
            return self.handle_fake_delegation(turn).await;
        }
        if turn.anchor.is_some() {
            self.tools
                .chat_send(
                    "Acknowledged. I will continue from that Ping reply.",
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        if lower.contains("pong") {
            self.tools.chat_send("pong", Some(turn.message_id)).await?;
            return Ok(());
        }
        if !turn.attachments.is_empty() {
            self.tools
                .chat_send(
                    format!("Scripted turn input:\n\n{turn_text}"),
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        self.tools
            .chat_send(
                "I received the Owner message. This scripted Agent mode is a deterministic test double; set HIRSEL_AGENT=lash for the real RLM runtime.",
                Some(turn.message_id),
            )
            .await?;
        Ok(())
    }

    async fn emit_scripted_timeline(&self) {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                seq: 1,
                event: TurnEventKind::Prose {
                    text: "I am checking the scripted path before replying.".to_string(),
                },
                sc: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                seq: 2,
                event: TurnEventKind::ToolStart {
                    id: "scripted-tool-1".to_string(),
                    name: "scripted_double".to_string(),
                    summary: Some("deterministic branch".to_string()),
                },
                sc: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                seq: 3,
                event: TurnEventKind::ToolDone {
                    id: "scripted-tool-1".to_string(),
                    name: "scripted_double".to_string(),
                    ok: true,
                    summary: Some("ok fixture selected".to_string()),
                },
                sc: None,
            },
        );
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                seq: 4,
                event: TurnEventKind::Prose {
                    text: "The scripted response is ready.".to_string(),
                },
                sc: None,
            },
        );
    }

    async fn handle_fake_delegation(&self, turn: &OwnerTurn) -> anyhow::Result<()> {
        let anchor = self
            .tools
            .chat_send(
                "I delegated the repo fix to a Sub-agent and will send the result as a Ping.",
                Some(turn.message_id),
            )
            .await?;
        let mut terminal_events = self.tools.terminal_events();
        let cwd = std::env::current_dir()?;
        let process = self
            .tools
            .subagents_spawn(
                AgentKind::Claude,
                None,
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
                            .pings_send(
                                "delegated-fix-ready",
                                "The delegated fix is ready for review",
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
                            tracing::warn!(%error, "failed to send Sub-agent terminal Ping");
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        processes::{ProcessRecord, ProcessStatus, ProcessStore},
        storage::Storage,
        tools::{ShellRunOutput, ToolsConfig},
    };
    use chrono::Utc;
    use hirsel_drivers::{SessionHandle, SubagentEvent};
    use hirsel_proto::{Blob, ChatAuthor, Ping, PingStatus};
    use lash_core::{
        ProcessExecutionEnvRef, ProcessIdentity, ProcessInput, ProcessOriginator, SessionScope,
        TriggerInputBinding, TriggerSubscriptionRecord,
    };

    use super::*;

    #[test]
    fn observation_resubscribe_backoff_grows_and_resets() {
        let mut backoff = ObservationRetryBackoff::default();
        let first = backoff.next_delay();
        assert!(backoff.next_delay() > first);
        backoff.reset();
        assert_eq!(backoff.next_delay(), first);
    }

    #[test]
    fn timeline_flushes_prose_before_tool_events() {
        let broadcast_log = BroadcastLog::default();
        let (broadcaster, _) = broadcast::channel(16);
        let mut timeline = TurnTimelineBridge::default();

        timeline.observe(
            &remote_turn_activity(RemoteTurnEvent::ModelRequestStarted {
                protocol_iteration: 0,
            }),
            &broadcast_log,
            &broadcaster,
        );
        timeline.observe(
            &remote_turn_activity(RemoteTurnEvent::AssistantProseDelta {
                text: "I will ".to_string(),
            }),
            &broadcast_log,
            &broadcaster,
        );
        timeline.observe(
            &remote_turn_activity(RemoteTurnEvent::AssistantProseDelta {
                text: "check now.".to_string(),
            }),
            &broadcast_log,
            &broadcaster,
        );
        assert!(turn_events(&broadcast_log).is_empty());

        timeline.observe(
            &remote_turn_activity(RemoteTurnEvent::ToolCallStarted {
                call_id: Some("call-1".to_string()),
                name: "shell_run".to_string(),
                args: serde_json::json!({ "cmd": "true" }),
                graph_key: None,
                parent_call_id: None,
            }),
            &broadcast_log,
            &broadcaster,
        );
        timeline.observe(
            &remote_turn_activity(RemoteTurnEvent::ToolCallCompleted {
                call_id: Some("call-1".to_string()),
                name: "shell_run".to_string(),
                args: serde_json::json!({ "cmd": "true" }),
                output: serde_json::json!({
                    "outcome": {
                        "status": "success",
                        "payload": {
                            "status": 0,
                            "stdout": "",
                            "stderr": "",
                            "timed_out": false
                        }
                    }
                }),
                duration_ms: 12,
                graph_key: None,
                parent_call_id: None,
            }),
            &broadcast_log,
            &broadcaster,
        );

        let events = turn_events(&broadcast_log);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].0, 1);
        assert_eq!(
            events[0].1,
            TurnEventKind::Prose {
                text: "I will check now.".to_string()
            }
        );
        assert_eq!(events[1].0, 2);
        assert_eq!(
            events[1].1,
            TurnEventKind::ToolStart {
                id: "call-1".to_string(),
                name: "shell_run".to_string(),
                summary: Some("cmd: true".to_string())
            }
        );
        assert_eq!(events[2].0, 3);
        assert_eq!(
            events[2].1,
            TurnEventKind::ToolDone {
                id: "call-1".to_string(),
                name: "shell_run".to_string(),
                ok: true,
                summary: Some("ok status 0".to_string())
            }
        );
    }

    #[test]
    fn tool_arg_summaries_are_condensed_and_not_json() {
        let summary = condense_args(
            "shell_run",
            &serde_json::json!({
                "cmd": "printf '{\"raw\":true}' && echo done",
                "timeout_secs": 30
            }),
        )
        .unwrap();

        assert!(summary.starts_with("cmd: printf"));
        assert!(!summary.contains("{\""));
        assert!(!summary.contains('{'));
        assert!(!summary.contains('}'));
        assert!(summary.chars().count() <= TURN_EVENT_SUMMARY_CHARS);
    }

    #[test]
    fn tool_result_summaries_include_status_and_error_hint() {
        let ok = condense_result(
            "shell_run",
            &serde_json::json!({ "cmd": "true" }),
            &serde_json::json!({
                "outcome": {
                    "status": "success",
                    "payload": {
                        "status": 0,
                        "stdout": "",
                        "stderr": "",
                        "timed_out": false
                    }
                }
            }),
        )
        .unwrap();
        assert_eq!(ok, "ok status 0");

        let err = condense_result(
            "shell_run",
            &serde_json::json!({ "cmd": "bad" }),
            &serde_json::json!({
                "outcome": {
                    "status": "failure",
                    "payload": {
                        "message": "failed with {\"raw\":true}"
                    }
                }
            }),
        )
        .unwrap();
        assert_eq!(err, "err failed with \"raw\":true");
        assert!(!err.contains("{\""));
    }

    #[tokio::test]
    async fn owner_turn_input_notes_all_attachments_and_references_images() {
        let dir = tempfile::tempdir().unwrap();
        let text_path = dir.path().join("text-blob");
        let image_path = dir.path().join("image-blob");
        tokio::fs::write(&text_path, b"hello").await.unwrap();
        tokio::fs::write(&image_path, [137, 80, 78, 71])
            .await
            .unwrap();
        let text = stored_blob("text-1", "note.txt", "text/plain", 5, text_path);
        let image = stored_blob("image-1", "tiny.png", "image/png", 4, image_path);
        let turn = OwnerTurn {
            message_id: 1,
            client_id: "client-1".to_string(),
            body: "see attached".to_string(),
            anchor: None,
            attachments: vec![text.clone(), image.clone()],
            mentioned_pings: Vec::new(),
            mode: SendMode::Send,
        };

        let rendered = owner_turn_text(&turn);
        assert!(rendered.contains(&format!(
            "[attachment stored at {}: note.txt (text/plain, 5 bytes)]",
            text.path.display()
        )));
        assert!(rendered.contains(&format!(
            "[attachment stored at {}: tiny.png (image/png, 4 bytes)]",
            image.path.display()
        )));

        let input = owner_turn_input(&turn).await.unwrap();
        assert_eq!(input.items.len(), 2);
        assert!(matches!(input.items[0], InputItem::Text { .. }));
        assert!(matches!(&input.items[1], InputItem::ImageRef { id } if id == "image-1"));
        assert_eq!(
            input.image_blobs.get("image-1").unwrap().as_slice(),
            &[137, 80, 78, 71]
        );
        assert!(!input.image_blobs.contains_key("text-1"));
    }

    #[test]
    fn owner_turn_text_expands_mentioned_ping_context() {
        let turn = OwnerTurn {
            message_id: 2,
            client_id: "mention-1".to_string(),
            body: "What changed?".to_string(),
            anchor: None,
            attachments: Vec::new(),
            mentioned_pings: vec![Ping {
                id: 7,
                name: "release-choice".to_string(),
                description: "Choose the release channel".to_string(),
                content: "Longer details".to_string(),
                anchor: 3,
                requires_response: true,
                quick_replies: Vec::new(),
                status: PingStatus::Done,
                read: true,
                ts: Utc::now(),
            }],
            mode: SendMode::Send,
        };

        assert_eq!(
            owner_turn_text(&turn),
            "What changed?\n[mentioned ping @release-choice (ping_id 7, done, requires_response=true, anchor 3): Choose the release channel]"
        );
    }

    #[tokio::test]
    async fn pings_send_uses_active_turn_anchor_when_later_owner_message_is_pending() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let owner_a = storage
            .append_chat(ChatAuthor::Owner, "owner A", None)
            .await
            .unwrap();
        let owner_b = storage
            .append_chat(ChatAuthor::Owner, "owner B", None)
            .await
            .unwrap();
        let (broadcaster, _) = broadcast::channel(16);
        let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
        let tools = ToolSuite::new(
            ToolsConfig {
                driver_mode: DriverMode::Fake,
                fake_fixture: None,
            },
            storage,
            broadcaster,
            BroadcastLog::default(),
            ProcessStore::default(),
            pushes,
        );
        let anchors = Arc::new(Mutex::new(TurnAnchorState::default()));
        {
            let mut anchors = anchors.lock().await;
            anchors.pending_by_source_key.insert(
                owner_turn_source_key("client-a"),
                TurnAnchors {
                    owner_message_id: owner_a.id,
                },
            );
            anchors.active = Some(TurnAnchors {
                owner_message_id: owner_a.id,
            });
            anchors.pending_by_source_key.insert(
                owner_turn_source_key("client-b"),
                TurnAnchors {
                    owner_message_id: owner_b.id,
                },
            );
        }
        let executor = HirselToolExecutor { tools, anchors };

        let result = executor
            .pings_send(&serde_json::json!({
                "name": "active-turn-result",
                "description": "Result for the active turn",
                "content": "A result for the active turn",
                "requires_response": true
            }))
            .await
            .unwrap();

        assert_eq!(result["anchor"], owner_a.id);
        assert_eq!(result["ping_id"], 1);
    }

    #[test]
    fn every_executor_result_matches_its_declared_output_schema() {
        let now = Utc::now();
        let ping = Ping {
            id: 7,
            name: "choose-release".to_string(),
            description: "Choose a release channel".to_string(),
            content: "Choose a release".to_string(),
            anchor: 3,
            requires_response: true,
            quick_replies: vec![QuickReply {
                value: "stable".to_string(),
                label: "Stable".to_string(),
            }],
            status: PingStatus::Done,
            read: true,
            ts: now,
        };
        let events = vec![
            SubagentEvent::Started {
                external_id: "driver-session-1".to_string(),
            },
            SubagentEvent::Progress {
                summary: "running tests".to_string(),
            },
            SubagentEvent::Terminal {
                outcome: TerminalOutcome::Done {
                    summary: "tests passed".to_string(),
                },
            },
        ];
        let process = ProcessRecord::restored(
            "proc-1".to_string(),
            AgentKind::Codex,
            Some("gpt-test".to_string()),
            SessionHandle {
                id: "driver-session-1".to_string(),
                agent: AgentKind::Codex,
            },
            "Run the tests".to_string(),
            "/tmp/repo".to_string(),
            Some("external-1".to_string()),
            ProcessStatus::Done,
            events.clone(),
            now,
            now,
        );
        let monitor = MonitorRecord {
            id: "monitor-1".to_string(),
            cmd: "test -f done".to_string(),
            every_secs: 30,
            wake_on: MonitorWakeOn::Regex,
            pattern: Some("ready".to_string()),
            label: "build ready".to_string(),
            created_ts: now,
            last_event_ts: now,
            last_run_ts: Some(now),
            last_output: Some("ready".to_string()),
            summary: Some("matched".to_string()),
            cancelled_ts: Some(now),
        };
        let wait_outcomes = [
            ProcessAwaitOutput::Success {
                value: json!({ "summary": "done" }),
                control: None,
            },
            ProcessAwaitOutput::Failure {
                class: lash_core::ToolFailureClass::Execution,
                code: "subagent_failed".to_string(),
                message: "failed".to_string(),
                raw: Some(json!({ "reason": "failed" })),
                control: None,
            },
            ProcessAwaitOutput::Cancelled {
                message: "interrupted".to_string(),
                raw: None,
                control: None,
            },
            ProcessAwaitOutput::Abandoned {
                evidence: Box::new(lash_core::AbandonEvidence {
                    writer: lash_core::AbandonWriter::ReconciledRequest,
                    owner: None,
                    epoch_ms: 42,
                }),
                control: None,
            },
        ];

        let mut results = BTreeMap::<&str, Vec<Value>>::new();
        results.insert("pings_send", vec![pings_send_result(&ping)]);
        results.insert(
            "pings_resolve",
            vec![
                pings_resolve_result(Some(&ping)).unwrap(),
                pings_resolve_result(None).unwrap(),
            ],
        );
        results.insert("subagents_spawn", vec![subagent_spawn_result("proc-1")]);
        results.insert("subagents_prompt", vec![acknowledgement_result()]);
        results.insert("subagents_interrupt", vec![acknowledgement_result()]);
        results.insert(
            "subagents_list",
            vec![subagents_list_result(std::slice::from_ref(&process)).unwrap()],
        );
        results.insert(
            "subagents_progress",
            vec![
                subagents_progress_result(Some(&process), &events).unwrap(),
                subagents_progress_result(None, &[]).unwrap(),
            ],
        );
        results.insert(
            "subagents_wait",
            wait_outcomes
                .iter()
                .map(|outcome| subagents_wait_result("proc-1", outcome).unwrap())
                .collect(),
        );
        results.insert(
            "monitors_create",
            vec![monitors_create_result(&monitor).unwrap()],
        );
        results.insert(
            "monitors_list",
            vec![monitors_list_result(std::slice::from_ref(&monitor)).unwrap()],
        );
        results.insert("monitors_cancel", vec![monitors_cancel_result("monitor-1")]);
        results.insert(
            "shell_run",
            vec![
                shell_run_result(&ShellRunOutput {
                    status: Some(0),
                    stdout: "done\n".to_string(),
                    stderr: String::new(),
                    timed_out: false,
                })
                .unwrap(),
                shell_run_result(&ShellRunOutput {
                    status: None,
                    stdout: String::new(),
                    stderr: "timed out".to_string(),
                    timed_out: true,
                })
                .unwrap(),
            ],
        );

        let definitions = hirsel_tool_definitions();
        assert_eq!(results.len(), definitions.len());
        for definition in definitions {
            let examples = results
                .get(definition.name())
                .unwrap_or_else(|| panic!("missing result examples for {}", definition.name()));
            let schema = definition.contract.output_schema.canonical();
            let validator = jsonschema::JSONSchema::compile(schema).unwrap_or_else(|error| {
                panic!("invalid schema for {}: {error}", definition.name())
            });
            for example in examples {
                if let Err(errors) = validator.validate(example) {
                    let errors = errors.map(|error| error.to_string()).collect::<Vec<_>>();
                    panic!(
                        "result for {} did not match its schema: {errors:?}\nresult: {example}",
                        definition.name()
                    );
                }
            }
        }
    }

    fn remote_turn_activity(event: RemoteTurnEvent) -> RemoteSessionObservationEventPayload {
        RemoteSessionObservationEventPayload::TurnActivity {
            activity: Box::new(lash::remote::usage::RemoteTurnActivity {
                protocol_version: lash::remote::REMOTE_PROTOCOL_VERSION,
                sequence: 1,
                id: "activity-1".to_string(),
                correlation_id: "turn-1".to_string(),
                event,
            }),
        }
    }

    fn turn_events(broadcast_log: &BroadcastLog) -> Vec<(u64, TurnEventKind)> {
        broadcast_log
            .recent()
            .into_iter()
            .filter_map(|event| match event {
                HostToClient::TurnEvent { seq, event, .. } => Some((seq, event)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn timer_in_secs_becomes_one_shot_due_from_registration_time() {
        let record = timer_registration(
            serde_json::json!({
                "label": "ping",
                "in_secs": 5
            }),
            1_000,
        );
        let schedule = TimerSchedule::from_registration(&record).unwrap();

        assert!(schedule.due_occurrence(&record, 5_999).is_none());
        let occurrence = schedule.due_occurrence(&record, 6_000).unwrap();
        assert!(occurrence.one_shot);
        assert_eq!(occurrence.label, "ping");
        assert_eq!(occurrence.scheduled_at_ms, 6_000);
        assert_eq!(occurrence.idempotency_key, "timer:source-key:once:6000");
    }

    #[test]
    fn timer_every_secs_uses_sixty_second_floor() {
        let record = timer_registration(
            serde_json::json!({
                "label": "heartbeat",
                "every_secs": 5
            }),
            1_000,
        );
        let schedule = TimerSchedule::from_registration(&record).unwrap();

        assert_eq!(schedule.every_secs, Some(TIMER_MIN_RECURRING_SECS));
        assert!(schedule.due_occurrence(&record, 60_999).is_none());
        let occurrence = schedule.due_occurrence(&record, 61_000).unwrap();
        assert!(!occurrence.one_shot);
        assert_eq!(occurrence.scheduled_at_ms, 61_000);
        assert_eq!(occurrence.idempotency_key, "timer:source-key:every:1");
    }

    #[test]
    fn timer_schedule_requires_exactly_one_clock_field() {
        let record = timer_registration(
            serde_json::json!({
                "label": "bad",
                "in_secs": 5,
                "every_secs": 60
            }),
            1_000,
        );

        let error = TimerSchedule::from_registration(&record).unwrap_err();
        assert!(error.contains("exactly one"));
    }

    fn stored_blob(id: &str, name: &str, mime: &str, size: u64, path: PathBuf) -> StoredBlob {
        StoredBlob {
            blob: Blob {
                id: id.to_string(),
                name: name.to_string(),
                mime: mime.to_string(),
                size,
            },
            path,
            created_ts: Utc::now(),
        }
    }

    fn timer_registration(value: Value, created_at_ms: u64) -> TriggerSubscriptionRecord {
        TriggerSubscriptionRecord {
            subscription_id: "subscription-id".to_string(),
            registrant: ProcessOriginator::session(SessionScope::new(AGENT_SESSION_ID)),
            env_ref: ProcessExecutionEnvRef::new("process-env:test"),
            wake_target: Some(SessionScope::new(AGENT_SESSION_ID)),
            handle: "handle-id".to_string(),
            name: None,
            source_type: TIMER_SOURCE_TYPE.to_string(),
            source_key: "source-key".to_string(),
            source: serde_json::json!({
                "$lash_host_descriptor_type": TIMER_SOURCE_TYPE,
                "$lash_host_descriptor_value": value,
            }),
            payload_schema: LashSchema::new(serde_json::json!({ "type": "object" })),
            target: ProcessInput::External {
                metadata: serde_json::json!({}),
            },
            target_identity: ProcessIdentity::new("timer-test"),
            event_types: Vec::new(),
            input_template: BTreeMap::<String, TriggerInputBinding>::new(),
            target_label: None,
            enabled: true,
            created_at_ms,
            updated_at_ms: created_at_ms,
        }
    }
}

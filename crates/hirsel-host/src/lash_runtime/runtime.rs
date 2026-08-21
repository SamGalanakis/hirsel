use super::*;

#[derive(Clone)]
pub struct AgentRuntime {
    pub(super) backend: Arc<AgentBackend>,
    pub(super) model_selection: Option<ModelSelectionState>,
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
    pub task_action: Option<TaskActionContext>,
}

#[derive(Debug, Clone)]
pub struct TaskActionContext {
    pub event: Event,
    pub action: String,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelQueuedResult {
    Cancelled,
    AlreadyClaimed,
}

pub(super) enum AgentBackend {
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
            AgentBackend::Lash(runtime) => crate::side_chat::SideChatBackend::Lash {
                core: Arc::new(runtime.core.clone()),
                prompts: runtime.prompts.clone(),
            },
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
        let model_selection = match config.provider_mode {
            provider @ (ProviderMode::Codex | ProviderMode::OpenRouter) => Some(
                ModelSelectionState::load(
                    provider,
                    config.config_store.clone(),
                    config.providers.clone(),
                    &config.model,
                )
                .await
                .context("load main-agent model selection")?,
            ),
            ProviderMode::Anthropic => None,
        };
        match config.agent_mode {
            AgentMode::Scripted => Ok(Self {
                backend: Arc::new(AgentBackend::Scripted(start_scripted_runtime(
                    config,
                    tools,
                    broadcaster,
                    broadcast_log,
                ))),
                model_selection,
            }),
            AgentMode::Lash => {
                match LashAgentRuntime::start(
                    config,
                    model_selection.clone(),
                    tools,
                    broadcaster,
                    broadcast_log,
                )
                .await?
                {
                    LashStartup::Ready(runtime) => Ok(Self {
                        backend: Arc::new(AgentBackend::Lash(runtime)),
                        model_selection,
                    }),
                    LashStartup::Unavailable(runtime) => Ok(Self {
                        backend: Arc::new(AgentBackend::Degraded(runtime)),
                        model_selection,
                    }),
                }
            }
        }
    }

    pub fn model_snapshot(&self) -> Option<ModelSnapshot> {
        self.model_selection
            .as_ref()
            .map(ModelSelectionState::snapshot)
    }

    pub async fn set_model(&self, model_id: &str, variant: &str) -> anyhow::Result<ModelSelection> {
        let state = self.model_selection.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "runtime model selection requires HIRSEL_PROVIDER=codex or HIRSEL_PROVIDER=openrouter"
            )
        })?;
        let selection = state.validate(model_id, variant)?;
        state.persist_and_select(selection.clone()).await?;
        if let AgentBackend::Lash(runtime) = self.backend.as_ref() {
            runtime.apply_selected_model().await?;
        }
        Ok(selection)
    }

    /// Apply the Owner's current Agent prompt to the live session. A no-op on
    /// the scripted and degraded backends, which have no Lash session to
    /// reprompt; the config store is still the authority for both.
    pub async fn apply_agent_prompt(&self) -> anyhow::Result<()> {
        if let AgentBackend::Lash(runtime) = self.backend.as_ref() {
            runtime.apply_agent_prompt().await?;
        }
        Ok(())
    }

    pub async fn refresh_subagent_model_tools(
        &self,
        catalog: &SubagentModelCatalog,
    ) -> anyhow::Result<()> {
        if let AgentBackend::Lash(runtime) = self.backend.as_ref() {
            runtime.refresh_subagent_model_tools(catalog).await?;
        }
        Ok(())
    }

    /// Re-advertise the agent tool catalog after a plugin was enabled or
    /// disabled. A no-op on the scripted and degraded backends, which have no
    /// lash session to refresh.
    pub async fn refresh_plugin_tools(&self, tool_names: &[String]) -> anyhow::Result<()> {
        if let AgentBackend::Lash(runtime) = self.backend.as_ref() {
            runtime.refresh_plugin_tools(tool_names).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn next_turn_model_spec(&self) -> Option<lash::ModelSpec> {
        self.model_selection
            .as_ref()
            .map(ModelSelectionState::model_spec)
            .transpose()
            .expect("selected model metadata is valid")
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

    /// Deliver a standalone monitor wake.
    ///
    /// On the Lash backend this is a non-owner message, so ADR-0015 routes it
    /// to a triage fork rather than the main Agent's queue; only the fork's
    /// Escalate exit reaches the Agent. The other backends have no fork
    /// dispatcher and keep their pre-ADR delivery.
    pub async fn deliver_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(runtime) => runtime.deliver_monitor_wake(text).await,
            AgentBackend::Lash(runtime) => {
                let message = crate::fork_wake::WakeMessage::new(
                    crate::fork_wake::WakeSource::Monitor {
                        monitor_id: "standalone".to_string(),
                        label: "standalone monitor".to_string(),
                    },
                    text,
                    format!("monitor:standalone:{}", Uuid::new_v4()),
                );
                if !runtime.fork_wake.dispatch(message) {
                    anyhow::bail!("fork-wake dispatch is not installed");
                }
                Ok(())
            }
            AgentBackend::Degraded(runtime) => runtime.deliver_monitor_wake(text).await,
        }
    }

    /// Route one non-owner message to a triage fork, for host surfaces that
    /// have one to deliver (the debug smoke lever, and any future ingress).
    /// Returns `false` when the backend has no fork dispatcher.
    #[must_use]
    pub fn dispatch_fork_wake(&self, message: crate::fork_wake::WakeMessage) -> bool {
        match self.backend.as_ref() {
            AgentBackend::Lash(runtime) => runtime.fork_wake.dispatch(message),
            AgentBackend::Scripted(_) | AgentBackend::Degraded(_) => false,
        }
    }
}

pub(super) fn start_scripted_runtime(
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

pub(super) enum LashStartup {
    Ready(Arc<LashAgentRuntime>),
    Unavailable(Arc<DegradedAgentRuntime>),
}

pub(super) struct LashAgentRuntime {
    pub(super) core: lash::LashCore,
    /// Kept so an ephemeral triage fork can open on the same transport the
    /// main session rides (ADR-0015); the fork differs in model, not provider.
    pub(super) provider: ProviderHandle,
    pub(super) session: lash::LashSession,
    pub(super) session_id: String,
    pub(super) tools: ToolSuite,
    pub(super) broadcaster: broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
    pub(super) notify: Arc<Notify>,
    pub(super) pump_lock: Mutex<()>,
    pub(super) anchors: Arc<Mutex<TurnAnchorState>>,
    pub(super) active_turn_id: Arc<Mutex<Option<String>>>,
    pub(super) drain_seq: AtomicU64,
    pub(super) drain_boot_ms: u64,
    pub(super) drain_retry_scheduled: AtomicBool,
    pub(super) drain_retry_attempts: AtomicU64,
    pub(super) model_selection: Option<ModelSelectionState>,
    pub(super) prompts: PromptConfig,
    /// The handoff seed this session opened with, kept so a prompt edit can
    /// rebuild the session guidance without dropping the seed the rotation
    /// carried over.
    pub(super) handoff_seed: Option<String>,
    /// ADR-0015's one mechanical dispatch. Handed to the wake sites while the
    /// core is still being built and filled in once the runtime exists; an
    /// uninstalled handle means the wake site keeps its pre-ADR behaviour.
    pub(super) fork_wake: crate::fork_wake::ForkWakeHandle,
}

#[derive(Debug, Clone)]
pub(super) struct TurnAnchors {
    pub(super) owner_message_id: u64,
    pub(super) task_action_event_id: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct TurnAnchorState {
    pub(super) pending_by_source_key: HashMap<String, TurnAnchors>,
    pub(super) active: Option<TurnAnchors>,
}

use super::*;

#[path = "thread_action.rs"]
mod thread_action;
pub use thread_action::ThreadActionSnapshot;

#[derive(Clone)]
pub struct AgentRuntime {
    pub(super) backend: Arc<AgentBackend>,
    pub(super) model_selection: Option<ModelSelectionState>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerTurn {
    pub history_id: String,
    pub turn_id: Option<u64>,
    pub thread_id: u64,
    pub thread_action: Option<ThreadActionContext>,
    pub message_id: Option<u64>,
    pub report_triggered: bool,
    pub client_id: String,
    pub body: String,
    pub anchor: Option<u64>,
    pub attachments: Vec<Blob>,
    pub mode: SendMode,
}

impl OwnerTurn {
    pub(super) async fn stored_turn(
        &self,
        storage: &crate::storage::Storage,
    ) -> anyhow::Result<hirsel_proto::ThreadTurn> {
        let id = self
            .turn_id
            .ok_or_else(|| anyhow::anyhow!("input requires an accepted turn identity"))?;
        storage
            .accepted_thread_turn(&self.history_id, id, self.thread_id)
            .await
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadActionContext {
    pub thread: ThreadActionSnapshot,
    pub action: String,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelQueuedResult {
    Cancelled,
    AlreadyClaimed,
}

pub(super) enum AgentBackend {
    Threaded(Arc<ThreadRuntimeRegistry>),
    Scripted(Arc<ScriptedAgentRuntime>),
    Lash(Arc<LashAgentRuntime>),
    Degraded(Arc<DegradedAgentRuntime>),
}

impl AgentRuntime {
    pub fn readiness(&self) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(_) | AgentBackend::Scripted(_) | AgentBackend::Lash(_) => Ok(()),
            AgentBackend::Degraded(_) => anyhow::bail!("Lash store is unavailable"),
        }
    }

    pub fn is_scripted(&self) -> bool {
        matches!(self.backend.as_ref(), AgentBackend::Scripted(_))
            || matches!(self.backend.as_ref(), AgentBackend::Threaded(r) if r.is_scripted())
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
        let registry = ThreadRuntimeRegistry::start(
            tools.storage().history_id().await?,
            config,
            model_selection.clone(),
            tools.clone(),
            broadcaster,
            broadcast_log,
        );
        registry.refresh_execution_default().await?;
        for turn in tools.storage().interrupt_unfinished_thread_turns().await? {
            tools.publish_thread_turn(turn).await;
        }
        registry.spawn_poller();
        Ok(Self {
            backend: Arc::new(AgentBackend::Threaded(registry)),
            model_selection,
        })
    }

    pub(crate) async fn reset_history(&self) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(registry) => registry.reset_history().await,
            _ => anyhow::bail!("history reset requires the Thread runtime registry"),
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
        // Stored-only when the main Agent has been pointed at a provider the
        // host did not boot on: persist and report, but leave the running
        // session strictly alone — it is on another provider's handle and takes
        // the new selection at the next host restart.
        if !state.applies_to_live_session() {
            return Ok(selection);
        }
        if let AgentBackend::Threaded(registry) = self.backend.as_ref() {
            registry.refresh_execution_default().await?;
        }
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
        if let AgentBackend::Threaded(registry) = self.backend.as_ref() {
            for lane in registry.opened().await {
                if let AgentBackend::Lash(runtime) = lane.as_ref() {
                    runtime.apply_agent_prompt().await?;
                }
            }
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
        if let AgentBackend::Threaded(registry) = self.backend.as_ref() {
            for lane in registry.opened().await {
                if let AgentBackend::Lash(runtime) = lane.as_ref() {
                    runtime.refresh_subagent_model_tools(catalog).await?;
                }
            }
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
        if let AgentBackend::Threaded(registry) = self.backend.as_ref() {
            for lane in registry.opened().await {
                if let AgentBackend::Lash(runtime) = lane.as_ref() {
                    runtime.refresh_plugin_tools(tool_names).await?;
                }
            }
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
            AgentBackend::Threaded(runtime) => runtime.enqueue(turn).await,
            AgentBackend::Scripted(runtime) => runtime.enqueue(turn).await,
            AgentBackend::Lash(runtime) => runtime.enqueue_inner(turn).await,
            AgentBackend::Degraded(runtime) => runtime.enqueue(turn).await,
        }
    }

    pub async fn cancel_thread_turn(&self, thread_id: u64) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(runtime) => runtime.cancel(thread_id).await?,
            AgentBackend::Scripted(runtime) => {
                let state = runtime.state.lock().await;
                let active = state
                    .active
                    .as_ref()
                    .filter(|a| a.thread_id == thread_id)
                    .ok_or_else(|| anyhow::anyhow!("Thread #{thread_id} has no running turn"))?;
                active.cancel.cancel();
            }
            AgentBackend::Lash(runtime) => runtime.cancel_owned_turn(Some(thread_id)).await?,
            AgentBackend::Degraded(_) => anyhow::bail!("Thread #{thread_id} has no running turn"),
        }
        Ok(())
    }

    pub async fn cancel_turn(&self) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(_) => anyhow::bail!("cancellation requires an explicit Thread"),
            AgentBackend::Scripted(runtime) => runtime.cancel_turn().await,
            AgentBackend::Lash(runtime) => runtime.cancel_turn().await,
            AgentBackend::Degraded(runtime) => runtime.cancel_turn().await,
        }
    }

    pub async fn cancel_queued(&self, client_id: &str) -> anyhow::Result<CancelQueuedResult> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(runtime) => runtime.cancel_queued(client_id).await,
            AgentBackend::Scripted(runtime) => runtime.cancel_queued(client_id).await,
            AgentBackend::Lash(runtime) => runtime.cancel_queued(client_id).await,
            AgentBackend::Degraded(runtime) => runtime.cancel_queued(client_id).await,
        }
    }

    pub async fn start_monitor_process(&self, record: &MonitorRecord) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(registry) => registry.start_monitor(record).await,
            AgentBackend::Lash(runtime) => runtime.start_monitor_process(record).await,
            AgentBackend::Scripted(runtime) => {
                runtime.spawn_standalone_monitor(record.id.clone());
                Ok(())
            }
            AgentBackend::Degraded(_) => Ok(()),
        }
    }

    pub async fn cancel_monitor_process(&self, _monitor_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Deliver a standalone monitor wake.
    ///
    /// On the Lash backend this is a non-owner message, so ADR-0015 routes it
    /// to a triage fork rather than the main Agent's queue; only the fork's
    /// Escalate exit reaches the Agent. The other backends have no fork
    /// dispatcher and keep their pre-ADR delivery.
    pub async fn dispatch_fork_wake(
        &self,
        message: crate::fork_wake::WakeMessage,
    ) -> anyhow::Result<bool> {
        match self.backend.as_ref() {
            AgentBackend::Threaded(registry) => registry.dispatch_fork_wake(message).await,
            AgentBackend::Lash(runtime) => Ok(runtime.fork_wake.dispatch(message)),
            _ => Ok(false),
        }
    }
}

pub(super) fn start_scripted_runtime(
    config: RuntimeConfig,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    thread_id: u64,
    tasks: RuntimeTasks,
    capacity: Arc<tokio::sync::Semaphore>,
) -> Arc<ScriptedAgentRuntime> {
    let runtime = Arc::new(ScriptedAgentRuntime {
        thread_id,
        tasks,
        capacity,
        config,
        tools,
        broadcaster,
        broadcast_log,
        state: Arc::new(Mutex::new(ScriptedQueueState::default())),
        notify: Arc::new(Notify::new()),
    });
    let worker = Arc::clone(&runtime);
    runtime.tasks.spawn(async move {
        worker.run().await;
    });
    runtime
}

pub(super) enum LashStartup {
    Ready(Arc<LashAgentRuntime>),
    Unavailable(Arc<DegradedAgentRuntime>),
}

pub(super) struct LashAgentRuntime {
    pub(super) tasks: RuntimeTasks,
    pub(super) history_id: String,
    pub(super) thread_id: u64,
    pub(super) provider_id: String,
    pub(super) capacity: Arc<tokio::sync::Semaphore>,
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
    pub(super) request_lock: Mutex<()>,
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
    pub(super) request_id: Option<String>,
    pub(super) thread_id: u64,
    pub(super) thread_turn_id: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct TurnAnchorState {
    pub(super) active: Option<TurnAnchors>,
}

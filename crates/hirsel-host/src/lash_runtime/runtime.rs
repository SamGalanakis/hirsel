use super::*;

#[path = "thread_action.rs"]
mod thread_action;
pub use thread_action::ThreadActionSnapshot;

#[derive(Clone)]
pub struct AgentRuntime {
    pub(super) registry: Arc<ThreadRuntimeRegistry>,
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

pub(super) enum LaneRuntime {
    Scripted(Arc<ScriptedAgentRuntime>),
    Lash(Arc<LashAgentRuntime>),
    Degraded,
}

impl AgentRuntime {
    pub fn readiness(&self) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn is_scripted(&self) -> bool {
        self.registry.is_scripted()
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
            registry,
            model_selection,
        })
    }

    /// Re-derive the default Native execution after a configuration change, so
    /// the change is applied by the time the op that made it returns.
    pub(crate) async fn refresh_execution_default(&self) -> anyhow::Result<()> {
        self.registry.refresh_execution_default().await
    }

    pub(crate) async fn reset_history(&self) -> anyhow::Result<()> {
        self.registry.reset_history().await
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
        // host cannot build a transport for at all: persist and report, but
        // leave the default route on the provider the session can still reach.
        if !state.applies_to_live_session() {
            return Ok(selection);
        }
        self.registry.refresh_execution_default().await?;
        Ok(selection)
    }

    /// Apply the Owner's current Agent prompt to the live session. A no-op on
    /// the scripted and degraded backends, which have no Lash session to
    /// reprompt; the config store is still the authority for both.
    pub async fn apply_agent_prompt(&self) -> anyhow::Result<()> {
        for lane in self.registry.opened().await {
            if let LaneRuntime::Lash(runtime) = lane.as_ref() {
                runtime.apply_agent_prompt().await?;
            }
        }
        Ok(())
    }

    pub async fn refresh_subagent_model_tools(
        &self,
        catalog: &SubagentModelCatalog,
    ) -> anyhow::Result<()> {
        for lane in self.registry.opened().await {
            if let LaneRuntime::Lash(runtime) = lane.as_ref() {
                runtime.refresh_subagent_model_tools(catalog).await?;
            }
        }
        Ok(())
    }

    /// Re-advertise the agent tool catalog after a plugin was enabled or
    /// disabled. A no-op on the scripted and degraded backends, which have no
    /// lash session to refresh.
    pub async fn refresh_plugin_tools(&self, tool_names: &[String]) -> anyhow::Result<()> {
        for lane in self.registry.opened().await {
            if let LaneRuntime::Lash(runtime) = lane.as_ref() {
                runtime.refresh_plugin_tools(tool_names).await?;
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
        self.registry.enqueue(turn).await
    }

    pub async fn cancel_thread_turn(
        &self,
        expected_history: &str,
        thread_id: u64,
    ) -> anyhow::Result<()> {
        self.registry.cancel(expected_history, thread_id).await
    }

    pub async fn cancel_turn(&self) -> anyhow::Result<()> {
        anyhow::bail!("cancellation requires an explicit Thread")
    }

    pub async fn cancel_queued(&self, client_id: &str) -> anyhow::Result<CancelQueuedResult> {
        self.registry.cancel_queued(client_id).await
    }

    pub async fn process_snapshot(&self) -> anyhow::Result<Vec<hirsel_proto::ProcessInfo>> {
        self.registry.process_snapshot().await
    }

    pub async fn cancel_process(&self, thread_id: u64, process_id: &str) -> anyhow::Result<()> {
        self.registry.cancel_process(thread_id, process_id).await
    }

    pub async fn disable_process_trigger(
        &self,
        thread_id: u64,
        subscription_key: &str,
        expected_revision: u64,
    ) -> anyhow::Result<()> {
        self.registry
            .disable_trigger(thread_id, subscription_key, expected_revision)
            .await
    }

    pub async fn dispatch_fork_wake(
        &self,
        message: crate::fork_wake::WakeMessage,
    ) -> anyhow::Result<bool> {
        self.registry.dispatch_fork_wake(message).await
    }
}

impl LashAgentRuntime {
    pub(crate) fn thread_id(&self) -> u64 {
        self.thread_id
    }

    pub(crate) async fn emit_thread_occurrence(
        &self,
        source_type: &str,
        event_type: &str,
        thread_id: u64,
        payload: Value,
        idempotency_key: &str,
    ) -> anyhow::Result<()> {
        let mut filter = TriggerSubscriptionFilter::for_session(&self.session_id);
        filter.source_type = Some(source_type.to_string());
        filter.enabled = Some(true);
        for record in self.trigger_store.list_subscriptions(filter).await? {
            let target = record
                .source
                .get("$lash_host_descriptor_value")
                .and_then(|value| value.get("thread_id"))
                .and_then(Value::as_u64);
            if target != Some(thread_id) {
                continue;
            }
            let report = self
                .core
                .triggers()
                .emit(
                    lash::triggers::TriggerOccurrenceRequest::new(
                        source_type,
                        record.source_key.clone(),
                        payload.clone(),
                        format!("{idempotency_key}:{}", record.subscription_key),
                    )
                    .with_source(record.source.clone()),
                    inline_trigger_scope(format!(
                        "thread-trigger:{event_type}:{}:{}",
                        record.subscription_key, thread_id
                    )),
                )
                .await?;
            if !report.deliveries.is_empty() {
                tracing::debug!(event_type, thread_id, subscription_key = %record.subscription_key, "Thread trigger delivered");
            }
        }
        Ok(())
    }
}

pub(super) fn start_scripted_runtime(
    config: RuntimeConfig,
    tools: ToolSuite,
    _broadcaster: broadcast::Sender<HostToClient>,
    _broadcast_log: BroadcastLog,
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
    Unavailable,
}

/// Which provider this Thread's Native session is currently bound to.
///
/// The provider is a roster id, not a transport kind: two OpenAI-compatible
/// instances are the same `ProviderHandle::kind()` and different providers, so
/// the id is what a rebind compares. The handle travels with it because an
/// ephemeral triage fork opens on the same transport the main session rides
/// (ADR-0015) — the fork differs in model, not provider.
pub(super) struct NativeBinding {
    pub(super) provider_id: String,
    pub(super) provider: ProviderHandle,
}

pub(crate) struct LashAgentRuntime {
    pub(super) tasks: RuntimeTasks,
    pub(super) history_id: String,
    pub(super) thread_id: u64,
    /// The provider this Native session runs on. A Thread may name its own
    /// provider and model; the binding follows it at the next admission.
    pub(super) native: std::sync::RwLock<NativeBinding>,
    /// The coding operations' working directory for this lane, shared with the
    /// tool provider so an admission can re-root it.
    pub(super) coding: Arc<NativeCodingBinding>,
    /// Kept so a provider rebind can build a handle for another roster instance
    /// without reaching back through the registry.
    pub(super) config: RuntimeConfig,
    pub(super) capacity: Arc<tokio::sync::Semaphore>,
    pub(super) core: lash::LashCore,
    pub(super) session: lash::LashSession,
    pub(super) session_id: String,
    pub(super) tools: ToolSuite,
    pub(super) broadcaster: broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
    pub(super) notify: Arc<Notify>,
    /// Queued-work notifications first pass through the process bridge so a
    /// process wake cannot race the resident Agent pump.
    pub(super) process_notify: Arc<Notify>,
    pub(super) pump_lock: Mutex<()>,
    pub(super) request_lock: Mutex<()>,
    pub(super) anchors: Arc<Mutex<TurnAnchorState>>,
    pub(super) timeline_commits: TimelineCommitBarrier,
    pub(super) drain_seq: AtomicU64,
    pub(super) drain_boot_ms: u64,
    pub(super) drain_retry_scheduled: AtomicBool,
    pub(super) drain_retry_attempts: AtomicU64,
    pub(super) prompts: PromptConfig,
    /// The handoff seed this session opened with, kept so a prompt edit can
    /// rebuild the session guidance without dropping the seed the rotation
    /// carried over.
    pub(super) handoff_seed: Option<String>,
    /// ADR-0015's one mechanical dispatch. Handed to the wake sites while the
    /// core is still being built and filled in once the runtime exists; an
    /// uninstalled handle means the wake site keeps its pre-ADR behaviour.
    pub(super) fork_wake: crate::fork_wake::ForkWakeHandle,
    pub(super) trigger_store: Arc<dyn TriggerStore>,
    pub(super) last_processes: Mutex<HashMap<String, hirsel_proto::ProcessInfo>>,
}

#[derive(Debug, Clone)]
pub(super) struct TurnAnchors {
    pub(super) request_id: Option<String>,
    pub(super) thread_id: u64,
    pub(super) thread_turn_id: u64,
}

#[derive(Debug, Default)]
pub(super) struct TurnAnchorState {
    pub(super) drain_id: Option<String>,
    pub(super) active: Option<TurnAnchors>,
}

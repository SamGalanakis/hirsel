pub mod attachments;
pub mod auth;
pub mod blob_route;
pub mod boot_provider;
pub mod config;
pub mod debug;
pub mod fork_wake;
pub mod health;
pub mod host_config;
pub mod iroh;
mod json_spec;
pub mod lash_runtime;
pub mod model_selection;
pub mod monitors;
// This slice lands before the worker runtime that consumes it. Keep the
// crate-private integration surface lint-clean in isolation.
#[allow(dead_code)]
pub(crate) mod native_coding_tools;
pub mod plugins;
pub mod process_run;

pub mod prompt_config;
mod protocol;
pub mod provider_detect;
pub mod providers;
pub mod push;
pub mod skills;
pub mod storage;
pub mod subagent_models;
pub mod templates;
mod text;
mod thread_commands;
pub mod thread_instrument;
pub mod thread_tool_bridge;
pub mod tools;
pub mod ws;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock},
    time::SystemTime,
};

use anyhow::Context;
use axum::Router;
use chrono::{DateTime, Utc};
use hirsel_proto::{
    AgentSlot, ChatMessage, HostToClient, ModelSelection, ModelSnapshot, ProcessInfo,
    PromptSnapshot, ProviderRoster, SendMode, SubagentModelCatalog,
};
use tokio::sync::{Mutex, broadcast};
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    config::Config,
    lash_runtime::{AgentRuntime, CancelQueuedResult},
    storage::{MonitorCondition, MonitorRecord, Storage, monitor_process_info},
    tools::{ToolSuite, ToolsConfig},
};

const INVALID_SNOOZE_UNTIL: &str = "snooze requires data.until as a future RFC3339 timestamp; choose a snooze preset: This evening, Tomorrow morning, Next week, or Pick time";

#[derive(Clone)]
pub struct AppState {
    pub token: Arc<str>,
    pub storage: Storage,
    pub broadcaster: broadcast::Sender<HostToClient>,
    pub broadcast_log: BroadcastLog,
    pub agent: AgentRuntime,
    pub tools: ToolSuite,
    pub pushes: push::PushGateway,
    pub views: templates::ViewManager,
    pub plugins: plugins::PluginHost,
    pub subagent_models: subagent_models::SubagentModelState,
    pub prompts: prompt_config::PromptConfig,
    pub providers_roster: providers::ProviderRosterState,
    pub started_at: SystemTime,
    pub debug_enabled: bool,
    pub data_dir: Arc<PathBuf>,
    pub auth_throttle: auth::AuthThrottle,
    pub blob_signer: blob_route::BlobSigner,
    model_change_lock: Arc<Mutex<()>>,
    subagent_model_change_lock: Arc<Mutex<()>>,
    prompt_change_lock: Arc<Mutex<()>>,
    provider_change_lock: Arc<Mutex<()>>,
    iroh_ticket: Arc<StdRwLock<Option<String>>>,
}

#[derive(Clone, Default)]
pub struct BroadcastLog {
    events: Arc<StdMutex<VecDeque<HostToClient>>>,
}

impl BroadcastLog {
    const CAPACITY: usize = 256;

    pub fn record(&self, event: HostToClient) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if events.len() == Self::CAPACITY {
            events.pop_front();
        }
        events.push_back(event);
    }

    pub fn recent(&self) -> Vec<HostToClient> {
        self.events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
}

#[derive(Debug, Clone)]
pub struct OwnerSubmission {
    pub client_id: String,
    pub message: ChatMessage,
    pub inserted: bool,
}

impl AppState {
    pub fn set_iroh_ticket(&self, ticket: Option<String>) {
        *self
            .iroh_ticket
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = ticket;
    }

    pub fn iroh_ticket(&self) -> Option<String> {
        self.iroh_ticket
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn broadcast(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }

    pub fn model_snapshot(&self) -> Option<ModelSnapshot> {
        self.agent.model_snapshot()
    }

    /// Select the main Agent's model + variant for its named provider.
    pub async fn set_agent_model(
        &self,
        provider_id: &str,
        model_id: &str,
        variant: &str,
    ) -> anyhow::Result<ModelSelection> {
        // Provider changes and model changes share the provider lock. The frame
        // names the provider it was rendered for, so a delayed model frame can
        // never be validated against a newer provider choice.
        let _provider_guard = self.provider_change_lock.lock().await;
        let _model_guard = self.model_change_lock.lock().await;
        self.ensure_selection_provider(AgentSlot::Main, provider_id)?;
        let previous = self.model_snapshot();
        let current = self.agent.set_model(model_id, variant).await?;
        self.broadcast_model_snapshot(previous);
        Ok(current)
    }

    /// Publish the main agent's model surface when it actually moved. The whole
    /// snapshot goes out, not just the selection: a provider change swaps the
    /// control's shape (curated registry vs free-text id) and the client has no
    /// way to derive that from a bare `ModelSelection`.
    fn broadcast_model_snapshot(&self, previous: Option<ModelSnapshot>) {
        let Some(model) = self.model_snapshot() else {
            return;
        };
        if previous.as_ref() == Some(&model) {
            return;
        }
        self.broadcast(HostToClient::ModelChanged { model });
    }

    pub fn subagent_model_snapshot(&self) -> SubagentModelCatalog {
        self.subagent_models.snapshot()
    }

    pub async fn set_subagent_model(
        &self,
        provider: &str,
        model_id: &str,
        enabled: bool,
        enabled_variants: &[String],
    ) -> anyhow::Result<SubagentModelCatalog> {
        let _guard = self.subagent_model_change_lock.lock().await;
        let previous = self.subagent_model_snapshot();
        let catalog = self
            .subagent_models
            .set(provider, model_id, enabled, enabled_variants)
            .await?;
        if catalog != previous {
            self.agent.refresh_subagent_model_tools(&catalog).await?;
            self.broadcast(HostToClient::SubagentModelsChanged {
                catalog: catalog.clone(),
            });
        }
        Ok(catalog)
    }

    pub fn prompt_snapshot(&self) -> PromptSnapshot {
        self.prompts.snapshot()
    }

    /// Replace the Agent's system prompt body; empty text restores the bundled
    /// default. The edit is applied to the live session before the op returns,
    /// so it is in force from the Agent's next turn.
    pub async fn set_agent_prompt(&self, text: &str) -> anyhow::Result<PromptSnapshot> {
        let _guard = self.prompt_change_lock.lock().await;
        let previous = self.prompt_snapshot();
        self.prompts.set_agent_prompt(text).await?;
        let snapshot = self.prompt_snapshot();
        if snapshot != previous {
            self.agent.apply_agent_prompt().await?;
        }
        self.broadcast_prompts(&snapshot);
        Ok(snapshot)
    }

    /// Replace the fork agent's prompt body; empty text restores the bundled
    /// default. Persisted only — no runtime consumes the fork config yet.
    pub async fn set_fork_prompt(&self, text: &str) -> anyhow::Result<PromptSnapshot> {
        let _guard = self.prompt_change_lock.lock().await;
        self.prompts.set_fork_prompt(text).await?;
        let snapshot = self.prompt_snapshot();
        self.broadcast_prompts(&snapshot);
        Ok(snapshot)
    }

    /// Select the fork agent's model for its named provider.
    pub async fn set_fork_model(
        &self,
        provider_id: &str,
        model_id: &str,
        variant: &str,
    ) -> anyhow::Result<PromptSnapshot> {
        let _provider_guard = self.provider_change_lock.lock().await;
        let _prompt_guard = self.prompt_change_lock.lock().await;
        self.ensure_selection_provider(AgentSlot::Fork, provider_id)?;
        self.prompts.set_fork_model(model_id, variant).await?;
        let snapshot = self.prompt_snapshot();
        self.broadcast_prompts(&snapshot);
        Ok(snapshot)
    }

    /// The whole roster, with fresh credential detection for the built-ins.
    pub async fn provider_roster(&self) -> ProviderRoster {
        self.providers_roster.snapshot().await
    }

    /// Point one resident agent at a provider instance, seeding that provider's
    /// default model and variant in the same write.
    ///
    /// The main Agent's `ProviderHandle` is built once at boot and baked into
    /// the live session, so this stores and broadcasts the choice but does not
    /// swap the running session's provider — `booted_provider_id` is what lets
    /// a client say when the change takes effect. The fork is stored only; no
    /// fork runtime consumes it yet.
    pub async fn set_agent_provider(
        &self,
        agent: AgentSlot,
        provider_id: &str,
    ) -> anyhow::Result<ProviderRoster> {
        let _guard = self.provider_change_lock.lock().await;
        if self.providers_roster.booted_provider_id().is_none() {
            anyhow::bail!(
                "provider selection requires HIRSEL_PROVIDER=codex or HIRSEL_PROVIDER=openrouter"
            );
        }
        let choice = self.providers_roster.selection_for(provider_id)?;
        let seed = self.seed_selection(agent, &choice)?;
        let previous_model = match agent {
            AgentSlot::Main => self.model_snapshot(),
            AgentSlot::Fork => None,
        };
        self.providers_roster
            .point_agent_at(agent, &choice, &seed)
            .await?;
        match agent {
            // The snapshot is derived from the roster's stored choice, so it is
            // already the new shape by now — but only a full broadcast tells the
            // client that, and choosing a reasoning effort has to be possible
            // the moment the provider that offers one is chosen.
            AgentSlot::Main => self.broadcast_model_snapshot(previous_model),
            AgentSlot::Fork => {
                let snapshot = self.prompt_snapshot();
                self.broadcast_prompts(&snapshot);
            }
        }
        self.broadcast_providers().await
    }

    pub async fn add_provider(
        &self,
        id: &str,
        label: &str,
        base_url: &str,
        api_key: &str,
        default_model: &str,
    ) -> anyhow::Result<ProviderRoster> {
        let _guard = self.provider_change_lock.lock().await;
        let previous_worker_providers = self.providers_roster.native_worker_provider_ids();
        self.providers_roster
            .add(id, label, base_url, api_key, default_model)
            .await?;
        if self.providers_roster.native_worker_provider_ids() != previous_worker_providers {
            self.agent
                .refresh_subagent_model_tools(&self.subagent_model_snapshot())
                .await?;
        }
        self.broadcast_providers().await
    }

    /// Edit one instance. Editing the instance a resident agent points at can
    /// reshape that agent's model surface — a new `default_model` is the model
    /// a free-text agent falls back to — so both agent surfaces are re-derived
    /// and published alongside the roster.
    pub async fn update_provider(
        &self,
        id: &str,
        label: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
        default_model: Option<&str>,
    ) -> anyhow::Result<ProviderRoster> {
        let _guard = self.provider_change_lock.lock().await;
        let surfaces = self.agent_surfaces();
        let previous_worker_providers = self.providers_roster.native_worker_provider_ids();
        self.providers_roster
            .update(id, label, base_url, api_key, default_model)
            .await?;
        if self.providers_roster.native_worker_provider_ids() != previous_worker_providers {
            self.agent
                .refresh_subagent_model_tools(&self.subagent_model_snapshot())
                .await?;
        }
        self.broadcast_agent_surfaces(surfaces);
        self.broadcast_providers().await
    }

    /// Remove an instance the Owner added.
    ///
    /// There is deliberately no in-use guard: a stale `[model].provider` or
    /// `[fork].provider` is a warning and a fallback to the booted provider,
    /// never a boot error (see `ProviderRosterState::agent_provider`), so an
    /// Owner is never trapped by a provider they no longer want. That fallback
    /// is a real change of shape though — a curated registry where there was a
    /// free-text id, a different `current` — so removing the instance an agent
    /// points at republishes that agent's surface too, exactly as moving the
    /// agent off it would.
    pub async fn remove_provider(&self, id: &str) -> anyhow::Result<ProviderRoster> {
        let _guard = self.provider_change_lock.lock().await;
        let surfaces = self.agent_surfaces();
        let previous_worker_providers = self.providers_roster.native_worker_provider_ids();
        self.providers_roster.remove(id).await?;
        if self.providers_roster.native_worker_provider_ids() != previous_worker_providers {
            self.agent
                .refresh_subagent_model_tools(&self.subagent_model_snapshot())
                .await?;
        }
        self.broadcast_agent_surfaces(surfaces);
        self.broadcast_providers().await
    }

    /// Re-probe one built-in's local credentials. Detection is never cached, so
    /// the fresh roster this broadcasts is the fresh answer.
    pub async fn redetect_provider(&self, id: &str) -> anyhow::Result<ProviderRoster> {
        let _guard = self.provider_change_lock.lock().await;
        self.providers_roster.redetect(id)?;
        self.broadcast_providers().await
    }

    /// The model an agent is seeded with when it moves to a provider: the
    /// instance's `default_model` for an OpenAI-compatible endpoint, and the
    /// curated registry's own default for `codex`.
    fn seed_selection(
        &self,
        agent: AgentSlot,
        choice: &providers::AgentProviderChoice,
    ) -> anyhow::Result<ModelSelection> {
        let mode = match choice.is_free_text() {
            true => model_selection::SelectionMode::FreeText {
                provider_id: choice.id.clone(),
                default_model: choice.default_model.clone(),
            },
            false => model_selection::SelectionMode::Curated {
                provider: config::ProviderMode::Codex,
                provider_id: Some(choice.id.clone()),
            },
        };
        model_selection::default_in_mode(&mode, matches!(agent, AgentSlot::Fork)).ok_or_else(|| {
            anyhow::anyhow!(
                "provider `{}` offers no default model to seed; set one first",
                choice.id
            )
        })
    }

    /// Reject a model frame rendered for any provider other than the one the
    /// named agent is currently configured to use.
    fn ensure_selection_provider(&self, agent: AgentSlot, provider_id: &str) -> anyhow::Result<()> {
        let configured = match agent {
            AgentSlot::Main => self
                .model_snapshot()
                .and_then(|snapshot| snapshot.provider_id),
            AgentSlot::Fork => self
                .prompt_snapshot()
                .fork
                .and_then(|fork| fork.provider_id),
        };
        match configured.as_deref() {
            Some(configured) if configured == provider_id => Ok(()),
            Some(configured) => anyhow::bail!(
                "model selection is for provider `{provider_id}`, but the {agent:?} agent is configured for `{configured}`"
            ),
            None => {
                anyhow::bail!("the {agent:?} agent has no selectable provider for model selection")
            }
        }
    }

    async fn broadcast_providers(&self) -> anyhow::Result<ProviderRoster> {
        let roster = self.provider_roster().await;
        self.broadcast(HostToClient::ProvidersChanged {
            roster: roster.clone(),
        });
        Ok(roster)
    }

    fn broadcast_prompts(&self, snapshot: &PromptSnapshot) {
        self.broadcast(HostToClient::PromptsChanged {
            prompts: snapshot.clone(),
        });
    }

    /// Both resident agents' Owner-visible surfaces, captured before a roster
    /// edit so the edit can publish whichever of them it actually moved.
    fn agent_surfaces(&self) -> (Option<ModelSnapshot>, PromptSnapshot) {
        (self.model_snapshot(), self.prompt_snapshot())
    }

    /// Publish whichever agent surface a roster edit reshaped. Both are derived
    /// from the roster's stored choices, so an edit to an instance an agent
    /// points at moves them without any agent op being sent; a client that only
    /// heard `providers_changed` would keep rendering the old control.
    fn broadcast_agent_surfaces(&self, previous: (Option<ModelSnapshot>, PromptSnapshot)) {
        let (previous_model, previous_prompts) = previous;
        self.broadcast_model_snapshot(previous_model);
        let prompts = self.prompt_snapshot();
        if prompts != previous_prompts {
            self.broadcast_prompts(&prompts);
        }
    }

    pub async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.agent.cancel_turn().await
    }

    pub async fn cancel_queued_message(&self, client_id: &str) -> anyhow::Result<u64> {
        let Some(message_id) = self.storage.message_id_for_client_id(client_id).await? else {
            anyhow::bail!("already claimed");
        };
        let thread_id = self
            .storage
            .chat_message(message_id)
            .await?
            .map(|m| m.thread_id);
        match self.agent.cancel_queued(client_id).await? {
            CancelQueuedResult::Cancelled => {
                self.storage.delete_chat_message(message_id).await?;
                self.broadcast(HostToClient::MsgRemoved { id: message_id });
                if let Some(thread_id) = thread_id {
                    self.tools.publish_thread_summary(thread_id).await;
                }
                Ok(message_id)
            }
            CancelQueuedResult::AlreadyClaimed => anyhow::bail!("already claimed"),
        }
    }

    pub async fn handle_view_event(
        &self,
        instance_id: String,
        action: String,
        data: serde_json::Value,
    ) -> anyhow::Result<OwnerSubmission> {
        if action.trim().is_empty() {
            anyhow::bail!("view action must be a non-empty string");
        }
        let (expected_history, view) = self
            .views
            .bound_view(&instance_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("unknown view instance `{instance_id}`"))?;
        #[cfg(test)]
        view_event_tests::pause_after_lookup(&instance_id).await;
        let body = format!(
            "View `{instance_id}` emitted action `{action}` with data {}.",
            serde_json::to_string(&data)?
        );
        self.submit_addressed_turn(
            &expected_history,
            format!("view-event-{}", uuid::Uuid::new_v4()),
            view.thread_id,
            body,
            Vec::new(),
            Vec::new(),
            SendMode::Send,
            None,
            Vec::new(),
        )
        .await
    }

    pub async fn process_snapshot(&self) -> anyhow::Result<Vec<ProcessInfo>> {
        let all = self.storage.monitor_snapshot().await?;
        let mut running = Vec::new();
        let mut terminal = Vec::new();
        for process in all {
            if matches!(process.state, hirsel_proto::ProcessState::Running) {
                running.push(process);
            } else {
                terminal.push(process);
            }
        }
        running.sort_by(|left, right| {
            left.started_ts
                .cmp(&right.started_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        terminal.sort_by(|left, right| {
            left.last_event_ts
                .cmp(&right.last_event_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        if terminal.len() > 10 {
            terminal.drain(..terminal.len() - 10);
        }
        running.extend(terminal);
        Ok(running)
    }

    pub async fn create_monitor(
        &self,
        thread_id: u64,
        cmd: String,
        every_secs: u64,
        condition: MonitorCondition,
        label: String,
    ) -> anyhow::Result<MonitorRecord> {
        let record = self
            .storage
            .create_monitor(thread_id, cmd, every_secs, condition, label)
            .await?;
        self.broadcast_monitor(&record);
        self.agent.start_monitor_process(&record).await?;
        Ok(record)
    }

    pub async fn cancel_monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let record = self.storage.cancel_monitor(monitor_id).await?;
        if let Some(record) = &record {
            self.broadcast_monitor(record);
        }
        self.agent.cancel_monitor_process(monitor_id).await?;
        Ok(record)
    }

    pub fn broadcast_monitor(&self, record: &MonitorRecord) {
        self.broadcast(HostToClient::ProcessUpsert {
            process: monitor_process_info(record),
        });
    }
}

fn validate_empty_lifecycle_data(action: &str, data: &serde_json::Value) -> anyhow::Result<()> {
    if data.is_null() {
        return Ok(());
    }
    let object = data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("event {action} data must be null or an empty object"))?;
    if !object.is_empty() {
        anyhow::bail!("event {action} data must be empty");
    }
    Ok(())
}

fn validate_snooze_lifecycle_data(data: &serde_json::Value) -> anyhow::Result<DateTime<Utc>> {
    let object = data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!(INVALID_SNOOZE_UNTIL))?;
    if object.len() != 1 || !object.contains_key("until") {
        anyhow::bail!(INVALID_SNOOZE_UNTIL);
    }
    let until = object
        .get("until")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!(INVALID_SNOOZE_UNTIL))?;
    if until.len() > 1024 {
        anyhow::bail!(INVALID_SNOOZE_UNTIL);
    }
    let until = DateTime::parse_from_rfc3339(until)
        .map_err(|_| anyhow::anyhow!(INVALID_SNOOZE_UNTIL))?
        .with_timezone(&Utc);
    if until <= Utc::now() {
        anyhow::bail!(INVALID_SNOOZE_UNTIL);
    }
    Ok(until)
}

pub async fn build_app(config: Config) -> anyhow::Result<Router> {
    let state = build_state(config).await?;
    Ok(router_from_state(state))
}

pub async fn build_state(config: Config) -> anyhow::Result<AppState> {
    let config_store = host_config::ConfigStore::load(
        config.config_path.clone(),
        &config.docs_path,
        &providers::env_bootstrap(
            config.provider,
            &config.model,
            config.openrouter_api_key.as_deref(),
        ),
    )
    .await
    .with_context(|| format!("load host config from {}", config.config_path.display()))?;
    // One resolution, shared: the roster reports what actually booted and the
    // agent runtime builds its handle from the same answer, so the two can
    // never disagree about which provider the session is on.
    let home = provider_detect::home_dir();
    let boot = boot_provider::resolve(&config_store, config.provider, home.as_deref()).await;
    let providers_roster = providers::ProviderRosterState::new(config_store.clone(), &boot, home);
    let subagent_models = subagent_models::SubagentModelState::load(config_store.clone());
    let skills = skills::Skills::for_host(&config.data_dir)?;
    let storage = Storage::open(&config.data_dir)
        .await
        .with_context(|| format!("open storage under {}", config.data_dir.display()))?;
    let (broadcaster, _) = broadcast::channel(512);
    let broadcast_log = BroadcastLog::default();
    let template_store = templates::TemplateStore::load(config.templates_dir.clone())
        .await
        .with_context(|| {
            format!(
                "load view templates from {}",
                config.templates_dir.display()
            )
        })?;
    let views = templates::ViewManager::new(
        storage.history_id().await?,
        template_store,
        broadcaster.clone(),
        broadcast_log.clone(),
    );
    let pushes = push::PushGateway::from_env(storage.clone()).await?;
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: config.driver,
            fake_fixture: config.fake_fixture.clone(),
            subagent_models: subagent_models.clone(),
            providers: providers_roster.clone(),
            skills: skills.clone(),
        },
        storage.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
        pushes.clone(),
        views.clone(),
    );
    tools
        .return_expired_snoozes()
        .await
        .context("return expired snoozed events at startup")?;
    // Plugins boot before the agent: their tools have to be in the catalog
    // when the first tool-surface fingerprint is computed, and their skills
    // have to be in the guidance before the session prompt is assembled.
    let plugin_host = plugins::PluginHost::start(
        hirsel_plugins::all(),
        storage.clone(),
        tools.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
        plugins::SupervisorConfig::default(),
    )
    .await
    .context("start in-tree plugins")?;
    // What the Owner's prompt body is followed by: the runtime configuration
    // paths, then the enabled plugins' skills. Host-generated, so a prompt edit
    // cannot drop it.
    let prompts = prompt_config::PromptConfig::new(
        config.provider,
        config_store.clone(),
        providers_roster.clone(),
        format!(
            "{}{}",
            lash_runtime::agent_host_section(&config),
            plugin_host.skills_prompt()
        ),
    )
    .with_skills(skills);
    let agent = AgentRuntime::start(
        lash_runtime::RuntimeConfig {
            agent_mode: config.agent,
            provider_mode: config.provider,
            boot_plan: boot.plan,
            anthropic_api_key: config.anthropic_api_key.clone(),
            openrouter_api_key: config.openrouter_api_key.clone(),
            model: config.model.clone(),
            data_dir: config.data_dir.clone(),
            driver_mode: config.driver,
            config_store,
            providers: providers_roster.clone(),
            prompts: prompts.clone(),
        },
        tools.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
    )
    .await?;
    let blob_signer = blob_route::BlobSigner::new(config.token.as_bytes());
    let state = AppState {
        token: Arc::from(config.token),
        storage,
        broadcaster,
        broadcast_log,
        agent,
        tools,
        pushes,
        views,
        plugins: plugin_host,
        subagent_models,
        prompts,
        providers_roster,
        started_at: SystemTime::now(),
        debug_enabled: config.debug,
        data_dir: Arc::new(config.data_dir),
        auth_throttle: auth::AuthThrottle::default(),
        blob_signer,
        model_change_lock: Arc::new(Mutex::new(())),
        subagent_model_change_lock: Arc::new(Mutex::new(())),
        prompt_change_lock: Arc::new(Mutex::new(())),
        provider_change_lock: Arc::new(Mutex::new(())),
        iroh_ticket: Arc::new(StdRwLock::new(None)),
    };
    Ok(state)
}

pub fn router_from_state(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/livez", axum::routing::get(health::livez))
        .route("/readyz", axum::routing::get(health::readyz))
        .route("/ws", axum::routing::get(ws::ws_handler))
        .route("/blob/:id", axum::routing::get(blob_route::blob_handler))
        .with_state(state.clone())
        .merge(plugins::routes(state.clone()));
    if state.debug_enabled {
        app = app.merge(debug::routes(state.clone()));
    }
    let app_dir =
        std::env::var_os("HIRSEL_APP_DIR").map_or_else(|| "app/dist".into(), PathBuf::from);
    if app_dir.exists() {
        // SPA history fallback: the client owns `/t/<id>` Task deep links, and a
        // cold load of one is a request this server has never heard of. Anything
        // the asset directory cannot answer gets the app shell, which then reads
        // the path and opens that Task.
        app = app.fallback_service(
            ServeDir::new(&app_dir).fallback(ServeFile::new(app_dir.join("index.html"))),
        );
    } else {
        tracing::warn!(
            app_dir = %app_dir.display(),
            "app shell directory not found; serving WS/blob/debug only (set HIRSEL_APP_DIR or run from the repo root)"
        );
    }
    app
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod view_event_tests;

pub mod attachments;
pub mod auth;
pub mod blob_route;
pub mod config;
pub mod debug;
pub mod health;
pub mod host_config;
pub mod iroh;
mod json_spec;
pub mod lash_runtime;
pub mod model_selection;
pub mod monitors;
pub mod plugins;
pub mod process_run;
pub mod processes;
pub mod prompt_config;
mod protocol;
pub mod push;
pub mod side_chat;
pub mod storage;
pub mod subagent_models;
pub mod task_ui;
pub mod templates;
mod text;
pub mod tools;
pub mod ws;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock},
    time::{Duration, SystemTime},
};

use anyhow::Context;
use axum::Router;
use chrono::{DateTime, Utc};
use hirsel_proto::{
    AgentActivityState, ChatMessage, HostToClient, ModelSelection, ModelSnapshot, ProcessInfo,
    PromptSnapshot, SendMode, SubagentModelCatalog, ViewInstance,
};
use tokio::sync::{Mutex, broadcast};
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    config::Config,
    lash_runtime::{AgentRuntime, CancelQueuedResult, OwnerTurn, TaskActionContext},
    processes::ProcessStore,
    storage::{MonitorRecord, MonitorWakeOn, Storage, monitor_process_info},
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
    pub side_chats: Arc<side_chat::SideChatManager>,
    pub processes: ProcessStore,
    pub tools: ToolSuite,
    pub pushes: push::PushGateway,
    pub views: templates::ViewManager,
    pub plugins: plugins::PluginHost,
    pub subagent_models: subagent_models::SubagentModelState,
    pub prompts: prompt_config::PromptConfig,
    pub started_at: SystemTime,
    pub debug_enabled: bool,
    pub data_dir: Arc<PathBuf>,
    pub auth_throttle: auth::AuthThrottle,
    pub blob_signer: blob_route::BlobSigner,
    model_change_lock: Arc<Mutex<()>>,
    subagent_model_change_lock: Arc<Mutex<()>>,
    prompt_change_lock: Arc<Mutex<()>>,
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

    pub async fn set_model(&self, model_id: &str, variant: &str) -> anyhow::Result<ModelSelection> {
        let _guard = self.model_change_lock.lock().await;
        let previous = self.model_snapshot().map(|snapshot| snapshot.current);
        let current = self.agent.set_model(model_id, variant).await?;
        if previous.as_ref() != Some(&current) {
            self.broadcast(HostToClient::ModelChanged {
                current: current.clone(),
            });
        }
        Ok(current)
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
            self.broadcast_prompts(&snapshot);
        }
        Ok(snapshot)
    }

    /// Replace the fork agent's prompt body; empty text restores the bundled
    /// default. Persisted only — no runtime consumes the fork config yet.
    pub async fn set_fork_prompt(&self, text: &str) -> anyhow::Result<PromptSnapshot> {
        let _guard = self.prompt_change_lock.lock().await;
        let previous = self.prompt_snapshot();
        self.prompts.set_fork_prompt(text).await?;
        let snapshot = self.prompt_snapshot();
        if snapshot != previous {
            self.broadcast_prompts(&snapshot);
        }
        Ok(snapshot)
    }

    /// Select the fork agent's model from the booted provider's registry.
    pub async fn set_fork_model(
        &self,
        model_id: &str,
        variant: &str,
    ) -> anyhow::Result<PromptSnapshot> {
        let _guard = self.prompt_change_lock.lock().await;
        let previous = self.prompt_snapshot();
        self.prompts.set_fork_model(model_id, variant).await?;
        let snapshot = self.prompt_snapshot();
        if snapshot != previous {
            self.broadcast_prompts(&snapshot);
        }
        Ok(snapshot)
    }

    fn broadcast_prompts(&self, snapshot: &PromptSnapshot) {
        self.broadcast(HostToClient::PromptsChanged {
            prompts: snapshot.clone(),
        });
    }

    pub async fn submit_owner_message(
        &self,
        client_id: String,
        body: String,
        anchor: Option<u64>,
        attachments: Vec<String>,
        mentions: Vec<u64>,
        mode: SendMode,
    ) -> anyhow::Result<OwnerSubmission> {
        self.submit_owner_turn(client_id, body, anchor, attachments, mentions, mode, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_owner_turn(
        &self,
        client_id: String,
        body: String,
        anchor: Option<u64>,
        attachments: Vec<String>,
        mentions: Vec<u64>,
        mode: SendMode,
        task_action: Option<TaskActionContext>,
    ) -> anyhow::Result<OwnerSubmission> {
        let mentioned_pings = self.storage.mentioned_pings(&mentions).await?;
        let (message, inserted) = self
            .storage
            .append_owner_message(&client_id, body, anchor, &attachments)
            .await?;
        if inserted {
            let stored_attachments = self.storage.blobs_for_message(message.id).await?;
            if let Err(error) = self
                .agent
                .enqueue(OwnerTurn {
                    message_id: message.id,
                    client_id: client_id.clone(),
                    body: message.body.clone(),
                    anchor: message.r#ref,
                    attachments: stored_attachments,
                    mentioned_pings,
                    mode,
                    task_action,
                })
                .await
            {
                if let Err(delete_error) = self.storage.delete_chat_message(message.id).await {
                    tracing::warn!(
                        %delete_error,
                        message_id = message.id,
                        "failed to delete owner message after Agent enqueue failed"
                    );
                }
                return Err(error);
            }
            self.broadcast(HostToClient::Msg {
                message: message.clone(),
                sc: None,
            });
        }
        Ok(OwnerSubmission {
            client_id,
            message,
            inserted,
        })
    }

    pub async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.agent.cancel_turn().await?;
        self.broadcast(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: None,
        });
        Ok(())
    }

    pub async fn cancel_queued_message(&self, client_id: &str) -> anyhow::Result<u64> {
        let Some(message_id) = self.storage.message_id_for_client_id(client_id).await? else {
            anyhow::bail!("already claimed");
        };
        match self.agent.cancel_queued(client_id).await? {
            CancelQueuedResult::Cancelled => {
                self.storage.delete_chat_message(message_id).await?;
                self.broadcast(HostToClient::MsgRemoved { id: message_id });
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
        let view = self
            .views
            .get(&instance_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("unknown view instance `{instance_id}`"))?;
        let anchor = view_anchor(&view, &self.storage).await?;
        let body = format!(
            "View `{instance_id}` emitted action `{action}` with data {}.",
            serde_json::to_string(&data)?
        );
        self.submit_owner_message(
            format!("view-event-{}", uuid::Uuid::new_v4()),
            body,
            anchor,
            Vec::new(),
            Vec::new(),
            SendMode::Send,
        )
        .await
    }

    pub async fn process_snapshot(&self) -> anyhow::Result<Vec<ProcessInfo>> {
        let mut all = self.processes.snapshot()?;
        all.extend(self.storage.monitor_snapshot().await?);
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

    pub async fn handle_event_action(
        &self,
        event_id: u64,
        action: String,
        data: serde_json::Value,
    ) -> anyhow::Result<hirsel_proto::Event> {
        let current = self
            .storage
            .ping(event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        let event = match action.as_str() {
            // These are Host lifecycle verbs, not producer-generated actions.
            // Validate their exact payload before allowing any mutation.
            "reopen" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.reopen_ping(event_id).await?
            }
            "dismiss" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.resolve_ping(event_id).await?
            }
            "snooze" => {
                let until = validate_snooze_lifecycle_data(&data)?;
                self.storage.snooze_event(event_id, until).await?
            }
            "unsnooze" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.unsnooze_event(event_id).await?
            }
            "archive" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.archive_event(event_id).await?
            }
            "unarchive" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.unarchive_event(event_id).await?
            }
            generated => {
                if current.lifecycle() != hirsel_proto::EventLifecycle::Open {
                    anyhow::bail!("only an open Task can accept generated actions");
                }
                let validated = task_ui::validate_action(&current.ui, generated, &data)?;
                if !validated.settles {
                    let body = validated
                        .choice_label
                        .as_deref()
                        .or_else(|| data.get("label").and_then(serde_json::Value::as_str))
                        .unwrap_or(generated)
                        .to_string();
                    self.submit_owner_turn(
                        format!("event-action-{generated}-{event_id}"),
                        body,
                        Some(current.anchor),
                        Vec::new(),
                        vec![current.id],
                        SendMode::Send,
                        Some(TaskActionContext {
                            event: current.clone(),
                            action: generated.to_string(),
                            data,
                        }),
                    )
                    .await?;
                    return Ok(current);
                }

                if generated == "choose" {
                    if let Some(event) = self.side_chats.decide_event(event_id, &data).await? {
                        return Ok(event);
                    }
                    if !matches!(current.kind, hirsel_proto::EventKind::Judgment) {
                        anyhow::bail!("choose is only valid for judgment events");
                    }
                    let choice_label = validated
                        .choice_label
                        .ok_or_else(|| anyhow::anyhow!("choose requires an option-list action"))?;
                    self.submit_owner_message(
                        format!("event-action-choose-{event_id}"),
                        choice_label,
                        Some(current.anchor),
                        Vec::new(),
                        Vec::new(),
                        SendMode::Send,
                    )
                    .await?;
                }
                self.storage.resolve_ping(event_id).await?
            }
        }
        .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
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

fn event_choice_label<'a>(ui: &'a serde_json::Value, choice: &str) -> Option<&'a str> {
    ui.get("children")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|node| node.get("type").and_then(serde_json::Value::as_str) == Some("optionList"))
        .and_then(|node| node.get("options"))
        .and_then(serde_json::Value::as_array)
        .and_then(|options| {
            options.iter().find(|option| {
                option.get("key").and_then(serde_json::Value::as_str) == Some(choice)
            })
        })
        .and_then(|option| option.get("label"))
        .and_then(serde_json::Value::as_str)
}

async fn view_anchor(view: &ViewInstance, storage: &Storage) -> anyhow::Result<Option<u64>> {
    let Some(ping_id) = view.placement.strip_prefix("ping:") else {
        return Ok(None);
    };
    let ping_id = ping_id
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("view has invalid ping placement `{}`", view.placement))?;
    let ping = storage
        .ping(ping_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("view references unknown ping `{ping_id}`"))?;
    Ok(Some(ping.anchor))
}

pub async fn build_app(config: Config) -> anyhow::Result<Router> {
    let state = build_state(config).await?;
    Ok(router_from_state(state))
}

pub async fn build_state(config: Config) -> anyhow::Result<AppState> {
    let config_store = host_config::ConfigStore::load(
        config.config_path.clone(),
        &config.data_dir,
        &config.docs_path,
    )
    .await
    .with_context(|| format!("load host config from {}", config.config_path.display()))?;
    let subagent_models = subagent_models::SubagentModelState::load(config_store.clone());
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
    let views =
        templates::ViewManager::new(template_store, broadcaster.clone(), broadcast_log.clone());
    let processes = ProcessStore::default();
    let pushes = push::PushGateway::from_env(storage.clone()).await?;
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: config.driver,
            fake_fixture: config.fake_fixture.clone(),
            subagent_models: subagent_models.clone(),
        },
        storage.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
        processes.clone(),
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
        format!(
            "{}{}",
            lash_runtime::agent_host_section(&config),
            plugin_host.skills_prompt()
        ),
    );
    let agent = AgentRuntime::start(
        lash_runtime::RuntimeConfig {
            agent_mode: config.agent,
            provider_mode: config.provider,
            anthropic_api_key: config.anthropic_api_key.clone(),
            openrouter_api_key: config.openrouter_api_key.clone(),
            model: config.model.clone(),
            data_dir: config.data_dir.clone(),
            driver_mode: config.driver,
            config_store,
            prompts: prompts.clone(),
        },
        tools.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
    )
    .await?;
    let side_session_compatibility = config.compat_side_session_ttl_secs.map(|ttl| {
        side_chat::SideSessionCompatibility::new(
            agent.side_chat_backend(),
            Duration::from_secs(ttl),
        )
    });
    let side_chats = Arc::new(side_chat::SideChatManager::new(
        side_session_compatibility,
        broadcaster.clone(),
        broadcast_log.clone(),
        storage.clone(),
    ));
    let blob_signer = blob_route::BlobSigner::new(config.token.as_bytes());
    let state = AppState {
        token: Arc::from(config.token),
        storage,
        broadcaster,
        broadcast_log,
        agent,
        side_chats,
        processes,
        tools,
        pushes,
        views,
        plugins: plugin_host,
        subagent_models,
        prompts,
        started_at: SystemTime::now(),
        debug_enabled: config.debug,
        data_dir: Arc::new(config.data_dir),
        auth_throttle: auth::AuthThrottle::default(),
        blob_signer,
        model_change_lock: Arc::new(Mutex::new(())),
        subagent_model_change_lock: Arc::new(Mutex::new(())),
        prompt_change_lock: Arc::new(Mutex::new(())),
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

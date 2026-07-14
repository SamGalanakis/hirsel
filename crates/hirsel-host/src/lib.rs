pub mod attachments;
pub mod auth;
pub mod blob_route;
pub mod config;
pub mod debug;
pub mod health;
pub mod host_config;
pub mod iroh;
pub mod lash_runtime;
pub mod model_selection;
pub mod monitors;
pub mod process_run;
pub mod processes;
mod protocol;
pub mod push;
pub mod side_chat;
pub mod storage;
pub mod subagent_models;
pub mod templates;
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
use hirsel_proto::{
    AgentActivityState, ChatMessage, HostToClient, ModelSelection, ModelSnapshot, ProcessInfo,
    SendMode, SubagentModelCatalog, ViewInstance,
};
use tokio::sync::{Mutex, broadcast};
use tower_http::services::ServeDir;

use crate::{
    config::Config,
    lash_runtime::{AgentRuntime, CancelQueuedResult, OwnerTurn},
    processes::ProcessStore,
    storage::{MonitorRecord, MonitorWakeOn, Storage, monitor_process_info},
    tools::{ToolSuite, ToolsConfig},
};

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
    pub subagent_models: subagent_models::SubagentModelState,
    pub started_at: SystemTime,
    pub debug_enabled: bool,
    pub data_dir: Arc<PathBuf>,
    pub auth_throttle: auth::AuthThrottle,
    pub blob_signer: blob_route::BlobSigner,
    model_change_lock: Arc<Mutex<()>>,
    subagent_model_change_lock: Arc<Mutex<()>>,
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
        default_variant: &str,
    ) -> anyhow::Result<SubagentModelCatalog> {
        let _guard = self.subagent_model_change_lock.lock().await;
        let previous = self.subagent_model_snapshot();
        let catalog = self
            .subagent_models
            .set(provider, model_id, enabled, default_variant)
            .await?;
        if catalog != previous {
            self.broadcast(HostToClient::SubagentModelsChanged {
                catalog: catalog.clone(),
            });
        }
        Ok(catalog)
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
            if let Some(anchor) = message.r#ref {
                for event in self.storage.resolve_open_pings_for_anchor(anchor).await? {
                    self.broadcast(HostToClient::EventUpsert { event });
                }
            }
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
        if action == "choose"
            && let Some(event) = self.side_chats.decide_event(event_id, &data).await?
        {
            return Ok(event);
        }
        let event = match action.as_str() {
            "choose" => {
                if !matches!(current.kind, hirsel_proto::EventKind::Judgment) {
                    anyhow::bail!("choose is only valid for judgment events");
                }
                let choice = data
                    .get("choice")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("event choose requires data.choice"))?;
                let choice_label = event_choice_label(&current.ui, choice)
                    .ok_or_else(|| anyhow::anyhow!("unknown event choice: {choice}"))?;
                let body = match data.get("note") {
                    Some(note) => format!(
                        "{choice_label}\n{}",
                        note.as_str()
                            .ok_or_else(|| anyhow::anyhow!("event note must be a string"))?
                    ),
                    None => choice_label.to_string(),
                };
                if let Some(rule) = data.get("record_rule") {
                    let rule = rule
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("event record_rule must be a string"))?;
                    self.storage
                        .record_taste_rule(event_id, Some(choice), rule)
                        .await?;
                }
                self.submit_owner_message(
                    format!("event-action-choose-{event_id}"),
                    body,
                    Some(current.anchor),
                    Vec::new(),
                    Vec::new(),
                    SendMode::Send,
                )
                .await?;
                self.storage.ping(event_id).await?
            }
            "submit" | "dismiss" => self.storage.resolve_ping(event_id).await?,
            "snooze" => self.storage.reopen_ping(event_id).await?,
            "archive" => self.storage.archive_event(event_id).await?,
            "unarchive" => self.storage.unarchive_event(event_id).await?,
            other => anyhow::bail!("unsupported event action: {other}"),
        }
        .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
    }
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
    let agent = AgentRuntime::start(
        lash_runtime::RuntimeConfig {
            agent_mode: config.agent,
            provider_mode: config.provider,
            anthropic_api_key: config.anthropic_api_key.clone(),
            model: config.model.clone(),
            data_dir: config.data_dir.clone(),
            driver_mode: config.driver,
            config_store,
            agent_guidance: lash_runtime::agent_guidance(&config),
        },
        tools.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
    )
    .await?;
    let side_chats = Arc::new(side_chat::SideChatManager::new(
        agent.side_chat_backend(),
        broadcaster.clone(),
        broadcast_log.clone(),
        storage.clone(),
    ));
    side_chats.spawn_reaper(Duration::from_secs(config.sidechat_ttl_secs));
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
        subagent_models,
        started_at: SystemTime::now(),
        debug_enabled: config.debug,
        data_dir: Arc::new(config.data_dir),
        auth_throttle: auth::AuthThrottle::default(),
        blob_signer,
        model_change_lock: Arc::new(Mutex::new(())),
        subagent_model_change_lock: Arc::new(Mutex::new(())),
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
        .with_state(state.clone());
    if state.debug_enabled {
        app = app.merge(debug::routes(state.clone()));
    }
    let app_dir =
        std::env::var_os("HIRSEL_APP_DIR").map_or_else(|| "app/dist".into(), PathBuf::from);
    if app_dir.exists() {
        app = app.fallback_service(ServeDir::new(&app_dir));
    } else {
        tracing::warn!(
            app_dir = %app_dir.display(),
            "app shell directory not found; serving WS/blob/debug only (set HIRSEL_APP_DIR or run from the repo root)"
        );
    }
    app
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hirsel_proto::{ChatAuthor, PingStatus, SendMode};
    use serde_json::json;

    use super::*;
    use crate::config::{AgentMode, Config, DriverMode, ProviderMode};

    #[tokio::test]
    async fn scripted_next_turn_waits_and_cancel_queued_removes_message() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let mut broadcasts = state.broadcaster.subscribe();

        state
            .submit_owner_message(
                "active".to_string(),
                "slow:0.4".to_string(),
                None,
                Vec::new(),
                Vec::new(),
                SendMode::Send,
            )
            .await
            .unwrap();
        read_until_agent_activity(&mut broadcasts, AgentActivityState::Thinking).await;

        let queued = state
            .submit_owner_message(
                "queued".to_string(),
                "pong".to_string(),
                None,
                Vec::new(),
                Vec::new(),
                SendMode::NextTurn,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            state
                .storage
                .all_chat()
                .await
                .unwrap()
                .iter()
                .all(|message| message.author == ChatAuthor::Owner),
            "queued next-turn input should not be answered while slow turn is active"
        );

        let removed_id = state.cancel_queued_message("queued").await.unwrap();
        assert_eq!(removed_id, queued.message.id);
        read_until_msg_removed(&mut broadcasts, removed_id).await;
        assert!(
            state
                .storage
                .all_chat()
                .await
                .unwrap()
                .iter()
                .all(|message| message.id != removed_id)
        );

        read_until_agent_activity(&mut broadcasts, AgentActivityState::Idle).await;
        let messages = state.storage.all_chat().await.unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.author == ChatAuthor::Agent)
                .count(),
            1,
            "only the uncancelled slow turn should receive a scripted reply"
        );
    }

    #[tokio::test]
    async fn scripted_cancel_turn_interrupts_slow_turn_without_reply() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let mut broadcasts = state.broadcaster.subscribe();

        state
            .submit_owner_message(
                "active".to_string(),
                "slow:5".to_string(),
                None,
                Vec::new(),
                Vec::new(),
                SendMode::Send,
            )
            .await
            .unwrap();
        read_until_agent_activity(&mut broadcasts, AgentActivityState::Thinking).await;

        state.cancel_turn().await.unwrap();
        read_until_agent_activity(&mut broadcasts, AgentActivityState::Idle).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            state
                .storage
                .all_chat()
                .await
                .unwrap()
                .iter()
                .all(|message| message.author == ChatAuthor::Owner),
            "cancelled slow turn should not produce an Agent reply"
        );
    }

    #[tokio::test]
    async fn owner_message_enqueue_failure_deletes_message_without_broadcast() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let mut broadcasts = state.broadcaster.subscribe();

        let error = state
            .submit_owner_message(
                "enqueue-fails".to_string(),
                "__hirsel_test_enqueue_error__".to_string(),
                None,
                Vec::new(),
                Vec::new(),
                SendMode::Send,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("scripted enqueue failed"));
        assert!(state.storage.all_chat().await.unwrap().is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), broadcasts.recv())
                .await
                .is_err(),
            "failed enqueue must not publish a sent message"
        );
    }

    #[tokio::test]
    async fn owner_reply_resolves_ping_and_broadcasts_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Choose", None)
            .await
            .unwrap();
        let ping = state
            .storage
            .create_ping(
                "choose-release",
                "Choose whether to release",
                "Choose",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();

        state
            .submit_owner_message(
                "reply-1".to_string(),
                "Ship it".to_string(),
                Some(anchor.id),
                Vec::new(),
                Vec::new(),
                SendMode::Send,
            )
            .await
            .unwrap();

        assert_eq!(
            state.storage.ping(ping.id).await.unwrap().unwrap().status,
            PingStatus::Done
        );
        assert!(state.broadcast_log.recent().iter().any(|event| matches!(
            event,
            HostToClient::EventUpsert { event: update }
                if update.id == ping.id && update.status == PingStatus::Done
        )));
    }

    #[tokio::test]
    async fn mentioning_a_ping_never_resolves_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Status", None)
            .await
            .unwrap();
        let ping = state
            .storage
            .create_ping(
                "status-check",
                "Check the current status",
                "Status",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();

        state
            .submit_owner_message(
                "mention-1".to_string(),
                "What is happening?".to_string(),
                None,
                Vec::new(),
                vec![ping.id],
                SendMode::Send,
            )
            .await
            .unwrap();

        assert_eq!(
            state.storage.ping(ping.id).await.unwrap().unwrap().status,
            PingStatus::Open
        );
        assert!(state.broadcast_log.recent().iter().all(|event| !matches!(
            event,
            HostToClient::EventUpsert { event: update } if update.id == ping.id
        )));
    }

    #[tokio::test]
    async fn set_model_changes_the_next_turn_model_spec() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.provider = ProviderMode::Codex;
        config.model = "gpt-5.6-sol".to_string();
        let state = build_state(config).await.unwrap();

        let selected = state.set_model("gpt-5.6-sol", "high").await.unwrap();
        let spec = state
            .agent
            .next_turn_model_spec()
            .expect("Codex runtime has a selectable model");

        assert_eq!(selected.id, "gpt-5.6-sol");
        assert_eq!(selected.variant, "high");
        assert_eq!(spec.id, "gpt-5.6-sol");
        assert_eq!(spec.variant.effort(), Some("high"));
        assert!(state.broadcast_log.recent().iter().any(|event| matches!(
            event,
            HostToClient::ModelChanged { current }
                if current.id == "gpt-5.6-sol" && current.variant == "high"
        )));
    }

    #[tokio::test]
    async fn set_model_rejects_unknown_models_and_variants() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.provider = ProviderMode::Codex;
        config.model = "gpt-5.6-sol".to_string();
        let state = build_state(config).await.unwrap();

        // gpt-5.5 is no longer offered for the main agent — reject it, and any
        // unknown variant, while leaving the configured selection untouched.
        assert!(state.set_model("gpt-5.5", "high").await.is_err());
        assert!(state.set_model("gpt-5.6-sol", "impossible").await.is_err());
        assert_eq!(
            state.model_snapshot().unwrap().current,
            ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "medium".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn set_subagent_model_persists_and_broadcasts_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let catalog = state
            .set_subagent_model("claude", "claude-sonnet-5", false, "low")
            .await
            .unwrap();
        let sonnet = &catalog.providers[1].models[1];
        assert!(!sonnet.enabled);
        assert_eq!(sonnet.default_variant, "low");
        assert!(state.broadcast_log.recent().iter().any(|event| matches!(
            event,
            HostToClient::SubagentModelsChanged { catalog }
                if !catalog.providers[1].models[1].enabled
        )));
        let persisted = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
        assert!(persisted.contains("[subagent_models.claude.claude-sonnet-5]"));
        assert!(persisted.contains("default_variant = \"low\""));
    }

    #[tokio::test]
    async fn canvas_view_event_enters_main_chat_as_owner_message() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        state
            .views
            .show(
                None,
                Some(json!({ "type": "action", "label": "Retry", "action": "retry" })),
                None,
                Some("view-canvas".to_string()),
                "canvas".to_string(),
            )
            .await
            .unwrap();

        let submission = state
            .handle_view_event(
                "view-canvas".to_string(),
                "retry".to_string(),
                json!({ "attempt": 2 }),
            )
            .await
            .unwrap();

        assert_eq!(submission.message.author, ChatAuthor::Owner);
        assert_eq!(submission.message.r#ref, None);
        assert!(submission.message.body.contains("`retry`"));
        assert!(submission.message.body.contains(r#"{"attempt":2}"#));
    }

    #[tokio::test]
    async fn ping_view_event_replies_to_anchor_and_auto_resolves_ping() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Choose", None)
            .await
            .unwrap();
        let ping = state
            .storage
            .create_ping(
                "release-window",
                "Choose a release window",
                "Choose",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
        state
            .views
            .show(
                None,
                Some(json!({
                    "type": "optionSet",
                    "action": "window_selected",
                    "choices": [{ "label": "Tonight", "value": "tonight" }]
                })),
                None,
                Some("view-ping".to_string()),
                format!("ping:{}", ping.id),
            )
            .await
            .unwrap();

        let submission = state
            .handle_view_event(
                "view-ping".to_string(),
                "window_selected".to_string(),
                json!({ "value": "tonight" }),
            )
            .await
            .unwrap();

        assert_eq!(submission.message.r#ref, Some(anchor.id));
        assert_eq!(
            state.storage.ping(ping.id).await.unwrap().unwrap().status,
            PingStatus::Done
        );
        assert!(state.broadcast_log.recent().iter().any(|event| matches!(
            event,
            HostToClient::EventUpsert { event: update }
                if update.id == ping.id && update.status == PingStatus::Done
        )));
    }

    #[tokio::test]
    async fn event_action_choose_resolves_judgment_and_records_taste_rule() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Choose the release", None)
            .await
            .unwrap();
        let event = state
            .tools
            .pings_send_with_view(
                "release-channel",
                "Which release channel should we use?",
                "Stable is slower; `edge` reaches testers now.",
                anchor.id,
                true,
                vec![
                    hirsel_proto::QuickReply {
                        value: "Use stable for lower risk".to_string(),
                        label: "Stable".to_string(),
                    },
                    hirsel_proto::QuickReply {
                        value: "Use edge for faster feedback".to_string(),
                        label: "Edge".to_string(),
                    },
                ],
                Some(json!({ "type": "text", "text": "release diff" })),
                Some(2),
            )
            .await
            .unwrap();

        assert_eq!(event.kind, hirsel_proto::EventKind::Judgment);
        assert_eq!(event.ui["type"], "card");
        assert_eq!(event.ui["children"][0]["type"], "eyebrow");
        assert_eq!(event.ui["children"][3]["type"], "optionList");
        assert_eq!(event.ui["children"][3]["options"][0]["key"], "A");
        assert_eq!(event.ui["children"][4]["type"], "viewSlot");
        let serialized_ui = event.ui.to_string();
        assert!(!serialized_ui.contains("wait"));
        assert!(!serialized_ui.contains("cost"));
        assert!(!serialized_ui.contains("turns"));

        let resolved = state
            .handle_event_action(
                event.id,
                "choose".to_string(),
                json!({
                    "choice": "A",
                    "record_rule": "Default releases to stable unless feedback speed is critical"
                }),
            )
            .await
            .unwrap();
        assert_eq!(resolved.status, PingStatus::Done);
        let owner_reply = state
            .storage
            .all_chat()
            .await
            .unwrap()
            .into_iter()
            .find(|message| {
                message.author == ChatAuthor::Owner && message.r#ref == Some(event.anchor)
            })
            .expect("choose should inject an anchor-refed Owner reply");
        assert_eq!(owner_reply.body, "Stable");
        let taste = state.storage.taste_decisions().await.unwrap();
        assert_eq!(taste.len(), 1);
        assert_eq!(taste[0].event_id, event.id);
        assert_eq!(taste[0].choice.as_deref(), Some("A"));
        assert_eq!(
            taste[0].rule,
            "Default releases to stable unless feedback speed is critical"
        );
    }

    #[tokio::test]
    async fn event_action_choose_appends_note_to_owner_reply() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Choose the storage design", None)
            .await
            .unwrap();
        let event = state
            .tools
            .pings_send(
                "storage-design",
                "Where should views be stored?",
                "Choose the durable representation.",
                anchor.id,
                true,
                vec![
                    hirsel_proto::QuickReply {
                        value: "Store views in their own table".to_string(),
                        label: "sqlite views table".to_string(),
                    },
                    hirsel_proto::QuickReply {
                        value: "Store views alongside events".to_string(),
                        label: "serialized event field".to_string(),
                    },
                ],
            )
            .await
            .unwrap();

        let resolved = state
            .handle_event_action(
                event.id,
                "choose".to_string(),
                json!({
                    "choice": "A",
                    "note": "Keep the schema queryable for debugging."
                }),
            )
            .await
            .unwrap();

        assert_eq!(resolved.status, PingStatus::Done);
        let owner_reply = state
            .storage
            .all_chat()
            .await
            .unwrap()
            .into_iter()
            .find(|message| {
                message.author == ChatAuthor::Owner && message.r#ref == Some(event.anchor)
            })
            .expect("choose should inject an anchor-refed Owner reply");
        assert_eq!(
            owner_reply.body,
            "sqlite views table\nKeep the schema queryable for debugging."
        );
    }

    #[tokio::test]
    async fn event_action_archive_and_unarchive_broadcast_full_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Review the release", None)
            .await
            .unwrap();
        let event = state
            .tools
            .pings_send(
                "release-review",
                "Review the release",
                "Choose whether to release.",
                anchor.id,
                true,
                vec![
                    hirsel_proto::QuickReply {
                        value: "release".to_string(),
                        label: "Release".to_string(),
                    },
                    hirsel_proto::QuickReply {
                        value: "hold".to_string(),
                        label: "Hold".to_string(),
                    },
                ],
            )
            .await
            .unwrap();
        let event_id = event.id;

        let archived = state
            .handle_event_action(event_id, "archive".to_string(), json!({}))
            .await
            .unwrap();
        assert!(archived.archived);
        assert_eq!(archived.status, PingStatus::Done);

        let unarchived = state
            .handle_event_action(event_id, "unarchive".to_string(), json!({}))
            .await
            .unwrap();
        assert!(!unarchived.archived);
        assert_eq!(unarchived.status, PingStatus::Done);

        let updates = state
            .broadcast_log
            .recent()
            .into_iter()
            .filter_map(|frame| match frame {
                HostToClient::EventUpsert { event } if event.id == event_id => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(updates.len(), 3);
        assert!(updates[1].archived);
        assert!(!updates[2].archived);
    }

    #[tokio::test]
    async fn scheduled_digest_emits_summary_event_without_push() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let event = state
            .tools
            .emit_scheduled_digest(
                "morning-digest",
                "Overnight work completed cleanly.",
                "3 repositories checked",
            )
            .await
            .unwrap();

        assert_eq!(event.kind, hirsel_proto::EventKind::Summary);
        assert_eq!(event.source.kind, hirsel_proto::EventSourceKind::Scheduled);
        assert_eq!(event.source.r#ref.as_deref(), Some("morning-digest"));
        assert_eq!(event.ui["children"][0]["type"], "text");
        assert_eq!(event.ui["children"][1]["type"], "status");
        assert_eq!(event.ui["children"][2]["type"], "keyValue");
        assert!(state.pushes.recorded_pushes().is_empty());
        assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
            frame,
            HostToClient::EventUpsert { event: update } if update.id == event.id
        )));
    }

    async fn read_until_agent_activity(
        broadcasts: &mut tokio::sync::broadcast::Receiver<HostToClient>,
        state: AgentActivityState,
    ) {
        loop {
            match broadcasts.recv().await.unwrap() {
                HostToClient::AgentActivity {
                    state: observed, ..
                } if observed == state => return,
                _ => {}
            }
        }
    }

    async fn read_until_msg_removed(
        broadcasts: &mut tokio::sync::broadcast::Receiver<HostToClient>,
        id: u64,
    ) {
        loop {
            match broadcasts.recv().await.unwrap() {
                HostToClient::MsgRemoved { id: observed } if observed == id => return,
                _ => {}
            }
        }
    }

    pub(crate) fn test_config(data_dir: &std::path::Path) -> Config {
        Config {
            token: "test-token".to_string(),
            agent: AgentMode::Scripted,
            provider: ProviderMode::Anthropic,
            anthropic_api_key: None,
            model: "claude-opus-4-7".to_string(),
            data_dir: data_dir.to_path_buf(),
            config_path: data_dir.join("hirsel.toml"),
            docs_path: crate::templates::bundled_docs_path(),
            templates_dir: crate::templates::bundled_templates_dir(),
            driver: DriverMode::Fake,
            fake_fixture: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            debug: true,
            sidechat_ttl_secs: 86_400,
        }
    }
}

pub mod attachments;
pub mod blob_route;
pub mod config;
pub mod debug;
pub mod lash_runtime;
pub mod processes;
pub mod storage;
pub mod tools;
pub mod ws;

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::SystemTime,
};

use anyhow::Context;
use axum::Router;
use hirsel_proto::{AgentActivityState, ChatMessage, HostToClient, SendMode};
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

use crate::{
    config::Config,
    lash_runtime::{AgentRuntime, CancelQueuedResult, OwnerTurn},
    processes::ProcessStore,
    storage::Storage,
    tools::{ToolSuite, ToolsConfig},
};

#[derive(Clone)]
pub struct AppState {
    pub token: Arc<str>,
    pub storage: Storage,
    pub broadcaster: broadcast::Sender<HostToClient>,
    pub broadcast_log: BroadcastLog,
    pub agent: AgentRuntime,
    pub processes: ProcessStore,
    pub started_at: SystemTime,
    pub debug_enabled: bool,
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
    pub fn broadcast(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }

    pub async fn submit_owner_message(
        &self,
        client_id: String,
        body: String,
        anchor: Option<u64>,
        attachments: Vec<String>,
        mode: SendMode,
    ) -> anyhow::Result<OwnerSubmission> {
        let (message, inserted) = self
            .storage
            .append_owner_message(&client_id, body, anchor, &attachments)
            .await?;
        if inserted {
            let stored_attachments = self.storage.blobs_for_message(message.id).await?;
            self.broadcast(HostToClient::Msg {
                message: message.clone(),
            });
            self.agent
                .enqueue(OwnerTurn {
                    message_id: message.id,
                    client_id: client_id.clone(),
                    body: message.body.clone(),
                    anchor: message.r#ref,
                    attachments: stored_attachments,
                    mode,
                })
                .await?;
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
}

pub async fn build_app(config: Config) -> anyhow::Result<Router> {
    let state = build_state(config).await?;
    Ok(router_from_state(state))
}

pub async fn build_state(config: Config) -> anyhow::Result<AppState> {
    let storage = Storage::open(&config.data_dir)
        .await
        .with_context(|| format!("open storage under {}", config.data_dir.display()))?;
    let (broadcaster, _) = broadcast::channel(512);
    let broadcast_log = BroadcastLog::default();
    let processes = ProcessStore::default();
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: config.driver,
            fake_fixture: config.fake_fixture.clone(),
        },
        storage.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
        processes.clone(),
    );
    let agent = AgentRuntime::start(
        lash_runtime::RuntimeConfig {
            agent_mode: config.agent,
            provider_mode: config.provider,
            anthropic_api_key: config.anthropic_api_key.clone(),
            model: config.model.clone(),
            data_dir: config.data_dir.clone(),
            driver_mode: config.driver,
        },
        tools,
        broadcaster.clone(),
        broadcast_log.clone(),
    )
    .await?;
    Ok(AppState {
        token: Arc::from(config.token),
        storage,
        broadcaster,
        broadcast_log,
        agent,
        processes,
        started_at: SystemTime::now(),
        debug_enabled: config.debug,
    })
}

pub fn router_from_state(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/ws", axum::routing::get(ws::ws_handler))
        .route("/blob/:id", axum::routing::get(blob_route::blob_handler))
        .with_state(state.clone());
    if state.debug_enabled {
        app = app.merge(debug::routes(state.clone()));
    }
    if Path::new("app/dist").exists() {
        app = app.fallback_service(ServeDir::new("app/dist"));
    }
    app
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hirsel_proto::{ChatAuthor, SendMode};

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

    fn test_config(data_dir: &std::path::Path) -> Config {
        Config {
            token: "test-token".to_string(),
            agent: AgentMode::Scripted,
            provider: ProviderMode::Anthropic,
            anthropic_api_key: None,
            model: "claude-opus-4-7".to_string(),
            data_dir: data_dir.to_path_buf(),
            driver: DriverMode::Fake,
            fake_fixture: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            debug: true,
        }
    }
}

pub mod config;
pub mod debug;
pub mod lash_runtime;
pub mod processes;
pub mod storage;
pub mod tools;
pub mod ws;

use std::{path::Path, sync::Arc, time::SystemTime};

use anyhow::Context;
use axum::Router;
use hirsel_proto::HostToClient;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

use crate::{
    config::Config,
    lash_runtime::AgentRuntime,
    processes::ProcessStore,
    storage::Storage,
    tools::{ToolSuite, ToolsConfig},
};

#[derive(Clone)]
pub struct AppState {
    pub token: Arc<str>,
    pub storage: Storage,
    pub broadcaster: broadcast::Sender<HostToClient>,
    pub agent: AgentRuntime,
    pub processes: ProcessStore,
    pub started_at: SystemTime,
    pub debug_enabled: bool,
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
    let processes = ProcessStore::default();
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: config.driver,
            fake_fixture: config.fake_fixture.clone(),
        },
        storage.clone(),
        broadcaster.clone(),
        processes.clone(),
    );
    let agent = AgentRuntime::start(
        lash_runtime::RuntimeConfig {
            has_anthropic_key: config.anthropic_api_key.is_some(),
            model: config.model.clone(),
            data_dir: config.data_dir.clone(),
            driver_mode: config.driver,
        },
        tools,
        broadcaster.clone(),
    );
    Ok(AppState {
        token: Arc::from(config.token),
        storage,
        broadcaster,
        agent,
        processes,
        started_at: SystemTime::now(),
        debug_enabled: config.debug,
    })
}

pub fn router_from_state(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/ws", axum::routing::get(ws::ws_handler))
        .with_state(state.clone());
    if state.debug_enabled {
        app = app.merge(debug::routes(state.clone()));
    }
    if Path::new("app/dist").exists() {
        app = app.fallback_service(ServeDir::new("app/dist"));
    }
    app
}

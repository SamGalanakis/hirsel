use std::{path::PathBuf, sync::Arc};

use hirsel_drivers::{AgentKind, ClaudeCodeDriver, CodexDriver, FakeDriver, SubagentDriver};
use hirsel_proto::{HostToClient, SubagentModelCatalog};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::{
    BroadcastLog, config::DriverMode, storage::Storage, subagent_models::SubagentModelState,
};

mod digest;
mod monitors;
mod session;
pub(crate) mod shell;

mod threads;
mod views;

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
    pushes: crate::push::PushGateway,
    views: crate::templates::ViewManager,
    subagent_models: SubagentModelState,
    fake: Arc<FakeDriver>,
    claude: Arc<ClaudeCodeDriver>,
    codex: Arc<CodexDriver>,
    /// Tools contributed by enabled plugins. Empty until the plugin host
    /// registers into it, and empty forever when no plugin is installed.
    plugin_tools: crate::plugins::PluginToolRegistry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSessionBootstrap {
    pub session_id: String,
    pub handoff_seed: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellRunOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ToolSuite {
    /// The store the suite writes through. Exposed for the fork-wake
    /// dispatcher, which reads the same slice the Agent's tools do to build a
    /// triage context pack.
    pub(crate) fn storage(&self) -> Storage {
        self.storage.clone()
    }

    pub fn new(
        config: ToolsConfig,
        storage: Storage,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
        pushes: crate::push::PushGateway,
        views: crate::templates::ViewManager,
    ) -> Self {
        let subagent_models = config.subagent_models.clone();
        Self {
            config,
            storage,
            broadcaster,
            broadcast_log,
            pushes,
            views,
            subagent_models,
            fake: Arc::new(FakeDriver::default()),
            claude: Arc::new(ClaudeCodeDriver::default()),
            codex: Arc::new(CodexDriver::default()),
            plugin_tools: crate::plugins::PluginToolRegistry::default(),
        }
    }

    /// The live plugin tool table. The plugin host writes it; the agent tool
    /// provider reads it on every catalog resolution, so an enable/disable is
    /// visible without rebuilding the provider.
    pub(crate) fn plugin_tools(&self) -> &crate::plugins::PluginToolRegistry {
        &self.plugin_tools
    }

    pub(crate) fn subagent_model_snapshot(&self) -> SubagentModelCatalog {
        self.subagent_models.snapshot()
    }

    pub(crate) fn broadcast(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }

    pub(crate) fn driver_for(&self, agent: AgentKind) -> Arc<dyn SubagentDriver> {
        match (self.config.driver_mode, agent) {
            (DriverMode::Fake, _) => self.fake.clone(),
            (DriverMode::Real, AgentKind::Claude) => self.claude.clone(),
            (DriverMode::Real, AgentKind::Codex) => self.codex.clone(),
        }
    }
}

impl ToolSuite {
    pub(crate) fn resolve_thread_cli_model(
        &self,
        agent: AgentKind,
        model: Option<&str>,
        variant: Option<&str>,
    ) -> anyhow::Result<crate::subagent_models::ResolvedSubagentModel> {
        self.subagent_models.resolve(agent, model, variant)
    }
}

impl ToolSuite {
    pub(crate) fn driver_fixture(&self) -> Option<std::path::PathBuf> {
        self.config.fake_fixture.clone()
    }
}

impl ToolSuite {
    pub(crate) async fn reset_runtime_projections(&self) {
        self.views
            .clear_all(
                self.storage
                    .history_id()
                    .await
                    .expect("current history after reset"),
            )
            .await;
        self.broadcast_log.clear();
        self.pushes.clear_recorded_pushes();
    }
}

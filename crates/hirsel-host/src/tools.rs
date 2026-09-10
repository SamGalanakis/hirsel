use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use hirsel_drivers::{AgentKind, ClaudeCodeDriver, CodexDriver, FakeDriver, SubagentDriver};
use hirsel_proto::{HostToClient, SubagentModelCatalog, ThreadTurnState, TurnEvent, TurnEventKind};
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
    /// Volatile safety latch for event-loss failures whose durable failure
    /// projection may be blocked by the same transient SQLite outage.
    timeline_integrity_failures: Arc<Mutex<HashMap<u64, String>>>,
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
            timeline_integrity_failures: Arc::new(Mutex::new(HashMap::new())),
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

    /// Commit a timeline event before exposing it. SQLite assigns the shared
    /// per-turn sequence, so independent producers cannot collide or reorder
    /// the durable and live views.
    pub(crate) async fn publish_turn_event(
        &self,
        thread_id: u64,
        turn_id: u64,
        event: TurnEventKind,
    ) -> anyhow::Result<TurnEvent> {
        let stored = match self
            .storage
            .append_next_turn_event(thread_id, turn_id, event)
            .await
        {
            Ok(stored) => stored,
            Err(error) => {
                self.fail_turn_timeline_persistence(turn_id, &error).await;
                return Err(error);
            }
        };
        self.broadcast_stored_turn_event(thread_id, turn_id, &stored);
        Ok(stored)
    }

    /// Commit and broadcast without releasing an execution guard. This keeps
    /// the event on the same side of cancellation/reset as its tool operation.
    pub(crate) fn publish_guarded_turn_event(
        &self,
        guard: &tokio::sync::MutexGuard<'_, rusqlite::Connection>,
        thread_id: u64,
        turn_id: u64,
        event: TurnEventKind,
    ) -> anyhow::Result<TurnEvent> {
        let stored = match self
            .storage
            .append_next_turn_event_guarded(guard, thread_id, turn_id, event)
        {
            Ok(stored) => stored,
            Err(error) => {
                self.record_turn_timeline_integrity(
                    turn_id,
                    format!("Turn timeline persistence failed: {error}"),
                );
                return Err(error);
            }
        };
        self.broadcast_stored_turn_event(thread_id, turn_id, &stored);
        Ok(stored)
    }

    fn broadcast_stored_turn_event(&self, thread_id: u64, turn_id: u64, stored: &TurnEvent) {
        self.broadcast(HostToClient::TurnEvent {
            thread_id,
            turn_id,
            seq: stored.seq,
            event: stored.event.clone(),
        });
    }

    pub(crate) async fn fail_turn_timeline_persistence(&self, turn_id: u64, error: &anyhow::Error) {
        self.fail_turn_timeline_integrity(
            turn_id,
            &format!("Turn timeline persistence failed: {error}"),
        )
        .await;
    }

    pub(crate) async fn fail_turn_timeline_integrity(&self, turn_id: u64, reason: &str) {
        self.record_turn_timeline_integrity(turn_id, reason.to_string());
        match self.fail_turn_timeline(turn_id, reason).await {
            Ok(()) => self.clear_turn_timeline_integrity_failure(turn_id),
            Err(terminal_error) => {
                tracing::error!(turn_id, %terminal_error, "failed to persist terminal timeline failure");
            }
        }
    }

    pub(crate) fn record_turn_timeline_integrity(&self, turn_id: u64, reason: String) {
        self.timeline_integrity_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(turn_id)
            .or_insert(reason);
    }

    pub(crate) fn turn_timeline_integrity_failure(&self, turn_id: u64) -> Option<String> {
        self.timeline_integrity_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&turn_id)
            .cloned()
    }

    pub(crate) fn clear_turn_timeline_integrity_failure(&self, turn_id: u64) {
        self.timeline_integrity_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&turn_id);
    }

    async fn fail_turn_timeline(&self, turn_id: u64, reason: &str) -> anyhow::Result<()> {
        let history_id = self.storage.history_id().await?;
        let completion = self
            .storage
            .complete_thread_turn_with_failure(
                &history_id,
                turn_id,
                ThreadTurnState::Failed,
                None,
                Some(reason),
            )
            .await?;
        anyhow::ensure!(
            completion.turn.state == ThreadTurnState::Failed,
            "timeline persistence failed after the turn was already terminal"
        );
        if let Some(activity) = completion.failure_activity {
            self.publish_thread_activity(activity).await;
        }
        self.publish_thread_turn(completion.turn).await;
        Ok(())
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
        self.timeline_integrity_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
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

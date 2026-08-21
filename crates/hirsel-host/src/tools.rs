use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use hirsel_drivers::{
    AgentKind, ClaudeCodeDriver, CodexDriver, FakeDriver, SessionHandle, SubagentDriver,
    TerminalOutcome,
};
use hirsel_proto::{HostToClient, ProcessInfo, SubagentModelCatalog};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{
    BroadcastLog, config::DriverMode, processes::ProcessStore, storage::Storage,
    subagent_models::SubagentModelState,
};

mod events;
mod monitors;
mod session;
mod shell;
mod subagents;
mod views;

#[cfg(test)]
mod tests;

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
    processes: ProcessStore,
    pushes: crate::push::PushGateway,
    views: crate::templates::ViewManager,
    subagent_models: SubagentModelState,
    fake: Arc<FakeDriver>,
    claude: Arc<ClaudeCodeDriver>,
    codex: Arc<CodexDriver>,
    terminal_events: TerminalEventBus,
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
pub struct ProcessTerminal {
    pub process_id: String,
    pub handle: SessionHandle,
    pub outcome: TerminalOutcome,
}

#[derive(Clone)]
struct TerminalEventBus {
    tx: broadcast::Sender<ProcessTerminal>,
    retained: Arc<Mutex<HashMap<String, ProcessTerminal>>>,
}

pub(crate) struct TerminalEventReceiver {
    rx: broadcast::Receiver<ProcessTerminal>,
    retained: Arc<Mutex<HashMap<String, ProcessTerminal>>>,
    pending: VecDeque<ProcessTerminal>,
    seen: HashSet<String>,
}

impl TerminalEventBus {
    fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            retained: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn subscribe(&self) -> TerminalEventReceiver {
        let rx = self.tx.subscribe();
        let pending = self.retained_events();
        TerminalEventReceiver {
            rx,
            retained: Arc::clone(&self.retained),
            pending,
            seen: HashSet::new(),
        }
    }

    fn publish(&self, event: ProcessTerminal) {
        self.retained
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(event.process_id.clone(), event.clone());
        let _ = self.tx.send(event);
    }

    fn retained_events(&self) -> VecDeque<ProcessTerminal> {
        let mut events = self
            .retained
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(|left, right| left.process_id.cmp(&right.process_id));
        events.into()
    }
}

impl TerminalEventReceiver {
    pub(crate) async fn recv(&mut self) -> Result<ProcessTerminal, broadcast::error::RecvError> {
        loop {
            while let Some(event) = self.pending.pop_front() {
                if self.seen.insert(event.process_id.clone()) {
                    return Ok(event);
                }
            }
            match self.rx.recv().await {
                Ok(event) if self.seen.insert(event.process_id.clone()) => return Ok(event),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.pending = self
                        .retained
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .values()
                        .filter(|event| !self.seen.contains(&event.process_id))
                        .cloned()
                        .collect();
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SpawnedProcess {
    pub process_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub handle: SessionHandle,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellRunOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JudgmentOptionInput {
    #[serde(default)]
    pub key: Option<String>,
    pub label: String,
    pub detail: String,
    #[serde(default)]
    pub recommended: bool,
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
        processes: ProcessStore,
        pushes: crate::push::PushGateway,
        views: crate::templates::ViewManager,
    ) -> Self {
        let subagent_models = config.subagent_models.clone();
        Self {
            config,
            storage,
            broadcaster,
            broadcast_log,
            processes,
            pushes,
            views,
            subagent_models,
            fake: Arc::new(FakeDriver::default()),
            claude: Arc::new(ClaudeCodeDriver::default()),
            codex: Arc::new(CodexDriver::default()),
            terminal_events: TerminalEventBus::new(128),
            plugin_tools: crate::plugins::PluginToolRegistry::default(),
        }
    }

    /// The live plugin tool table. The plugin host writes it; the agent tool
    /// provider reads it on every catalog resolution, so an enable/disable is
    /// visible without rebuilding the provider.
    pub(crate) fn plugin_tools(&self) -> &crate::plugins::PluginToolRegistry {
        &self.plugin_tools
    }

    pub(crate) fn terminal_events(&self) -> TerminalEventReceiver {
        self.terminal_events.subscribe()
    }

    pub(crate) fn subagent_model_snapshot(&self) -> SubagentModelCatalog {
        self.subagent_models.snapshot()
    }

    fn broadcast(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }

    fn broadcast_process_upsert(&self, process: ProcessInfo) {
        publish_process_upsert(&self.broadcast_log, &self.broadcaster, process);
    }

    fn driver_for(&self, agent: AgentKind) -> Arc<dyn SubagentDriver> {
        match (self.config.driver_mode, agent) {
            (DriverMode::Fake, _) => self.fake.clone(),
            (DriverMode::Real, AgentKind::Claude) => self.claude.clone(),
            (DriverMode::Real, AgentKind::Codex) => self.codex.clone(),
        }
    }
}

fn info_ui(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "card",
        "children": [{ "type": "text", "text": text }]
    })
}

fn publish_process_upsert(
    broadcast_log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
    process: ProcessInfo,
) {
    let event = HostToClient::ProcessUpsert { process };
    broadcast_log.record(event.clone());
    let _ = broadcaster.send(event);
}

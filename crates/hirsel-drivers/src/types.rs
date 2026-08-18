//! Shared driver contract: errors, spawn/session types, events, and the
//! [`SubagentDriver`] trait every driver implements.

use std::{path::PathBuf, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type EventStream = Pin<Box<dyn Stream<Item = SubagentEvent> + Send>>;
pub type DriverResult<T> = Result<T, DriverError>;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("sub-agent session not found: {0}")]
    SessionNotFound(String),
    #[error("sub-agent session has no active turn")]
    NoActiveTurn,
    #[error("driver state lock was poisoned")]
    StatePoisoned,
    #[error("missing child pipe: {0}")]
    MissingPipe(&'static str),
    #[error("CLI did not return an external id before timeout")]
    MissingExternalId,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub agent: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub variant: Option<String>,
    pub prompt: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub fake_fixture: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionHandle {
    pub id: String,
    pub agent: AgentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentEvent {
    Started { external_id: String },
    Progress { summary: String },
    Terminal { outcome: TerminalOutcome },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TerminalOutcome {
    Done { summary: String },
    Failed { reason: String },
    Interrupted,
}

#[async_trait]
pub trait SubagentDriver: Send + Sync {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle>;
    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()>;
    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()>;
    async fn retire(&self, handle: &SessionHandle) -> DriverResult<()>;
    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream>;
}

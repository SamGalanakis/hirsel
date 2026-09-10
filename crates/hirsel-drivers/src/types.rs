//! Shared driver contract: errors, spawn/session types, events, and the
//! [`SubagentDriver`] trait every driver implements.

use std::{collections::HashSet, ffi::OsString, fmt, path::PathBuf, pin::Pin};

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
    #[error("provider protocol error: {0}")]
    Protocol(String),
    #[error("provider request timed out: {0}")]
    RequestTimeout(String),
    #[error("sub-agent session is closed")]
    SessionClosed,
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

/// Private, host-issued tool bridge for one accepted execution.
/// The driver passes paths to the bridge but never reads the capability file.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedMcpLaunch {
    pub host_executable: PathBuf,
    pub socket_path: PathBuf,
    pub capability_file: PathBuf,
    pub expected_tools: Vec<String>,
}

impl fmt::Debug for ScopedMcpLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedMcpLaunch")
            .field("host_executable", &"<private path>")
            .field("socket_path", &"<private path>")
            .field("capability_file", &"<private path>")
            .field("expected_tools", &self.expected_tools)
            .finish()
    }
}

impl ScopedMcpLaunch {
    pub fn validate(&self) -> DriverResult<()> {
        if !self.host_executable.is_absolute()
            || !self.socket_path.is_absolute()
            || !self.capability_file.is_absolute()
        {
            return Err(DriverError::Protocol(
                "scoped MCP launch requires absolute bridge paths".into(),
            ));
        }
        let mut names = HashSet::new();
        if self.expected_tools.is_empty()
            || self.expected_tools.iter().any(|name| {
                name.is_empty()
                    || name.trim() != name
                    || name.chars().any(char::is_control)
                    || !names.insert(name)
            })
        {
            return Err(DriverError::Protocol(
                "scoped MCP launch requires a nonempty unique canonical tool set".into(),
            ));
        }
        Ok(())
    }

    /// Arguments only: no shell parsing, config loading or capability reads.
    pub fn bridge_args(&self) -> Vec<OsString> {
        vec![
            "thread-tool-bridge".into(),
            "--socket".into(),
            self.socket_path.as_os_str().to_owned(),
            "--cap-file".into(),
            self.capability_file.as_os_str().to_owned(),
        ]
    }
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
    pub scoped_mcp: ScopedMcpLaunch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionHandle {
    pub id: String,
    pub agent: AgentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentEvent {
    Started {
        external_id: String,
    },
    Progress {
        summary: String,
    },
    /// Complete final assistant text, independent of bounded process summaries.
    AssistantOutput {
        text: String,
    },
    Terminal {
        outcome: TerminalOutcome,
    },
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

use std::sync::{Arc, Mutex};

use hirsel_client_core as core;
use thiserror::Error;
use tokio::runtime::Runtime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionState {
    Connecting,
    Online,
    Offline,
}

impl From<core::ConnectionState> for ConnectionState {
    fn from(value: core::ConnectionState) -> Self {
        match value {
            core::ConnectionState::Connecting => Self::Connecting,
            core::ConnectionState::Online => Self::Online,
            core::ConnectionState::Offline => Self::Offline,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ChatAuthor {
    Owner,
    Agent,
}

impl From<core::ChatAuthor> for ChatAuthor {
    fn from(value: core::ChatAuthor) -> Self {
        match value {
            core::ChatAuthor::Owner => Self::Owner,
            core::ChatAuthor::Agent => Self::Agent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PingStatus {
    Open,
    Done,
}

impl From<core::PingStatus> for PingStatus {
    fn from(value: core::PingStatus) -> Self {
        match value {
            core::PingStatus::Open => Self::Open,
            core::PingStatus::Done => Self::Done,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Blob {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl From<core::Blob> for Blob {
    fn from(value: core::Blob) -> Self {
        Self {
            id: value.id,
            name: value.name,
            mime: value.mime,
            size: value.size,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ToolCall {
    pub name: String,
    pub ok: bool,
}

impl From<core::ToolCallSummary> for ToolCall {
    fn from(value: core::ToolCallSummary) -> Self {
        Self {
            name: value.name,
            ok: value.ok,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ChatMessage {
    pub id: Option<u64>,
    pub author: ChatAuthor,
    pub body: String,
    pub reply_to: Option<u64>,
    pub timestamp: String,
    pub attachments: Vec<Blob>,
    pub tool_calls: Vec<ToolCall>,
    pub client_id: Option<String>,
    pub pending: bool,
}

impl From<core::ChatEntry> for ChatMessage {
    fn from(value: core::ChatEntry) -> Self {
        Self {
            id: value.id,
            author: value.author.into(),
            body: value.body,
            reply_to: value.reply_to,
            timestamp: value.timestamp,
            attachments: value.attachments.into_iter().map(Into::into).collect(),
            tool_calls: value.tool_calls.into_iter().map(Into::into).collect(),
            client_id: value.client_id,
            pending: value.pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuickReply {
    pub value: String,
    pub label: String,
}

impl From<core::QuickReply> for QuickReply {
    fn from(value: core::QuickReply) -> Self {
        Self {
            value: value.value,
            label: value.label,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Ping {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub content: String,
    pub anchor: u64,
    pub requires_response: bool,
    pub quick_replies: Vec<QuickReply>,
    pub status: PingStatus,
    pub read: bool,
    pub timestamp: String,
}

impl From<core::Ping> for Ping {
    fn from(value: core::Ping) -> Self {
        Self {
            id: value.id,
            name: value.name,
            description: value.description,
            content: value.content,
            anchor: value.anchor,
            requires_response: value.requires_response,
            quick_replies: value.quick_replies.into_iter().map(Into::into).collect(),
            status: value.status.into(),
            read: value.read,
            timestamp: value.ts.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AgentActivityState {
    Thinking,
    Idle,
}

impl From<core::AgentActivityState> for AgentActivityState {
    fn from(value: core::AgentActivityState) -> Self {
        match value {
            core::AgentActivityState::Thinking => Self::Thinking,
            core::AgentActivityState::Idle => Self::Idle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AgentActivity {
    pub state: AgentActivityState,
    pub text: Option<String>,
}

impl From<core::AgentActivity> for AgentActivity {
    fn from(value: core::AgentActivity) -> Self {
        Self {
            state: value.state.into(),
            text: value.text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ClientSnapshot {
    pub connection: ConnectionState,
    pub messages: Vec<ChatMessage>,
    pub pings: Vec<Ping>,
    pub agent_activity: AgentActivity,
    pub last_seen_msg_id: Option<u64>,
    /// Host build identity from the last `hello_ok`; `None` until reported.
    pub host_version: Option<String>,
}

impl From<core::ClientSnapshot> for ClientSnapshot {
    fn from(value: core::ClientSnapshot) -> Self {
        Self {
            connection: value.connection.into(),
            messages: value.messages.into_iter().map(Into::into).collect(),
            pings: value.pings.into_iter().map(Into::into).collect(),
            agent_activity: value.agent_activity.into(),
            last_seen_msg_id: value.last_seen_msg_id,
            host_version: value.host_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum LifecycleEvent {
    Connecting { attempt: u32 },
    Online,
    Offline { reason: Option<String> },
    ProtocolError { detail: String },
}

impl From<core::LifecycleEvent> for LifecycleEvent {
    fn from(value: core::LifecycleEvent) -> Self {
        match value {
            core::LifecycleEvent::Connecting { attempt } => Self::Connecting { attempt },
            core::LifecycleEvent::Online => Self::Online,
            core::LifecycleEvent::Offline { reason } => Self::Offline { reason },
            core::LifecycleEvent::ProtocolError { detail } => Self::ProtocolError { detail },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendReceipt {
    pub client_id: String,
}

#[derive(Debug, Error, uniffi::Error)]
pub enum ClientError {
    #[error("invalid client configuration: {detail}")]
    InvalidConfig { detail: String },
    #[error("client connection manager is already running")]
    AlreadyRunning,
    #[error("unsupported push platform: {platform}")]
    UnsupportedPushPlatform { platform: String },
    #[error("push token must not be empty")]
    EmptyPushToken,
    #[error("failed to initialize the client runtime: {detail}")]
    Runtime { detail: String },
}

impl From<core::ClientError> for ClientError {
    fn from(value: core::ClientError) -> Self {
        match value {
            core::ClientError::InvalidConfig(error) => Self::InvalidConfig {
                detail: error.to_string(),
            },
            core::ClientError::AlreadyRunning => Self::AlreadyRunning,
            core::ClientError::UnsupportedPushPlatform(platform) => {
                Self::UnsupportedPushPlatform { platform }
            }
            core::ClientError::EmptyPushToken => Self::EmptyPushToken,
        }
    }
}

#[uniffi::export(callback_interface)]
pub trait ClientObserver: Send + Sync {
    fn on_state_changed(&self, snapshot: ClientSnapshot);

    fn on_lifecycle_event(&self, event: LifecycleEvent);
}

struct ObserverAdapter {
    observer: Box<dyn ClientObserver>,
}

/// Generates a new persistent iroh identity for pairing and later reconnects.
#[uniffi::export]
pub fn generate_iroh_identity() -> String {
    core::generate_iroh_identity()
}

impl core::ClientObserver for ObserverAdapter {
    fn on_state_changed(&self, snapshot: core::ClientSnapshot) {
        self.observer.on_state_changed(snapshot.into());
    }

    fn on_lifecycle_event(&self, event: core::LifecycleEvent) {
        self.observer.on_lifecycle_event(event.into());
    }
}

#[derive(uniffi::Object)]
pub struct Client {
    core: core::Client,
    runtime: Mutex<Option<Runtime>>,
}

#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    pub fn new(
        host: String,
        token: String,
        observer: Box<dyn ClientObserver>,
    ) -> Result<Arc<Self>, ClientError> {
        Self::from_config(core::ClientConfig::new(host, token), observer)
    }

    /// Creates an iroh client authenticated by a previously issued device token.
    #[uniffi::constructor]
    pub fn new_iroh(
        ticket: String,
        device_token: String,
        iroh_secret_key: String,
        observer: Box<dyn ClientObserver>,
    ) -> Result<Arc<Self>, ClientError> {
        Self::from_config(
            core::ClientConfig::new_iroh(ticket, device_token, iroh_secret_key),
            observer,
        )
    }

    /// Creates an iroh client that redeems a one-time pairing code.
    #[uniffi::constructor]
    pub fn new_iroh_pairing(
        ticket: String,
        code: String,
        device_label: String,
        iroh_secret_key: String,
        observer: Box<dyn ClientObserver>,
    ) -> Result<Arc<Self>, ClientError> {
        Self::from_config(
            core::ClientConfig::new_iroh_pairing(ticket, code, device_label, iroh_secret_key),
            observer,
        )
    }

    pub fn connect(&self) -> Result<(), ClientError> {
        self.with_runtime(|runtime| runtime.block_on(self.core.connect()))?
            .map_err(Into::into)
    }

    pub fn disconnect(&self) -> Result<(), ClientError> {
        self.with_runtime(|runtime| runtime.block_on(self.core.disconnect()))?;
        Ok(())
    }

    pub fn send_message(&self, body: String) -> SendReceipt {
        let receipt = self.core.send_message(core::SendMessageRequest::new(body));
        SendReceipt {
            client_id: receipt.client_id,
        }
    }

    pub fn register_push_token(&self, platform: String, token: String) -> Result<(), ClientError> {
        self.core
            .register_push_token(platform, token)
            .map_err(Into::into)
    }

    pub fn snapshot(&self) -> ClientSnapshot {
        self.core.snapshot().into()
    }

    /// Returns the device token issued by a successful pairing handshake.
    ///
    /// Pairing clients should persist this value after reaching `Online`, then
    /// use `new_iroh` for later connections.
    pub fn issued_device_token(&self) -> Option<String> {
        self.core.paired_device_token()
    }
}

impl Client {
    fn from_config(
        config: core::ClientConfig,
        observer: Box<dyn ClientObserver>,
    ) -> Result<Arc<Self>, ClientError> {
        let core = core::Client::new(config)?;
        core.set_observer(Some(Arc::new(ObserverAdapter { observer })));
        let runtime = Runtime::new().map_err(|error| ClientError::Runtime {
            detail: error.to_string(),
        })?;
        Ok(Arc::new(Self {
            core,
            runtime: Mutex::new(Some(runtime)),
        }))
    }

    fn with_runtime<T>(&self, f: impl FnOnce(&Runtime) -> T) -> Result<T, ClientError> {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        runtime.as_ref().map(f).ok_or_else(|| ClientError::Runtime {
            detail: "runtime is shutting down".to_string(),
        })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(runtime) = self
            .runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            runtime.shutdown_background();
        }
    }
}

uniffi::setup_scaffolding!();

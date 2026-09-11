mod threads;
pub use threads::*;

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
    pub id: String,
    pub name: String,
    pub ok: bool,
}

impl From<core::ToolCallSummary> for ToolCall {
    fn from(value: core::ToolCallSummary) -> Self {
        Self {
            id: value.id,
            name: value.name,
            ok: value.ok,
        }
    }
}

#[cfg(test)]
mod tool_call_tests {
    use super::*;

    #[test]
    fn ffi_tool_call_keeps_canonical_id() {
        let call: ToolCall = core::ToolCallSummary {
            id: "call-7".to_string(),
            name: "shell_run".to_string(),
            ok: false,
        }
        .into();
        assert_eq!(call.id, "call-7");
        assert_eq!(call.name, "shell_run");
        assert!(!call.ok);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ChatMessage {
    pub error: Option<String>,
    pub thread_id: u64,
    pub mentions: Vec<u64>,
    pub artifact_ids: Vec<u64>,
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
        match value {
            core::ChatEntry::Confirmed(message) => Self {
                error: None,
                id: Some(message.id),
                thread_id: message.thread_id,
                mentions: message.mentions,
                artifact_ids: message.artifact_ids,
                author: message.author.into(),
                body: message.body,
                reply_to: message.reply_to,
                timestamp: message.timestamp,
                attachments: message.attachments.into_iter().map(Into::into).collect(),
                tool_calls: message.tool_calls.into_iter().map(Into::into).collect(),
                client_id: message.client_id,
                pending: false,
            },
            core::ChatEntry::Pending(send) => Self {
                error: send.error,
                id: None,
                thread_id: send.thread_id,
                mentions: send.mentions,
                artifact_ids: send.artifact_ids,
                author: ChatAuthor::Owner,
                body: send.body,
                reply_to: None,
                timestamp: send.timestamp,
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                client_id: Some(send.client_id),
                pending: true,
            },
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
    pub threads: Vec<Thread>,
    pub turns: Vec<ThreadTurn>,
    pub activities: Vec<ThreadActivity>,
    pub briefs: Vec<ThreadBrief>,
    pub related_items: Vec<ThreadRelatedItem>,
    pub streams: Vec<ThreadStream>,
    pub opened_threads: Vec<u64>,
    pub history_has_more: Vec<u64>,
    pub created_threads: Vec<CreatedThread>,
    pub history_id: Option<String>,
    pub recovered_drafts: Vec<String>,
    /// Host build identity from the last `hello_ok`; `None` until reported.
    pub host_version: Option<String>,
}

impl From<core::ClientSnapshot> for ClientSnapshot {
    fn from(value: core::ClientSnapshot) -> Self {
        Self {
            connection: value.connection.into(),
            messages: value.messages.into_iter().map(Into::into).collect(),
            threads: value.threads.into_iter().map(Into::into).collect(),
            turns: value.turns.into_iter().map(Into::into).collect(),
            activities: value.activities.into_iter().map(Into::into).collect(),
            briefs: value.briefs.into_iter().map(Into::into).collect(),
            related_items: value.related_items.into_iter().map(Into::into).collect(),
            streams: value.streams.into_iter().map(Into::into).collect(),
            opened_threads: value.opened_threads,
            history_has_more: value.history_has_more,
            created_threads: value.created_threads.into_iter().map(Into::into).collect(),
            history_id: value.history_id,
            recovered_drafts: value.recovered_drafts,
            host_version: value.host_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum LifecycleEvent {
    Connecting {
        attempt: u32,
    },
    Online,
    Offline {
        reason: Option<String>,
    },
    ProtocolError {
        detail: String,
        client_id: Option<String>,
    },
    ThreadActionApplied {
        client_id: String,
        history_id: String,
        thread_id: u64,
    },
    ThreadOpened {
        client_id: String,
        thread_id: u64,
    },
    ThreadRelatedChanged {
        history_id: String,
        thread_id: u64,
        client_id: Option<String>,
    },
}

impl From<core::LifecycleEvent> for LifecycleEvent {
    fn from(value: core::LifecycleEvent) -> Self {
        match value {
            core::LifecycleEvent::Connecting { attempt } => Self::Connecting { attempt },
            core::LifecycleEvent::Online => Self::Online,
            core::LifecycleEvent::Offline { reason } => Self::Offline { reason },
            core::LifecycleEvent::ProtocolError { detail, client_id } => {
                Self::ProtocolError { detail, client_id }
            }
            core::LifecycleEvent::ThreadActionApplied {
                client_id,
                history_id,
                thread_id,
            } => Self::ThreadActionApplied {
                client_id,
                history_id,
                thread_id,
            },
            core::LifecycleEvent::ThreadOpened {
                client_id,
                thread_id,
            } => Self::ThreadOpened {
                client_id,
                thread_id,
            },
            core::LifecycleEvent::ThreadRelatedChanged {
                history_id,
                thread_id,
                client_id,
            } => Self::ThreadRelatedChanged {
                history_id,
                thread_id,
                client_id,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendReceipt {
    pub client_id: String,
}

#[derive(Debug, Error, uniffi::Error)]
pub enum ClientError {
    #[error("invalid thread action: {detail}")]
    InvalidAction { detail: String },
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

    pub fn retry_send(&self, client_id: String) {
        self.core.retry_send(client_id);
    }

    pub fn create_thread(
        &self,
        history_id: String,
        title: String,
        kind: threads::ThreadKind,
        parent_thread_id: Option<u64>,
    ) -> Option<SendReceipt> {
        self.core
            .create_thread(history_id, title, kind.into(), parent_thread_id)
            .map(|receipt| SendReceipt {
                client_id: receipt.client_id,
            })
    }

    pub fn open_thread(&self, thread_id: u64, before_id: Option<u64>) -> SendReceipt {
        SendReceipt {
            client_id: self.core.open_thread(thread_id, before_id).client_id,
        }
    }

    pub fn send_thread_message(
        &self,
        history_id: String,
        thread_id: u64,
        body: String,
        attachments: Vec<String>,
        mentions: Vec<u64>,
        artifact_ids: Vec<u64>,
    ) -> Option<SendReceipt> {
        let mut request = core::SendThreadMessageRequest::new(history_id, thread_id, body);
        request.thread_id = thread_id;
        request.attachments = attachments;
        request.mentions = mentions;
        request.artifact_ids = artifact_ids;
        self.core.send_message(request).map(|receipt| SendReceipt {
            client_id: receipt.client_id,
        })
    }

    pub fn thread_action(
        &self,
        history_id: String,
        thread_id: u64,
        action: String,
        data_json: String,
        expected_revision: Option<u64>,
    ) -> Result<Option<SendReceipt>, ClientError> {
        let data =
            serde_json::from_str(&data_json).map_err(|error| ClientError::InvalidAction {
                detail: error.to_string(),
            })?;
        Ok(self
            .core
            .thread_action(history_id, thread_id, action, data, expected_revision)
            .map(|receipt| SendReceipt {
                client_id: receipt.client_id,
            }))
    }

    pub fn open_related_thread(&self, target: ThreadRelatedTarget) -> Option<SendReceipt> {
        self.core
            .open_related_thread(target.into())
            .map(|receipt| SendReceipt {
                client_id: receipt.client_id,
            })
    }

    pub fn add_thread_related(
        &self,
        history_id: String,
        thread_id: u64,
        target: ThreadRelatedTarget,
        title: Option<String>,
    ) -> SendReceipt {
        SendReceipt {
            client_id: self
                .core
                .add_thread_related(history_id, thread_id, target.into(), title)
                .client_id,
        }
    }

    pub fn remove_thread_related(
        &self,
        history_id: String,
        thread_id: u64,
        item_id: u64,
    ) -> SendReceipt {
        SendReceipt {
            client_id: self
                .core
                .remove_thread_related(history_id, thread_id, item_id)
                .client_id,
        }
    }

    pub fn update_thread_icon(
        &self,
        expected_history: String,
        thread_id: u64,
        icon: Option<String>,
        expected_revision: u64,
    ) -> Option<SendReceipt> {
        self.core
            .update_thread_icon(expected_history, thread_id, icon, expected_revision)
            .map(|receipt| SendReceipt {
                client_id: receipt.client_id,
            })
    }

    pub fn update_thread_showcase(
        &self,
        expected_history: String,
        thread_id: u64,
        artifact_id: Option<u64>,
        expected_revision: u64,
    ) -> Option<SendReceipt> {
        self.core
            .update_thread_showcase(expected_history, thread_id, artifact_id, expected_revision)
            .map(|receipt| SendReceipt {
                client_id: receipt.client_id,
            })
    }

    pub fn cancel_turn(&self, history_id: String, thread_id: u64) -> bool {
        self.core.cancel_turn(history_id, thread_id)
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

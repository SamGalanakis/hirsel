use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock, Weak};

use hirsel_proto::{ClientToHost, HelloAuth, PushPlatform};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{ClientConfig, ConfigError};
use crate::observer::{ClientObserver, LifecycleEvent};
use crate::store::{ClientSnapshot, LocalStore, PendingSend};
use crate::transport;

/// Explicitly addressed Thread send arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendThreadMessageRequest {
    pub history_id: String,
    pub thread_id: u64,
    pub attachments: Vec<String>,
    pub body: String,
    pub mentions: Vec<u64>,
    pub artifact_ids: Vec<u64>,
}

impl SendThreadMessageRequest {
    pub fn new(history_id: String, thread_id: u64, body: String) -> Self {
        Self {
            history_id,
            thread_id,
            attachments: Vec::new(),
            body,
            mentions: Vec::new(),
            artifact_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub client_id: String,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    InvalidConfig(#[from] ConfigError),
    #[error("client connection manager is already running")]
    AlreadyRunning,
    #[error("unsupported push platform: {0}")]
    UnsupportedPushPlatform(String),
    #[error("push token must not be empty")]
    EmptyPushToken,
}

pub(crate) enum Command {
    SendPending,
    Retry(String),
    Stop,
}

pub(crate) struct ClientInner {
    pub config: ClientConfig,
    pub store: RwLock<LocalStore>,
    pub pending_frames: Mutex<VecDeque<ClientToHost>>,
    pub auth: RwLock<HelloAuth>,
    pub iroh_secret_key: Option<iroh::SecretKey>,
    paired_device_token: RwLock<Option<String>>,
    observer: RwLock<Option<Arc<dyn ClientObserver>>>,
    command_tx: Mutex<Option<mpsc::UnboundedSender<Command>>>,
    task: AsyncMutex<Option<JoinHandle<()>>>,
}

impl ClientInner {
    pub fn set_connection(&self, state: crate::ConnectionState) {
        self.write_store().connection = state;
        self.notify_snapshot();
    }

    pub fn notify_snapshot(&self) {
        let snapshot = self.read_store().snapshot();
        let observer = { self.read_observer().clone() };
        if let Some(observer) = observer {
            observer.on_state_changed(snapshot);
        }
    }

    pub fn notify_lifecycle(&self, event: LifecycleEvent) {
        let observer = { self.read_observer().clone() };
        if let Some(observer) = observer {
            observer.on_lifecycle_event(event);
        }
    }

    pub fn read_store(&self) -> std::sync::RwLockReadGuard<'_, LocalStore> {
        self.store.read().unwrap_or_else(|error| error.into_inner())
    }

    pub fn write_store(&self) -> std::sync::RwLockWriteGuard<'_, LocalStore> {
        self.store
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn read_observer(&self) -> std::sync::RwLockReadGuard<'_, Option<Arc<dyn ClientObserver>>> {
        self.observer
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn current_auth(&self) -> HelloAuth {
        self.auth
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn capture_paired_device_token(&self, token: String) {
        *self.auth.write().unwrap_or_else(|error| error.into_inner()) =
            HelloAuth::DeviceToken(token.clone());
        *self
            .paired_device_token
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(token);
    }
}

/// Cheaply cloneable handle to the shared client state and transport manager.
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        config.validate()?;
        let auth = config.auth.clone();
        let iroh_secret_key = config.parsed_iroh_secret_key()?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                config,
                store: RwLock::new(LocalStore::default()),
                pending_frames: Mutex::new(VecDeque::new()),
                auth: RwLock::new(auth),
                iroh_secret_key,
                paired_device_token: RwLock::new(None),
                observer: RwLock::new(None),
                command_tx: Mutex::new(None),
                task: AsyncMutex::new(None),
            }),
        })
    }

    /// Start the connection manager. This returns after spawning; observe the
    /// `Connecting` and `Online` state transitions for readiness.
    pub async fn connect(&self) -> Result<(), ClientError> {
        let mut task = self.inner.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return Err(ClientError::AlreadyRunning);
        }
        if let Some(finished) = task.take() {
            let _ = finished.await;
        }

        let (command_tx, command_rx) = mpsc::unbounded_channel();
        *self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(command_tx);
        let weak = Arc::downgrade(&self.inner);
        *task = Some(tokio::spawn(async move {
            transport::run(weak, command_rx).await;
        }));
        Ok(())
    }

    /// Stop reconnecting and close the active socket, if any.
    pub async fn disconnect(&self) {
        let sender = self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(Command::Stop);
        }
        if let Some(task) = self.inner.task.lock().await.take() {
            let _ = task.await;
        }
    }

    pub fn send_message(&self, request: SendThreadMessageRequest) -> Option<SendReceipt> {
        let client_id = Uuid::new_v4().to_string();
        let mut store = self.inner.write_store();
        if store.history_id.as_deref() != Some(&request.history_id) {
            return None;
        }
        store.add_optimistic_send(PendingSend::new(
            request.history_id,
            request.thread_id,
            request.attachments,
            client_id.clone(),
            request.body,
            request.mentions,
            request.artifact_ids,
        ));
        drop(store);
        self.inner.notify_snapshot();
        if let Some(sender) = self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            let _ = sender.send(Command::SendPending);
        }
        Some(SendReceipt { client_id })
    }

    pub fn retry_send(&self, client_id: String) {
        for entry in &mut self.inner.write_store().messages {
            if let crate::ChatEntry::Pending(send) = entry
                && send.client_id == client_id
            {
                send.error = None;
            }
        }
        if let Some(sender) = self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = sender.send(Command::Retry(client_id));
        }
        self.inner.notify_snapshot();
    }

    fn queue_frame(&self, frame: ClientToHost) {
        self.inner
            .pending_frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(frame);
        if let Some(sender) = self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = sender.send(Command::SendPending);
        }
    }

    pub fn create_thread(
        &self,
        history_id: String,
        title: String,
        kind: hirsel_proto::ThreadKind,
        parent_thread_id: Option<u64>,
    ) -> Option<SendReceipt> {
        let client_id = Uuid::new_v4().to_string();
        let mut store = self.inner.write_store();
        if store.history_id.as_deref() != Some(&history_id) {
            return None;
        }
        store
            .pending_creates
            .push((client_id.clone(), history_id, title, kind, parent_thread_id));
        drop(store);
        if let Some(sender) = self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = sender.send(Command::SendPending);
        }
        Some(SendReceipt { client_id })
    }

    pub fn open_thread(&self, thread_id: u64, before_id: Option<u64>) -> SendReceipt {
        let client_id = Uuid::new_v4().to_string();
        self.inner
            .write_store()
            .requests
            .push((client_id.clone(), thread_id));
        self.queue_frame(ClientToHost::OpenThread {
            client_id: client_id.clone(),
            thread_id,
            before_id,
        });
        SendReceipt { client_id }
    }

    /// Validate a saved Thread target against live native state while queueing.
    /// UI snapshots can lag a reset; history must survive the UI/FFI boundary.
    pub fn open_related_thread(
        &self,
        target: hirsel_proto::ThreadRelatedTarget,
    ) -> Option<SendReceipt> {
        let hirsel_proto::ThreadRelatedTarget::Thread {
            history_id,
            thread_id,
        } = target
        else {
            return None;
        };
        let mut store = self.inner.write_store();
        if store.connection != crate::ConnectionState::Online
            || store.history_id.as_deref() != Some(&history_id)
            || !store.threads.iter().any(|thread| thread.id == thread_id)
        {
            return None;
        }
        let client_id = Uuid::new_v4().to_string();
        store.requests.push((client_id.clone(), thread_id));
        self.queue_frame(ClientToHost::OpenThread {
            client_id: client_id.clone(),
            thread_id,
            before_id: None,
        });
        drop(store);
        Some(SendReceipt { client_id })
    }

    pub fn thread_action(
        &self,
        history_id: String,
        thread_id: u64,
        action: String,
        data: serde_json::Value,
        expected_revision: Option<u64>,
    ) -> Option<SendReceipt> {
        let store = self.inner.read_store();
        if store.history_id.as_deref() != Some(&history_id) {
            return None;
        }
        let client_id = Uuid::new_v4().to_string();
        self.queue_frame(ClientToHost::ThreadAction {
            client_id: client_id.clone(),
            history_id,
            thread_id,
            action,
            data,
            expected_revision,
        });
        drop(store);
        Some(SendReceipt { client_id })
    }

    /// Save a typed reference in the history the caller was viewing when it chose the Thread.
    /// Never replace this explicit history with the current connection history:
    /// delayed actions must not address reused IDs after a host reset.
    pub fn add_thread_related(
        &self,
        history_id: String,
        thread_id: u64,
        target: hirsel_proto::ThreadRelatedTarget,
        title: Option<String>,
    ) -> SendReceipt {
        let client_id = Uuid::new_v4().to_string();
        self.queue_frame(ClientToHost::AddThreadRelated {
            client_id: client_id.clone(),
            history_id,
            thread_id,
            target,
            title,
        });
        SendReceipt { client_id }
    }

    /// Remove a saved reference in the caller's explicitly addressed history.
    pub fn remove_thread_related(
        &self,
        history_id: String,
        thread_id: u64,
        item_id: u64,
    ) -> SendReceipt {
        let client_id = Uuid::new_v4().to_string();
        self.queue_frame(ClientToHost::RemoveThreadRelated {
            client_id: client_id.clone(),
            history_id,
            thread_id,
            item_id,
        });
        SendReceipt { client_id }
    }

    /// Queue an icon edit only for the exact history and Thread the picker saw.
    /// Holding the store guard through enqueue pairs with Hello's store→queue
    /// lock order, so a history replacement cannot slip between check and write.
    pub fn update_thread_icon(
        &self,
        expected_history: String,
        thread_id: u64,
        icon: Option<String>,
        expected_revision: u64,
    ) -> Option<SendReceipt> {
        self.update_thread_presentation(
            expected_history,
            thread_id,
            "set_icon",
            serde_json::json!({"icon": icon}),
            expected_revision,
        )
    }

    pub fn update_thread_showcase(
        &self,
        expected_history: String,
        thread_id: u64,
        artifact_id: Option<u64>,
        expected_revision: u64,
    ) -> Option<SendReceipt> {
        let data = serde_json::json!({"artifact_id": artifact_id});
        self.update_thread_presentation(
            expected_history,
            thread_id,
            "set_showcase",
            data,
            expected_revision,
        )
    }

    fn update_thread_presentation(
        &self,
        expected_history: String,
        thread_id: u64,
        action: &str,
        data: serde_json::Value,
        expected_revision: u64,
    ) -> Option<SendReceipt> {
        let store = self.inner.read_store();
        if store.history_id.as_deref() != Some(expected_history.as_str())
            || store.connection != crate::ConnectionState::Online
            || !store
                .threads
                .iter()
                .any(|thread| thread.id == thread_id && thread.revision == expected_revision)
        {
            return None;
        }
        let client_id = Uuid::new_v4().to_string();
        self.queue_frame(ClientToHost::ThreadAction {
            client_id: client_id.clone(),
            history_id: expected_history,
            thread_id,
            action: action.into(),
            data,
            expected_revision: Some(expected_revision),
        });
        drop(store);
        Some(SendReceipt { client_id })
    }

    pub fn cancel_turn(&self, history_id: String, thread_id: u64) -> bool {
        let store = self.inner.read_store();
        if store.history_id.as_deref() != Some(&history_id) {
            return false;
        }
        self.queue_frame(ClientToHost::CancelTurn {
            history_id,
            thread_id,
        });
        drop(store);
        true
    }

    /// Register a push token once the WebSocket is online. Registrations made
    /// while disconnected remain queued until the next successful handshake.
    pub fn register_push_token(&self, platform: String, token: String) -> Result<(), ClientError> {
        let platform = match platform.as_str() {
            "android" => PushPlatform::Android,
            "web" => PushPlatform::Web,
            "ios" => PushPlatform::Ios,
            _ => return Err(ClientError::UnsupportedPushPlatform(platform)),
        };
        if token.trim().is_empty() {
            return Err(ClientError::EmptyPushToken);
        }

        self.inner
            .pending_frames
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back(ClientToHost::RegisterPushToken { platform, token });
        if let Some(sender) = self
            .inner
            .command_tx
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            let _ = sender.send(Command::SendPending);
        }
        Ok(())
    }

    pub fn snapshot(&self) -> ClientSnapshot {
        self.inner.read_store().snapshot()
    }

    /// Returns the token issued during this client's pairing handshake.
    pub fn paired_device_token(&self) -> Option<String> {
        self.inner
            .paired_device_token
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Register or replace the observer. Passing `None` unregisters it.
    pub fn set_observer(&self, observer: Option<Arc<dyn ClientObserver>>) {
        *self
            .inner
            .observer
            .write()
            .unwrap_or_else(|error| error.into_inner()) = observer;
    }
}

pub(crate) fn pending_to_wire(send: &PendingSend) -> ClientToHost {
    ClientToHost::SendThreadMessage {
        client_id: send.client_id.clone(),
        history_id: send.history_id.clone(),
        thread_id: send.thread_id,
        body: send.body.clone(),
        attachments: send.attachments.clone(),
        mode: hirsel_proto::SendMode::Send,
        mentions: send.mentions.clone(),
        artifact_ids: send.artifact_ids.clone(),
    }
}

pub(crate) fn upgrade(weak: &Weak<ClientInner>) -> Option<Arc<ClientInner>> {
    weak.upgrade()
}

#[cfg(test)]
#[path = "icon_tests.rs"]
mod icon_tests;

#[cfg(test)]
#[path = "showcase_tests.rs"]
mod showcase_tests;

#[cfg(test)]
#[path = "history_mutation_tests.rs"]
mod history_mutation_tests;

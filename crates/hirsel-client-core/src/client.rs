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

/// Owned send arguments. Later slices will add attachments and send mode here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendMessageRequest {
    pub body: String,
    pub reply_to: Option<u64>,
    pub mentions: Vec<u64>,
}

impl SendMessageRequest {
    pub fn new(body: String) -> Self {
        Self {
            body,
            reply_to: None,
            mentions: Vec::new(),
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

    pub fn send_message(&self, request: SendMessageRequest) -> SendReceipt {
        let client_id = Uuid::new_v4().to_string();
        self.inner
            .write_store()
            .add_optimistic_send(PendingSend::new(
                client_id.clone(),
                request.body,
                request.reply_to,
                request.mentions,
            ));
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
        SendReceipt { client_id }
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

pub(crate) fn pending_to_wire(pending: &PendingSend) -> ClientToHost {
    ClientToHost::SendMessage {
        client_id: pending.client_id.clone(),
        body: pending.body.clone(),
        r#ref: pending.reply_to,
        attachments: Vec::new(),
        mode: hirsel_proto::SendMode::Send,
        sc: None,
        mentions: pending.mentions.clone(),
    }
}

pub(crate) fn upgrade(weak: &Weak<ClientInner>) -> Option<Arc<ClientInner>> {
    weak.upgrade()
}

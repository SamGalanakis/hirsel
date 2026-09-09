//! `PluginCtx` — the cheap-clone handle a plugin uses to reach the host.
//!
//! Every capability here is a trait the host implements. A plugin never sees a
//! host type, which is what keeps this crate free of a dependency edge back
//! into `hirsel-host`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::watch;

/// The plugin's current setting values, keyed by descriptor key. Defaults are
/// already folded in; secret values are present in cleartext (the masking
/// happens at the management API boundary, not here).
pub type SettingsSnapshot = Arc<Map<String, Value>>;

/// A durable thread a plugin wants to create, independently of attention.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub title: String,
    pub description: String,
    /// Constrained semantic UI, validated by the same host catalog as Agent instruments.
    pub instrument: Value,
    pub needs_owner: bool,
}

impl NewThread {
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            instrument: Value::Null,
            needs_owner: false,
        }
    }

    pub fn with_instrument(mut self, instrument: Value) -> Self {
        self.instrument = instrument;
        self
    }

    pub fn needs_owner(mut self) -> Self {
        self.needs_owner = true;
        self
    }
}

/// Activity is an occurrence within an existing thread, never a new work item.
#[derive(Debug, Clone)]
pub struct NewActivity {
    pub thread_id: u64,
    pub kind: String,
    pub data: Value,
}

impl NewActivity {
    /// Record informational activity in the global orchestrator conversation.
    pub fn new(kind: impl Into<String>, data: Value) -> Self {
        Self {
            thread_id: 0,
            kind: kind.into(),
            data,
        }
    }

    pub fn in_thread(mut self, thread_id: u64) -> Self {
        self.thread_id = thread_id;
        self
    }
}

/// Explicitly distinguishes an activity identity from its owning thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityReceipt {
    pub activity_id: u64,
    pub thread_id: u64,
}

/// Durable work and activity use separate operations and identities.
#[async_trait]
pub trait PluginThreads: Send + Sync {
    /// Create visible work. Returns its Thread id; no decision is required.
    async fn create(&self, thread: NewThread) -> Result<u64, String>;
    /// Append informational activity with host-stamped plugin provenance.
    async fn append_activity(&self, activity: NewActivity) -> Result<ActivityReceipt, String>;
    /// Explicitly settle or reopen a Thread. Reading activity never settles it.
    async fn settle(&self, thread_id: u64, settled: bool) -> Result<(), String>;
}

/// Durable per-plugin key/value storage. Keys live in a namespace private to
/// the plugin; two plugins cannot see each other's keys.
#[async_trait]
pub trait PluginKv: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Value>, String>;
    async fn set(&self, key: &str, value: Value) -> Result<(), String>;
    async fn delete(&self, key: &str) -> Result<(), String>;
    async fn entries(&self) -> Result<Vec<(String, Value)>, String>;
}

/// Read the plugin's settings, and observe changes made from the app.
pub trait PluginSettingsAccess: Send + Sync {
    fn values(&self) -> SettingsSnapshot;
    /// A `watch` receiver that fires whenever Settings are saved. The current
    /// snapshot is already in the channel, so `changed()` is the right way to
    /// wait for the *next* change.
    fn watch(&self) -> watch::Receiver<SettingsSnapshot>;
}

/// Broadcast a message to every connected app client as a `plugin_push` frame.
pub trait PluginPush: Send + Sync {
    fn push(&self, topic: &str, data: Value);
}

struct CtxInner {
    id: String,
    label: String,
    threads: Arc<dyn PluginThreads>,
    kv: Arc<dyn PluginKv>,
    settings: Arc<dyn PluginSettingsAccess>,
    push: Arc<dyn PluginPush>,
}

/// Cheap-clone handle to everything a plugin may do to the host.
#[derive(Clone)]
pub struct PluginCtx {
    inner: Arc<CtxInner>,
}

impl PluginCtx {
    /// Constructed by the host; a plugin only ever receives one.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        threads: Arc<dyn PluginThreads>,
        kv: Arc<dyn PluginKv>,
        settings: Arc<dyn PluginSettingsAccess>,
        push: Arc<dyn PluginPush>,
    ) -> Self {
        Self {
            inner: Arc::new(CtxInner {
                id: id.into(),
                label: label.into(),
                threads,
                kv,
                settings,
                push,
            }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn label(&self) -> &str {
        &self.inner.label
    }

    pub fn threads(&self) -> &dyn PluginThreads {
        self.inner.threads.as_ref()
    }

    pub fn kv(&self) -> &dyn PluginKv {
        self.inner.kv.as_ref()
    }

    pub fn settings(&self) -> &dyn PluginSettingsAccess {
        self.inner.settings.as_ref()
    }

    /// Current value of one setting, defaults already folded in.
    pub fn setting(&self, key: &str) -> Option<Value> {
        self.inner.settings.values().get(key).cloned()
    }

    pub fn setting_str(&self, key: &str) -> Option<String> {
        self.setting(key)
            .and_then(|value| value.as_str().map(str::to_string))
    }

    pub fn setting_bool(&self, key: &str) -> Option<bool> {
        self.setting(key).and_then(|value| value.as_bool())
    }

    /// Observe setting changes made from the app.
    pub fn watch_settings(&self) -> watch::Receiver<SettingsSnapshot> {
        self.inner.settings.watch()
    }

    /// Broadcast `data` under `topic` to every connected client.
    pub fn push(&self, topic: &str, data: Value) {
        self.inner.push.push(topic, data);
    }

    /// A plugin-scoped logger. Every line carries `plugin = <id>`.
    pub fn log(&self) -> PluginLog<'_> {
        PluginLog { id: &self.inner.id }
    }
}

impl std::fmt::Debug for PluginCtx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginCtx")
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

/// `tracing` emitter scoped to one plugin.
pub struct PluginLog<'a> {
    id: &'a str,
}

impl PluginLog<'_> {
    pub fn debug(&self, message: &str) {
        tracing::debug!(plugin = %self.id, "{message}");
    }

    pub fn info(&self, message: &str) {
        tracing::info!(plugin = %self.id, "{message}");
    }

    pub fn warn(&self, message: &str) {
        tracing::warn!(plugin = %self.id, "{message}");
    }

    pub fn error(&self, message: &str) {
        tracing::error!(plugin = %self.id, "{message}");
    }
}

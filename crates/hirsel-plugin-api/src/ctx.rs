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

/// A typed Event the plugin wants to raise in Sam's feed.
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// Short handle, e.g. `"build-failed"`.
    pub name: String,
    /// The card heading — for a judgment, the question.
    pub description: String,
    /// Optional markdown body. It must add information beyond the heading.
    pub content_md: Option<String>,
}

impl NewEvent {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            content_md: None,
        }
    }

    pub fn with_content(mut self, content_md: impl Into<String>) -> Self {
        self.content_md = Some(content_md.into());
        self
    }
}

/// One choice on a judgment Event.
#[derive(Debug, Clone)]
pub struct EventOption {
    pub label: String,
    pub detail: String,
    pub recommended: bool,
}

impl EventOption {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            recommended: false,
        }
    }

    pub fn recommended(mut self) -> Self {
        self.recommended = true;
        self
    }
}

/// Create and resolve typed Events. These mirror the host's own
/// `events.notify` / `events.summary` / `events.judgment` tools; a plugin Event
/// lands in the same feed with the same lifecycle.
#[async_trait]
pub trait PluginEvents: Send + Sync {
    /// A quiet FYI. Returns the new Event id.
    async fn notify(&self, event: NewEvent) -> Result<u64, String>;
    /// A digest-shaped Event.
    async fn summary(&self, event: NewEvent) -> Result<u64, String>;
    /// A decision that needs Sam. Supply 2–4 options.
    async fn judgment(&self, event: NewEvent, options: Vec<EventOption>) -> Result<u64, String>;
    /// Settle an Event this plugin raised.
    async fn resolve(&self, event_id: u64) -> Result<(), String>;
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
    events: Arc<dyn PluginEvents>,
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
        events: Arc<dyn PluginEvents>,
        kv: Arc<dyn PluginKv>,
        settings: Arc<dyn PluginSettingsAccess>,
        push: Arc<dyn PluginPush>,
    ) -> Self {
        Self {
            inner: Arc::new(CtxInner {
                id: id.into(),
                label: label.into(),
                events,
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

    pub fn events(&self) -> &dyn PluginEvents {
        self.inner.events.as_ref()
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

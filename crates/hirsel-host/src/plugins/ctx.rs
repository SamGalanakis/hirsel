//! Host implementations of the `PluginCtx` capability traits.

use std::sync::Arc;

use async_trait::async_trait;
use hirsel_plugin_api::{
    EventOption, NewEvent, PluginEvents, PluginKv, PluginPush, PluginSettingsAccess,
    SettingsSnapshot,
};
use hirsel_proto::{ChatAuthor, HostToClient};
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::{
    BroadcastLog,
    storage::Storage,
    tools::{JudgmentOptionInput, ToolSuite},
};

/// Events raised by a plugin go through the same `ToolSuite` seam the agent's
/// own `events.*` tools use, so they land in Sam's feed with identical shape,
/// broadcast, and push behaviour. The only host-side difference is the anchor:
/// a plugin has no turn, so it anchors to the newest chat message (appending a
/// bootstrap message when the log is still empty, the way a scheduled digest
/// does).
pub(super) struct HostEvents {
    pub(super) plugin_id: String,
    pub(super) label: String,
    pub(super) tools: ToolSuite,
    pub(super) storage: Storage,
}

impl HostEvents {
    async fn anchor(&self) -> Result<u64, String> {
        let latest = self.storage.latest_msg_id().await.map_err(stringify)?;
        if latest != 0 {
            return Ok(latest);
        }
        self.storage
            .append_chat(
                ChatAuthor::Agent,
                format!("Plugin `{}` raised its first Event.", self.label),
                None,
            )
            .await
            .map(|message| message.id)
            .map_err(stringify)
    }
}

#[async_trait]
impl PluginEvents for HostEvents {
    async fn notify(&self, event: NewEvent) -> Result<u64, String> {
        let anchor = self.anchor().await?;
        self.tools
            .events_notify(
                event_name(&self.plugin_id, &event.name),
                event.description,
                event.content_md,
                anchor,
            )
            .await
            .map(|event| event.id)
            .map_err(stringify)
    }

    async fn summary(&self, event: NewEvent) -> Result<u64, String> {
        let anchor = self.anchor().await?;
        let content = event
            .content_md
            .clone()
            .unwrap_or_else(|| event.description.clone());
        self.tools
            .events_summary(
                event_name(&self.plugin_id, &event.name),
                event.description,
                Some(content),
                None,
                anchor,
            )
            .await
            .map(|event| event.id)
            .map_err(stringify)
    }

    async fn judgment(&self, event: NewEvent, options: Vec<EventOption>) -> Result<u64, String> {
        let anchor = self.anchor().await?;
        let content = event.content_md.unwrap_or_default();
        let options = options
            .into_iter()
            .map(|option| JudgmentOptionInput {
                key: None,
                label: option.label,
                detail: option.detail,
                recommended: option.recommended,
            })
            .collect();
        self.tools
            .pings_send_with_options(
                event_name(&self.plugin_id, &event.name),
                event.description,
                content,
                anchor,
                true,
                options,
                None,
                None,
            )
            .await
            .map(|event| event.id)
            .map_err(stringify)
    }

    async fn resolve(&self, event_id: u64) -> Result<(), String> {
        self.tools
            .pings_resolve(event_id)
            .await
            .map(|_| ())
            .map_err(stringify)
    }
}

/// Per-plugin KV over the `plugin_kv` table. The plugin id is supplied by the
/// host, never by the plugin, so one plugin cannot read another's namespace.
pub(super) struct HostKv {
    pub(super) plugin_id: String,
    pub(super) storage: Storage,
}

#[async_trait]
impl PluginKv for HostKv {
    async fn get(&self, key: &str) -> Result<Option<Value>, String> {
        self.storage
            .plugin_kv_get(&self.plugin_id, key)
            .await
            .map_err(stringify)
    }

    async fn set(&self, key: &str, value: Value) -> Result<(), String> {
        self.storage
            .plugin_kv_set(&self.plugin_id, key, &value)
            .await
            .map_err(stringify)
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        self.storage
            .plugin_kv_delete(&self.plugin_id, key)
            .await
            .map_err(stringify)
    }

    async fn entries(&self) -> Result<Vec<(String, Value)>, String> {
        self.storage
            .plugin_kv_entries(&self.plugin_id)
            .await
            .map_err(stringify)
    }
}

/// Settings are served from a `watch` channel the management API writes to, so
/// a running daemon observes a save without polling storage.
pub(super) struct HostSettings {
    pub(super) values: watch::Receiver<SettingsSnapshot>,
}

impl PluginSettingsAccess for HostSettings {
    fn values(&self) -> SettingsSnapshot {
        self.values.borrow().clone()
    }

    fn watch(&self) -> watch::Receiver<SettingsSnapshot> {
        self.values.clone()
    }
}

/// `ctx.push` fans out as a `plugin_push` frame on the same broadcast channel
/// every other host→client frame uses.
pub(super) struct HostPush {
    pub(super) plugin_id: String,
    pub(super) broadcaster: tokio::sync::broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
}

impl PluginPush for HostPush {
    fn push(&self, topic: &str, data: Value) {
        let frame = HostToClient::PluginPush {
            plugin: self.plugin_id.clone(),
            topic: topic.to_string(),
            data,
        };
        self.broadcast_log.record(frame.clone());
        let _ = self.broadcaster.send(frame);
    }
}

/// Effective settings: declared defaults with stored values layered on top.
pub(super) fn effective_settings(
    descriptors: &[hirsel_plugin_api::SettingDescriptor],
    stored: &Map<String, Value>,
) -> SettingsSnapshot {
    let mut values = Map::new();
    for descriptor in descriptors {
        if let Some(default) = &descriptor.default {
            values.insert(descriptor.key.clone(), default.clone());
        }
    }
    for (key, value) in stored {
        values.insert(key.clone(), value.clone());
    }
    Arc::new(values)
}

fn event_name(plugin_id: &str, name: &str) -> String {
    let combined = format!("{plugin_id}-{name}");
    // `create_event` caps handles at 32 characters.
    combined.chars().take(32).collect()
}

fn stringify(error: anyhow::Error) -> String {
    error.to_string()
}

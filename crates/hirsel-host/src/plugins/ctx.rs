//! Host implementations of the `PluginCtx` capability traits.

use std::sync::Arc;

use async_trait::async_trait;
use hirsel_plugin_api::{
    ActivityReceipt, NewActivity, NewThread, PluginKv, PluginPush, PluginSettingsAccess,
    PluginThreads, SettingsSnapshot,
};
use hirsel_proto::{HostToClient, ThreadAttention};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::{BroadcastLog, storage::Storage, tools::ToolSuite};

/// Plugins create durable work or append activity through distinct capabilities.
pub(super) struct HostThreads {
    pub(super) plugin_id: String,
    pub(super) label: String,
    pub(super) tools: ToolSuite,
    pub(super) storage: Storage,
}

#[async_trait]
impl PluginThreads for HostThreads {
    async fn create(&self, input: NewThread) -> Result<u64, String> {
        let (thread, _) = self
            .storage
            .create_thread(
                &format!("plugin:{}:{}", self.plugin_id, uuid::Uuid::new_v4()),
                &input.title,
                &input.description,
                &input.instrument,
                if input.needs_owner {
                    ThreadAttention::NeedsOwner
                } else {
                    ThreadAttention::Quiet
                },
            )
            .await
            .map_err(stringify)?;
        self.tools.publish_thread(thread.clone());
        self.append_activity(
            NewActivity::new("created", json!({"title": input.title})).in_thread(thread.id),
        )
        .await?;
        Ok(thread.id)
    }

    async fn append_activity(&self, input: NewActivity) -> Result<ActivityReceipt, String> {
        if input.kind.trim().is_empty() {
            return Err("activity kind must not be empty".into());
        }
        let activity = self
            .storage
            .append_thread_activity(
                input.thread_id,
                None,
                &format!("plugin.{}", input.kind),
                &json!({"plugin": self.plugin_id, "label": self.label, "payload": input.data}),
            )
            .await
            .map_err(stringify)?;
        let receipt = ActivityReceipt {
            activity_id: activity.id,
            thread_id: activity.thread_id,
        };
        self.tools.publish_thread_activity(activity).await;
        Ok(receipt)
    }

    async fn settle(&self, thread_id: u64, settled: bool) -> Result<(), String> {
        let thread = self
            .storage
            .settle_thread(thread_id, settled)
            .await
            .map_err(stringify)?;
        self.tools.publish_thread(thread);
        Ok(())
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

fn stringify(error: anyhow::Error) -> String {
    error.to_string()
}

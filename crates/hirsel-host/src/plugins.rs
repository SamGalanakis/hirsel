//! In-tree plugins: discovery, lifecycle, and the host side of `PluginCtx`.
//!
//! Plugins are compiled into the host (see `docs/adr/0014-in-tree-plugin-folders.md`).
//! `hirsel_plugins::all()` is generated from the folders under `plugins/`, so
//! this module never scans a directory or loads a library at runtime: it
//! validates ids, restores persisted enable state, builds one `PluginCtx` per
//! plugin, and — for each enabled plugin — registers tools, mounts routes,
//! collects skills, and starts the supervised daemon.
//!
//! With zero plugins installed every entry point here degrades to an empty
//! `Vec`: no tasks, no routes, no tool definitions, no boot cost.

mod ctx;
mod http;
mod supervisor;
mod tools;

#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use hirsel_plugin_api::{
    Plugin, PluginCtx, PluginRegistration, SettingDescriptor, SettingsSnapshot, is_valid_plugin_id,
};
use hirsel_proto::HostToClient;
use serde_json::{Map, Value};
use tokio::sync::{broadcast, watch};

use crate::{BroadcastLog, storage::Storage, tools::ToolSuite};

pub use supervisor::PluginState;
pub(crate) use supervisor::SupervisorConfig;
pub use tools::PluginToolRegistry;

pub(crate) use http::routes;

/// Environment override for the directory holding plugin folders. The
/// generated registry records each plugin's repository-relative folder; the
/// host resolves it against the repository root when it can find one, and
/// against this variable when the deployment layout differs.
const PLUGINS_DIR_ENV: &str = "HIRSEL_PLUGINS_DIR";

struct LoadedPlugin {
    plugin: Arc<dyn Plugin>,
    id: String,
    label: String,
    version: String,
    dir: PathBuf,
    descriptors: Vec<SettingDescriptor>,
    ctx: PluginCtx,
    settings_tx: watch::Sender<SettingsSnapshot>,
}

struct PluginHostInner {
    plugins: Vec<Arc<LoadedPlugin>>,
    statuses: supervisor::StatusTable,
    tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    tool_registry: PluginToolRegistry,
    storage: Storage,
    config: SupervisorConfig,
    /// Skills gathered at boot for the plugins that were enabled then. The
    /// agent prompt is assembled once per session, so a toggle takes effect on
    /// the next host start (recorded as a consequence in the ADR).
    skills_prompt: String,
}

/// Handle to every installed plugin. Cheap to clone; lives in `AppState`.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<PluginHostInner>,
}

impl PluginHost {
    /// Boot the installed plugins. Call before the agent runtime starts so
    /// enabled plugin tools are part of the first tool-surface fingerprint.
    pub(crate) async fn start(
        registrations: Vec<PluginRegistration>,
        storage: Storage,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
        config: SupervisorConfig,
    ) -> anyhow::Result<Self> {
        let enabled_flags = storage.plugin_enabled_flags().await?;
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        let mut plugins = Vec::new();
        let mut statuses = HashMap::new();

        for registration in registrations {
            let id = registration.plugin.id().to_string();
            if !is_valid_plugin_id(&id) {
                tracing::error!(
                    plugin = %id,
                    "plugin id must be lowercase kebab-case; skipping the plugin"
                );
                continue;
            }
            if seen.insert(id.clone(), ()).is_some() {
                tracing::error!(plugin = %id, "duplicate plugin id; skipping the later plugin");
                continue;
            }
            let plugin: Arc<dyn Plugin> = Arc::from(registration.plugin);
            let label = plugin.label().to_string();
            let descriptors = plugin.settings();
            let stored = storage.plugin_settings(&id).await?;
            let (settings_tx, settings_rx) =
                watch::channel(ctx::effective_settings(&descriptors, &stored));
            let plugin_ctx = PluginCtx::new(
                id.clone(),
                label.clone(),
                Arc::new(ctx::HostEvents {
                    plugin_id: id.clone(),
                    label: label.clone(),
                    tools: tools.clone(),
                    storage: storage.clone(),
                }),
                Arc::new(ctx::HostKv {
                    plugin_id: id.clone(),
                    storage: storage.clone(),
                }),
                Arc::new(ctx::HostSettings {
                    values: settings_rx,
                }),
                Arc::new(ctx::HostPush {
                    plugin_id: id.clone(),
                    broadcaster: broadcaster.clone(),
                    broadcast_log: broadcast_log.clone(),
                }),
            );
            // First sight of a plugin enables it: dropping a folder in and
            // rebuilding is the install, so it should be live afterwards.
            let enabled = enabled_flags.get(&id).copied().unwrap_or(true);
            statuses.insert(
                id.clone(),
                if enabled {
                    supervisor::PluginStatus::running()
                } else {
                    supervisor::PluginStatus::disabled()
                },
            );
            plugins.push(Arc::new(LoadedPlugin {
                plugin,
                id,
                label,
                version: registration.version.to_string(),
                dir: resolve_plugin_dir(registration.dir),
                descriptors,
                ctx: plugin_ctx,
                settings_tx,
            }));
        }

        let tool_registry = tools.plugin_tools().clone();
        let statuses: supervisor::StatusTable = Arc::new(Mutex::new(statuses));
        let mut skills = String::new();
        let mut tasks = HashMap::new();
        for loaded in &plugins {
            if !is_enabled(&statuses, &loaded.id) {
                continue;
            }
            tool_registry.register(&loaded.id, &loaded.ctx, loaded.plugin.tools());
            if let Some(section) = read_skills(loaded).await {
                skills.push_str(&section);
            }
            tasks.insert(
                loaded.id.clone(),
                supervisor::spawn(
                    Arc::clone(&loaded.plugin),
                    loaded.ctx.clone(),
                    Arc::clone(&statuses),
                    config,
                ),
            );
        }

        Ok(Self {
            inner: Arc::new(PluginHostInner {
                plugins,
                statuses,
                tasks: Mutex::new(tasks),
                tool_registry,
                storage,
                config,
                skills_prompt: skills,
            }),
        })
    }

    /// Skills contributed by the plugins that were enabled at boot, ready to
    /// append to the agent prompt. Empty when no enabled plugin ships skills.
    pub(crate) fn skills_prompt(&self) -> &str {
        &self.inner.skills_prompt
    }

    fn find(&self, id: &str) -> Option<&Arc<LoadedPlugin>> {
        self.inner.plugins.iter().find(|loaded| loaded.id == id)
    }

    fn status(&self, id: &str) -> supervisor::PluginStatus {
        self.inner
            .statuses
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(id)
            .cloned()
            .unwrap_or_else(supervisor::PluginStatus::disabled)
    }

    /// True while the plugin's routes and tools should be live. `errored`
    /// plugins stay mounted (the owner can see why) but are not running.
    pub(crate) fn is_running(&self, id: &str) -> bool {
        self.status(id).state == PluginState::Running
    }

    /// Persist a new enable flag and start or stop the plugin's tools, daemon,
    /// and route visibility to match. Returns whether anything changed.
    pub(crate) async fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<bool> {
        let loaded = self
            .find(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown plugin `{id}`"))?;
        let previous = self.status(id);
        self.inner.storage.set_plugin_enabled(id, enabled).await?;
        if enabled && previous.state == PluginState::Running {
            return Ok(false);
        }
        if !enabled && previous.state == PluginState::Disabled {
            return Ok(false);
        }
        if enabled {
            self.inner
                .tool_registry
                .register(id, &loaded.ctx, loaded.plugin.tools());
            let handle = supervisor::spawn(
                Arc::clone(&loaded.plugin),
                loaded.ctx.clone(),
                Arc::clone(&self.inner.statuses),
                self.inner.config,
            );
            self.tasks().insert(id.to_string(), handle);
            self.set_status(id, supervisor::PluginStatus::running());
        } else {
            self.inner.tool_registry.unregister(id);
            if let Some(handle) = self.tasks().remove(id) {
                handle.abort();
            }
            self.set_status(id, supervisor::PluginStatus::disabled());
        }
        Ok(true)
    }

    /// Merge `values` into the plugin's persisted settings and publish the new
    /// snapshot on its watch channel.
    pub(crate) async fn update_settings(
        &self,
        id: &str,
        values: &Map<String, Value>,
    ) -> anyhow::Result<SettingsSnapshot> {
        let loaded = self
            .find(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown plugin `{id}`"))?;
        let stored = self.inner.storage.merge_plugin_settings(id, values).await?;
        let snapshot = ctx::effective_settings(&loaded.descriptors, &stored);
        // A send error only means no daemon is listening.
        let _ = loaded.settings_tx.send(snapshot.clone());
        Ok(snapshot)
    }

    /// Catalog names of the tools currently contributed by enabled plugins.
    pub(crate) fn tool_names(&self) -> Vec<String> {
        self.inner.tool_registry.names()
    }

    fn set_status(&self, id: &str, status: supervisor::PluginStatus) {
        self.inner
            .statuses
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id.to_string(), status);
    }

    fn tasks(&self) -> std::sync::MutexGuard<'_, HashMap<String, tokio::task::JoinHandle<()>>> {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn is_enabled(statuses: &supervisor::StatusTable, id: &str) -> bool {
    statuses
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(id)
        .is_some_and(|status| status.state == PluginState::Running)
}

/// Resolve the repository-relative folder recorded by the generator.
///
/// `HIRSEL_PLUGINS_DIR` wins when set (a deployment whose layout differs).
/// Otherwise the folder is looked up relative to the working directory and
/// then each of its ancestors, which covers both the host running from the
/// repository root and a test running from a crate directory. Only skills read
/// from this path, so an unresolved folder degrades to "no skills", never to a
/// boot failure.
fn resolve_plugin_dir(dir: &str) -> PathBuf {
    if let Some(root) = std::env::var_os(PLUGINS_DIR_ENV) {
        let name = std::path::Path::new(dir)
            .file_name()
            .map_or_else(|| PathBuf::from(dir), PathBuf::from);
        return PathBuf::from(root).join(name);
    }
    let Ok(cwd) = std::env::current_dir() else {
        return PathBuf::from(dir);
    };
    for base in std::iter::successors(Some(cwd.as_path()), |path| path.parent()) {
        let candidate = base.join(dir);
        if candidate.is_dir() {
            return candidate;
        }
    }
    PathBuf::from(dir)
}

/// Read one plugin's `.md` skills into a delimited prompt section.
async fn read_skills(loaded: &LoadedPlugin) -> Option<String> {
    let skills_dir = loaded.dir.join(loaded.plugin.skills_dir()?);
    let mut entries = match tokio::fs::read_dir(&skills_dir).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                plugin = %loaded.id,
                dir = %skills_dir.display(),
                %error,
                "plugin skills directory could not be read; skipping its skills"
            );
            return None;
        }
    };
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    let mut section = String::new();
    for path in files {
        match tokio::fs::read_to_string(&path).await {
            Ok(body) => {
                section.push_str(&format!(
                    "\n\n## Plugin skill: {} / {}\n\n",
                    loaded.label,
                    path.file_stem().unwrap_or_default().to_string_lossy()
                ));
                section.push_str(body.trim());
            }
            Err(error) => tracing::warn!(
                plugin = %loaded.id,
                path = %path.display(),
                %error,
                "plugin skill file could not be read"
            ),
        }
    }
    (!section.is_empty()).then_some(section)
}

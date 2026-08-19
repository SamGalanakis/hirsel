//! The plugin half of the agent tool catalog.
//!
//! Plugin tools are ordinary lash `ToolDefinition`s: they go through the same
//! `HirselToolProvider` → `HirselToolExecutor` path as the built-ins, so they
//! participate in the tool-surface fingerprint, the recorded-attempt
//! machinery, and the RLM call surface without a second mechanism. The only
//! thing that is plugin-specific is the namespace and the dispatch table held
//! here.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use hirsel_plugin_api::{PluginCtx, PluginTool, PluginToolHandler, is_valid_tool_name};
use lash::tools::{LashlangToolBinding, ToolDefinition, ToolDefinitionLashlangExt};
use serde_json::{Value, json};

/// A plugin tool body that outlives this is a bug in the plugin, not a slow
/// call: the agent is blocked for the whole duration.
pub(crate) const PLUGIN_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

/// Lashlang module the plugin tool surface hangs off: `plugins.<id>.<tool>`.
const PLUGIN_MODULE_ROOT: &str = "plugins";

/// The catalog name the agent sees for one plugin tool.
pub(crate) fn catalog_name(plugin_id: &str, tool_name: &str) -> String {
    format!("plugin__{}__{tool_name}", plugin_id.replace('-', "_"))
}

struct RegisteredTool {
    plugin_id: String,
    catalog_name: String,
    module_segment: String,
    operation: String,
    description: String,
    input_schema: Value,
    ctx: PluginCtx,
    handler: Arc<dyn PluginToolHandler>,
}

/// The live plugin tool table. Cheap to clone; `ToolSuite` holds one and the
/// plugin host mutates it as plugins are enabled and disabled.
#[derive(Clone, Default)]
pub struct PluginToolRegistry {
    tools: Arc<RwLock<HashMap<String, Arc<RegisteredTool>>>>,
}

impl PluginToolRegistry {
    /// Replace this plugin's tools with `tools`. A tool whose name is not a
    /// lowercase identifier cannot be bound into the lashlang surface, so it is
    /// logged and skipped rather than failing the whole plugin.
    pub(crate) fn register(&self, plugin_id: &str, ctx: &PluginCtx, tools: Vec<PluginTool>) {
        let mut registered = Vec::new();
        for tool in tools {
            if !is_valid_tool_name(&tool.name) {
                tracing::error!(
                    plugin = %plugin_id,
                    tool = %tool.name,
                    "plugin tool name must be a lowercase identifier; skipping the tool"
                );
                continue;
            }
            registered.push(Arc::new(RegisteredTool {
                plugin_id: plugin_id.to_string(),
                catalog_name: catalog_name(plugin_id, &tool.name),
                module_segment: plugin_id.replace('-', "_"),
                operation: tool.name.clone(),
                description: tool.description,
                input_schema: tool.input_schema,
                ctx: ctx.clone(),
                handler: Arc::clone(&tool.handler),
            }));
        }
        let mut table = self.write();
        table.retain(|_, entry| entry.plugin_id != plugin_id);
        for tool in registered {
            table.insert(tool.catalog_name.clone(), tool);
        }
    }

    /// Drop every tool belonging to `plugin_id` (a disable, or a plugin that
    /// failed validation).
    pub(crate) fn unregister(&self, plugin_id: &str) {
        self.write().retain(|_, entry| entry.plugin_id != plugin_id);
    }

    /// The definitions to append to the built-in catalog, in a stable order so
    /// the tool-surface fingerprint does not churn between boots.
    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = self.snapshot();
        tools.sort_by(|left, right| left.catalog_name.cmp(&right.catalog_name));
        tools
            .into_iter()
            .map(|tool| {
                ToolDefinition::raw(
                    format!("hirsel.{}", tool.catalog_name),
                    tool.catalog_name.clone(),
                    tool.description.clone(),
                    tool.input_schema.clone(),
                    // Plugin tool results are free-form JSON objects; the host
                    // does not model them further.
                    json!({ "type": "object", "additionalProperties": true }),
                )
                .with_lashlang_binding(LashlangToolBinding::new(
                    [PLUGIN_MODULE_ROOT.to_string(), tool.module_segment.clone()],
                    tool.operation.clone(),
                ))
            })
            .collect()
    }

    /// Catalog names of every registered plugin tool, sorted. Used as the
    /// material for the catalog-refresh key when a plugin is toggled.
    pub(crate) fn names(&self) -> Vec<String> {
        let mut names = self
            .snapshot()
            .into_iter()
            .map(|tool| tool.catalog_name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// Dispatch `name`. `None` means "not a plugin tool" — the executor then
    /// reports the unknown-tool error it always did.
    pub(crate) async fn call(&self, name: &str, args: Value) -> Option<Result<Value, String>> {
        let tool = self.read().get(name).cloned()?;
        let handler = Arc::clone(&tool.handler);
        let ctx = tool.ctx.clone();
        let call = handler.call(ctx, args);
        Some(
            match tokio::time::timeout(PLUGIN_TOOL_TIMEOUT, call).await {
                Ok(result) => result,
                Err(_) => Err(format!(
                    "plugin tool `{name}` exceeded the {}s host timeout",
                    PLUGIN_TOOL_TIMEOUT.as_secs()
                )),
            },
        )
    }

    fn snapshot(&self) -> Vec<Arc<RegisteredTool>> {
        self.read().values().cloned().collect()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<RegisteredTool>>> {
        self.tools
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<RegisteredTool>>> {
        self.tools
            .write()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

//! Fixed coding-tool profile for in-process Lash workers.
//!
//! This provider is intentionally the complete worker tool catalog. It must
//! not be combined with Lash's standard tool stack, which exposes additional
//! shell and process-control tools.

mod file_tools;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use lash::tools::{
    ToolBinding, ToolCall, ToolContract, ToolDefinition, ToolDefinitionBindingExt, ToolManifest,
    ToolOutcome, ToolProvider,
};
use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use self::file_tools::execute_file_tool;

const READ: &str = "read";
const EDIT: &str = "edit";
const WRITE: &str = "write";
const EXEC_COMMAND: &str = "exec_command";

/// The complete, four-tool profile for a native coding worker.
pub(crate) struct NativeCodingTools {
    cwd: Arc<PathBuf>,
    shell: Arc<dyn ToolProvider>,
    mutations: Arc<Mutex<()>>,
    lifecycle: Arc<Lifecycle>,
}

impl NativeCodingTools {
    /// Creates a tool profile rooted at an already accepted worker cwd.
    ///
    /// The cwd is execution context, not a filesystem sandbox. Absolute file
    /// paths retain their ordinary meaning.
    pub(crate) fn new(cwd: PathBuf) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(&cwd)
            .with_context(|| format!("failed to inspect worker cwd `{}`", cwd.display()))?;
        ensure!(
            metadata.is_dir(),
            "worker cwd is not a directory: `{}`",
            cwd.display()
        );

        let shell = lash_tools::shell::shell_provider(
            lash_tools::shell::StandardShell::new().with_cwd(cwd.clone()),
        );
        Ok(Self {
            cwd: Arc::new(cwd),
            shell: Arc::new(shell),
            mutations: Arc::new(Mutex::new(())),
            lifecycle: Arc::new(Lifecycle::accepting()),
        })
    }

    /// Freezes admission, cancels active shell calls, and waits for every
    /// admitted tool body to finish. Repeated calls are safe.
    pub(crate) async fn shutdown(&self) {
        self.lifecycle.accepting.store(false, Ordering::Release);
        let cancellations = {
            let active = self
                .lifecycle
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            active.values().filter_map(Clone::clone).collect::<Vec<_>>()
        };
        for cancellation in cancellations {
            cancellation.cancel();
        }

        loop {
            let idle = self.lifecycle.idle.notified();
            tokio::pin!(idle);
            // Register before inspecting the map so a transition to empty
            // cannot fall between the check and waiter registration.
            idle.as_mut().enable();
            if self
                .lifecycle
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            {
                return;
            }
            idle.await;
        }
    }

    fn admit(&self, cancellation: Option<CancellationToken>) -> Option<ActiveCall> {
        if !self.lifecycle.accepting.load(Ordering::Acquire) {
            return None;
        }
        let mut active = self
            .lifecycle
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.lifecycle.accepting.load(Ordering::Acquire) {
            return None;
        }
        let id = self.lifecycle.next_id.fetch_add(1, Ordering::Relaxed);
        active.insert(id, cancellation);
        Some(ActiveCall {
            id,
            lifecycle: Arc::clone(&self.lifecycle),
        })
    }
}

#[derive(Default)]
struct Lifecycle {
    accepting: AtomicBool,
    next_id: AtomicU64,
    active: StdMutex<HashMap<u64, Option<CancellationToken>>>,
    idle: Notify,
}

impl Lifecycle {
    fn accepting() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            ..Self::default()
        }
    }
}

struct ActiveCall {
    id: u64,
    lifecycle: Arc<Lifecycle>,
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.lifecycle
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        self.lifecycle.idle.notify_waiters();
    }
}

#[async_trait]
impl ToolProvider for NativeCodingTools {
    fn tool_manifests(&self) -> Vec<ToolManifest> {
        definitions()
            .into_iter()
            .map(|tool| tool.manifest())
            .collect()
    }

    fn resolve_contract(&self, name: &str) -> Option<Arc<ToolContract>> {
        definitions()
            .into_iter()
            .find(|tool| tool.name() == name)
            .map(|tool| Arc::new(tool.contract()))
    }

    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        let cancellation = call.context.cancellation_token().cloned();
        if call.name == EXEC_COMMAND && cancellation.is_none() {
            return ToolOutcome::err_fmt(
                "exec_command requires an attempt cancellation scope; command was not started",
            );
        }
        let Some(_active) = self.admit(cancellation) else {
            return ToolOutcome::cancelled("native coding tools are shut down");
        };

        match call.name {
            READ | EDIT | WRITE => {
                execute_file_tool(
                    call.name,
                    call.args,
                    Arc::clone(&self.cwd),
                    Arc::clone(&self.mutations),
                )
                .await
            }
            EXEC_COMMAND => self.shell.execute(call).await,
            name => ToolOutcome::err_fmt(format!("unknown native coding tool `{name}`")),
        }
    }
}

fn definitions() -> Vec<ToolDefinition> {
    vec![
        read_definition(),
        edit_definition(),
        write_definition(),
        exec_definition(),
    ]
}

fn read_definition() -> ToolDefinition {
    ToolDefinition::raw(
        "hirsel:native-coding:read:v1",
        READ,
        "Read a UTF-8 text file by a bounded 1-based line window, or return a supported image as an inline attachment. Files are limited to 10 MiB; text output to 2,000 lines and 50 KiB. Results report truncation and the next line offset explicitly.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "offset": { "type": "integer", "minimum": 1, "default": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 2000, "default": 2000 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        json!({}),
    )
}

fn edit_definition() -> ToolDefinition {
    ToolDefinition::raw(
        "hirsel:native-coding:edit:v1",
        EDIT,
        "Replace one non-empty exact UTF-8 string in a file up to 10 MiB. The old text must occur exactly once; missing or ambiguous matches fail without changing the file.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "old_text": { "type": "string", "minLength": 1 },
                "new_text": { "type": "string" }
            },
            "required": ["path", "old_text", "new_text"],
            "additionalProperties": false
        }),
        json!({ "type": "object" }),
    )
}

fn write_definition() -> ToolDefinition {
    ToolDefinition::raw(
        "hirsel:native-coding:write:v1",
        WRITE,
        "Atomically create or replace a UTF-8 file up to 10 MiB, creating missing parent directories. Readers see either the prior complete file or the new complete file.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "content": { "type": "string" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
        json!({ "type": "object" }),
    )
}

fn exec_definition() -> ToolDefinition {
    ToolDefinition::raw(
        "hirsel:native-coding:exec-command:v1",
        EXEC_COMMAND,
        "Run one noninteractive command in the accepted worker cwd and wait for completion. Nonzero exits are ordinary result data. Timeout and cancellation kill owned children. Large output is truncated with a readable full-output path.",
        json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "minLength": 1 },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000, "default": 600000 },
                "max_output_tokens": { "type": "integer", "minimum": 1 }
            },
            "required": ["cmd"],
            "additionalProperties": false
        }),
        json!({ "type": "object" }),
    )
    .with_tool_binding(ToolBinding::new(["shell"], "exec"))
}

#[cfg(test)]
mod tests;

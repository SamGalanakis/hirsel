//! Fixed coding-tool profile for in-process Lash workers.
//!
//! This provider is intentionally the complete worker tool catalog. It must
//! not be combined with Lash's standard tool stack, which exposes additional
//! shell and process-control tools.

mod file_tools;

use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use lash::tools::{
    ToolBinding, ToolCall, ToolContract, ToolDefinition, ToolDefinitionBindingExt, ToolManifest,
    ToolOutcome, ToolProvider,
};
use lash_core::{ToolCallOutcome, ToolValue};
use serde_json::{Value, json};
use tempfile::TempPath;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

use self::file_tools::execute_file_tool;

const READ: &str = "read";
const EDIT: &str = "edit";
const WRITE: &str = "write";
const EXEC_COMMAND: &str = "exec_command";
const PROFILE_SHELL: &str = "/bin/sh";

/// The complete, four-tool profile for a native coding worker.
pub(crate) struct NativeCodingTools {
    cwd: Arc<PathBuf>,
    shell: lash_tools::shell::StandardShell,
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

        let shell = lash_tools::shell::StandardShell::new().with_cwd(cwd.clone());
        Ok(Self {
            cwd: Arc::new(cwd),
            shell,
            mutations: Arc::new(Mutex::new(())),
            lifecycle: Arc::new(Lifecycle::new()),
        })
    }

    /// Freezes admission, cancels active shell calls, and waits for every
    /// admitted tool body to finish. Repeated calls are safe.
    pub(crate) async fn shutdown(&self) {
        self.lifecycle.shutdown().await;
    }

    async fn execute_file(&self, name: &str, args: &Value) -> ToolOutcome {
        let name = name.to_string();
        let args = args.clone();
        let cwd = Arc::clone(&self.cwd);
        let mutations = Arc::clone(&self.mutations);
        let work = async move { execute_file_tool(&name, &args, cwd, mutations).await };
        self.lifecycle.run_owned(None, work).await
    }

    async fn execute_shell(
        &self,
        args: &Value,
        attempt_cancellation: CancellationToken,
    ) -> ToolOutcome {
        let Some(command) = args.get("cmd").and_then(Value::as_str) else {
            return ToolOutcome::err_fmt("missing required string field `cmd`");
        };
        let status_owner = match tempfile::NamedTempFile::new() {
            Ok(file) => file.into_temp_path(),
            Err(error) => {
                return ToolOutcome::err_fmt(format!(
                    "failed to create shell exit status control file: {error}"
                ));
            }
        };
        let status_path = status_owner.to_path_buf();
        let mut owned_args = args.clone();
        let Some(arguments) = owned_args.as_object_mut() else {
            return ToolOutcome::err_fmt("exec_command arguments must be an object");
        };
        arguments.insert(
            "cmd".to_string(),
            Value::String(wrap_one_shot_command(command, &status_path)),
        );
        arguments.insert(
            "shell".to_string(),
            Value::String(PROFILE_SHELL.to_string()),
        );
        arguments.insert("login".to_string(), Value::Bool(false));

        let shutdown_cancellation = CancellationToken::new();
        let retained_cancellation = shutdown_cancellation.clone();
        let shell = self.shell.clone();
        let work = async move {
            execute_owned_shell(
                shell,
                owned_args,
                attempt_cancellation,
                shutdown_cancellation,
                status_path,
                status_owner,
            )
            .await
        };
        self.lifecycle
            .run_owned(Some(retained_cancellation), work)
            .await
    }
}

struct Lifecycle {
    state: StdMutex<LifecycleState>,
    shutdown: Mutex<()>,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            state: StdMutex::new(LifecycleState {
                accepting: true,
                cancellations: Vec::new(),
                tasks: Vec::new(),
            }),
            shutdown: Mutex::new(()),
        }
    }

    async fn run_owned<F>(&self, cancellation: Option<CancellationToken>, work: F) -> ToolOutcome
    where
        F: Future<Output = ToolOutcome> + Send + 'static,
    {
        let receiver = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.accepting {
                return ToolOutcome::cancelled("native coding tools are shut down");
            }
            if let Some(cancellation) = cancellation {
                state.cancellations.push(cancellation);
            }
            let (sender, receiver) = oneshot::channel();
            state.tasks.push(tokio::spawn(async move {
                let _ = sender.send(work.await);
            }));
            receiver
        };

        match receiver.await {
            Ok(outcome) => outcome,
            Err(_) => {
                ToolOutcome::err_fmt("owned native coding tool task ended without an outcome")
            }
        }
    }

    async fn shutdown(&self) {
        let _shutdown = self.shutdown.lock().await;
        let tasks = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.accepting = false;
            for cancellation in &state.cancellations {
                cancellation.cancel();
            }
            state.cancellations.clear();
            std::mem::take(&mut state.tasks)
        };
        for task in tasks {
            if let Err(error) = task.await {
                tracing::error!(%error, "owned native coding tool task failed during shutdown");
            }
        }
    }
}

struct LifecycleState {
    accepting: bool,
    cancellations: Vec<CancellationToken>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
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
        match call.name {
            READ | EDIT | WRITE => self.execute_file(call.name, call.args).await,
            EXEC_COMMAND => {
                self.execute_shell(call.args, cancellation.expect("checked above"))
                    .await
            }
            name => ToolOutcome::err_fmt(format!("unknown native coding tool `{name}`")),
        }
    }
}

async fn execute_owned_shell(
    shell: lash_tools::shell::StandardShell,
    args: Value,
    attempt_cancellation: CancellationToken,
    shutdown_cancellation: CancellationToken,
    status_path: PathBuf,
    _status_owner: TempPath,
) -> ToolOutcome {
    let shell_call = shell.exec_command_owned(args, shutdown_cancellation.clone());
    tokio::pin!(shell_call);
    let outcome = tokio::select! {
        outcome = &mut shell_call => outcome,
        () = attempt_cancellation.cancelled() => {
            shutdown_cancellation.cancel();
            shell_call.await
        }
        () = shutdown_cancellation.cancelled() => shell_call.await,
    };
    restore_shell_exit_code(outcome, read_shell_exit_code(&status_path))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn wrap_one_shot_command(command: &str, status_path: &std::path::Path) -> String {
    format!(
        "__hirsel_native_command={command}; \
         ( eval \"$__hirsel_native_command\" ); \
         __hirsel_native_status=$?; \
         printf '%s' \"$__hirsel_native_status\" > {status}; \
         kill -KILL -$$",
        command = shell_quote(command),
        status = shell_quote(&status_path.to_string_lossy()),
    )
}

fn read_shell_exit_code(status_path: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(status_path)
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
}

fn restore_shell_exit_code(mut outcome: ToolOutcome, status: Option<i32>) -> ToolOutcome {
    let Some(status) = status else {
        return outcome;
    };
    if let ToolOutcome::Done(output) = &mut outcome
        && let ToolCallOutcome::Success(ToolValue::UntrustedJson(Value::Object(record))) =
            &mut output.outcome
    {
        record.insert("exit_code".to_string(), json!(status));
    }
    outcome
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
        "Read a UTF-8 text file by a bounded cursor, or return a supported image as an inline attachment. Files are limited to 10 MiB; text output to 2,000 lines and 50 KiB. Continue truncated results with the returned 1-based next_offset and zero-based UTF-8 next_byte_offset.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "offset": { "type": "integer", "minimum": 1, "default": 1 },
                "byte_offset": { "type": "integer", "minimum": 0, "default": 0 },
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
        "Run one noninteractive POSIX /bin/sh command in the accepted worker cwd and wait for completion. Nonzero exits are ordinary result data. Timeout and cancellation kill owned children. Large output is truncated with a readable full-output path.",
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
pub(crate) fn exec_definition_for_test() -> ToolDefinition {
    exec_definition()
}

#[cfg(test)]
mod tests;

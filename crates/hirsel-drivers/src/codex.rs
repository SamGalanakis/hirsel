//! Driver for the `codex app-server` JSON-RPC protocol.

use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Duration, timeout},
};
use uuid::Uuid;

use crate::{
    shared::{
        EventHub, drain_stderr, kill_process_group, lock, short_line, start_in_process_group,
        terminal_message, write_json_line,
    },
    types::{
        AgentKind, DriverError, DriverResult, EventStream, SessionHandle, SpawnSpec,
        SubagentDriver, SubagentEvent, TerminalOutcome,
    },
};

#[derive(Default)]
pub struct CodexDriver {
    sessions: Mutex<HashMap<String, Arc<CodexSession>>>,
}

struct CodexSession {
    events: Arc<EventHub>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    thread_id: String,
    cwd: PathBuf,
    active_turn_id: Mutex<Option<String>>,
    next_request_id: AtomicU64,
    pgid: i32,
}

impl CodexSession {
    fn kill_group(&self) {
        kill_process_group(self.pgid);
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        kill_process_group(self.pgid);
    }
}

#[async_trait]
impl SubagentDriver for CodexDriver {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle> {
        let mut command = Command::new("codex");
        command.arg("app-server").arg("--stdio");
        if let Some(model) = task.model.as_deref() {
            command.arg("-c").arg(format!("model={model}"));
        }
        if let Some(variant) = task.variant.as_deref() {
            command
                .arg("-c")
                .arg(format!("model_reasoning_effort={variant}"));
        }
        for (key, value) in codex_mcp_disable_flags() {
            command.arg("-c").arg(format!("{key}={value}"));
        }
        command
            .current_dir(&task.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        start_in_process_group(&mut command);

        let mut child = command.spawn()?;
        let pgid = child.id().map(|id| id as i32).unwrap_or_default();
        let mut stdin = child
            .stdin
            .take()
            .ok_or(DriverError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(DriverError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(DriverError::MissingPipe("stderr"))?;
        let mut lines = BufReader::new(stdout).lines();
        let events = EventHub::new(256);

        write_json_line(&mut stdin, &codex_initialize_request(1)).await?;
        write_json_line(&mut stdin, &codex_thread_start_request(2, &task.cwd)).await?;
        let thread_id = timeout(
            Duration::from_secs(30),
            read_codex_thread_id(&mut lines, events.clone()),
        )
        .await
        .map_err(|_| DriverError::MissingExternalId)??;
        let _ = events.emit(SubagentEvent::Started {
            external_id: thread_id.clone(),
        });

        let session = Arc::new(CodexSession {
            events: events.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            thread_id: thread_id.clone(),
            cwd: task.cwd.clone(),
            active_turn_id: Mutex::new(None),
            next_request_id: AtomicU64::new(4),
            pgid,
        });
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: AgentKind::Codex,
        };
        lock(&self.sessions)?.insert(handle.id.clone(), session.clone());

        {
            let mut stdin = session.stdin.lock().await;
            write_json_line(
                &mut stdin,
                &codex_turn_start_request(3, &thread_id, &task.prompt, &task.cwd),
            )
            .await?;
        }
        tokio::spawn(drain_stderr(stderr));
        tokio::spawn(read_codex_stdout(lines, child, session.clone()));
        Ok(handle)
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let session = {
            let sessions = lock(&self.sessions)?;
            sessions
                .get(&handle.id)
                .cloned()
                .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?
        };
        let request_id = session.next_request_id.fetch_add(1, Ordering::SeqCst);
        let mut stdin = session.stdin.lock().await;
        write_json_line(
            &mut stdin,
            &codex_turn_start_request(request_id, &session.thread_id, &text, &session.cwd),
        )
        .await
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let session = {
            let sessions = lock(&self.sessions)?;
            sessions
                .get(&handle.id)
                .cloned()
                .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?
        };
        let turn_id = lock(&session.active_turn_id)?
            .clone()
            .ok_or(DriverError::NoActiveTurn)?;
        let request_id = session.next_request_id.fetch_add(1, Ordering::SeqCst);
        let mut stdin = session.stdin.lock().await;
        write_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "turn/interrupt",
                "params": {
                    "threadId": session.thread_id,
                    "turnId": turn_id
                }
            }),
        )
        .await
    }

    async fn retire(&self, handle: &SessionHandle) -> DriverResult<()> {
        if let Some(session) = lock(&self.sessions)?.remove(&handle.id) {
            session.kill_group();
        }
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        session.events.stream()
    }
}

fn codex_mcp_disable_flags() -> [(&'static str, &'static str); 7] {
    [
        ("mcp_servers.figments.enabled", "false"),
        ("mcp_servers.playwright.enabled", "false"),
        ("mcp_servers.runpod.enabled", "false"),
        ("mcp_servers.openaiDeveloperDocs.enabled", "false"),
        ("mcp_servers.runpod-docs.enabled", "false"),
        ("mcp_servers.wandb.enabled", "false"),
        ("mcp_servers.linear.enabled", "false"),
    ]
}

fn codex_initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "hirsel", "title": null, "version": "0" },
            "capabilities": { "experimentalApi": true, "requestAttestation": false }
        }
    })
}

fn codex_thread_start_request(id: u64, cwd: &std::path::Path) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "thread/start",
        "params": {
            "cwd": cwd,
            "runtimeWorkspaceRoots": [cwd],
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "threadSource": "hirsel",
            "config": {
                "mcp_servers": {
                    "figments": { "enabled": false },
                    "playwright": { "enabled": false },
                    "runpod": { "enabled": false },
                    "openaiDeveloperDocs": { "enabled": false },
                    "runpod-docs": { "enabled": false },
                    "wandb": { "enabled": false },
                    "linear": { "enabled": false }
                }
            }
        }
    })
}

fn codex_turn_start_request(
    id: u64,
    thread_id: &str,
    prompt: &str,
    cwd: &std::path::Path,
) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "input": [{ "type": "text", "text": prompt, "text_elements": [] }],
            "cwd": cwd,
            "runtimeWorkspaceRoots": [cwd],
            "approvalPolicy": "never",
            "sandboxPolicy": { "type": "dangerFullAccess" }
        }
    })
}

async fn read_codex_thread_id(
    lines: &mut Lines<BufReader<ChildStdout>>,
    events: Arc<EventHub>,
) -> DriverResult<String> {
    while let Some(line) = lines.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        if let Some(summary) = codex_progress(&value) {
            let _ = events.emit(SubagentEvent::Progress { summary });
        }
        if value.get("id").and_then(Value::as_u64) == Some(2)
            && let Some(thread_id) = value.pointer("/result/thread/id").and_then(Value::as_str)
        {
            return Ok(thread_id.to_string());
        }
        if value.get("method").and_then(Value::as_str) == Some("thread/started")
            && let Some(thread_id) = value.pointer("/params/thread/id").and_then(Value::as_str)
        {
            return Ok(thread_id.to_string());
        }
    }
    Err(DriverError::MissingExternalId)
}

async fn read_codex_stdout(
    mut lines: Lines<BufReader<ChildStdout>>,
    mut child: Child,
    session: Arc<CodexSession>,
) {
    let mut terminal_sent = false;
    let mut last_agent_message = None;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    if let Some(message) = codex_agent_message(&value) {
                        last_agent_message = Some(message.to_string());
                    }
                    if let Some(turn_id) = value.pointer("/result/turn/id").and_then(Value::as_str)
                        && let Ok(mut active_turn_id) = lock(&session.active_turn_id)
                    {
                        *active_turn_id = Some(turn_id.to_string());
                    }
                    if value.get("method").and_then(Value::as_str) == Some("turn/started")
                        && let Some(turn_id) =
                            value.pointer("/params/turn/id").and_then(Value::as_str)
                    {
                        terminal_sent = false;
                        last_agent_message = None;
                        if let Ok(mut active_turn_id) = lock(&session.active_turn_id) {
                            *active_turn_id = Some(turn_id.to_string());
                        }
                    }
                    if let Some(summary) = codex_progress(&value) {
                        let _ = session.events.emit(SubagentEvent::Progress { summary });
                    }
                    if let Some(outcome) =
                        codex_terminal_outcome(&value, last_agent_message.as_deref())
                    {
                        terminal_sent = true;
                        if let Ok(mut active_turn_id) = lock(&session.active_turn_id) {
                            *active_turn_id = None;
                        }
                        let _ = session.events.emit(SubagentEvent::Terminal { outcome });
                    }
                }
                Err(error) => {
                    let _ = session.events.emit(SubagentEvent::Progress {
                        summary: short_line(format!("unparsed codex output: {error}")),
                    });
                }
            },
            Ok(None) => break,
            Err(error) => {
                if !terminal_sent {
                    let _ = session.events.emit(SubagentEvent::Terminal {
                        outcome: TerminalOutcome::Failed {
                            reason: format!("codex stdout error: {error}"),
                        },
                    });
                    terminal_sent = true;
                }
                break;
            }
        }
    }
    match child.wait().await {
        Ok(status) if terminal_sent || status.success() => {}
        Ok(status) => {
            let _ = session.events.emit(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: format!("codex exited without terminal notification: {status}"),
                },
            });
        }
        Err(error) if !terminal_sent => {
            let _ = session.events.emit(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: format!("codex wait failed: {error}"),
                },
            });
        }
        Err(_) => {}
    }
}

fn codex_progress(value: &Value) -> Option<String> {
    let method = value.get("method").and_then(Value::as_str);
    if matches!(method, Some("configWarning")) {
        return value
            .pointer("/params/summary")
            .and_then(Value::as_str)
            .map(short_line);
    }
    if !matches!(method, Some("item/started") | Some("item/completed")) {
        return None;
    }
    let item = value.pointer("/params/item")?;
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("item");
    let status = item.get("status").and_then(Value::as_str).unwrap_or("");
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return Some(short_line(text));
    }
    if let Some(command) = item.get("command").and_then(Value::as_str) {
        return Some(short_line(format!("{item_type} {status}: {command}")));
    }
    Some(short_line(format!("{item_type} {status}")))
}

pub(crate) fn codex_agent_message(value: &Value) -> Option<&str> {
    if value.get("method").and_then(Value::as_str) != Some("item/completed") {
        return None;
    }
    let item = value.pointer("/params/item")?;
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("agentMessage" | "agent_message")
    ) {
        return None;
    }
    item.get("text").and_then(Value::as_str)
}

pub(crate) fn codex_terminal_outcome(
    value: &Value,
    last_agent_message: Option<&str>,
) -> Option<TerminalOutcome> {
    if value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }
    let turn = value.pointer("/params/turn")?;
    let status = turn
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    match status {
        "interrupted" => Some(TerminalOutcome::Interrupted),
        "failed" => Some(TerminalOutcome::Failed {
            reason: terminal_message(
                turn.get("error")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "codex turn failed".to_string()),
            ),
        }),
        _ => Some(TerminalOutcome::Done {
            summary: terminal_message(last_agent_message.unwrap_or("codex turn completed")),
        }),
    }
}

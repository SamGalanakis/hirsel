//! Driver for the `codex app-server` JSON-RPC protocol.

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::oneshot,
    time::{Duration, timeout},
};
use uuid::Uuid;

use crate::{
    shared::{
        EventHub, ProcessGroup, SessionRegistry, drain_stderr, lock, short_line,
        start_in_process_group, terminal_message, write_json_line,
    },
    types::{
        AgentKind, DriverError, DriverResult, EventStream, SessionHandle, SpawnSpec,
        SubagentDriver, SubagentEvent, TerminalOutcome,
    },
};

#[derive(Default)]
pub struct CodexDriver {
    sessions: SessionRegistry<CodexSession>,
}

const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

type PendingReply = oneshot::Sender<Result<Value, String>>;

#[derive(Default)]
struct CodexState {
    thread_id: Option<String>,
    active_turn_id: Option<String>,
    last_agent_message: Option<String>,
    closed: bool,
    pending: HashMap<u64, (&'static str, PendingReply)>,
}

struct CodexSession {
    events: Arc<EventHub>,
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
    state: Mutex<CodexState>,
    next_request_id: AtomicU64,
    control_timeout: Duration,
    // Only the spawn future/registry and in-flight controls own this guard.
    // The reader holds a Weak reference so cancelled startup kills the group.
    process_group: ProcessGroup,
}

struct StartupGuard<'a> {
    session: &'a CodexSession,
    committed: bool,
}

impl Drop for StartupGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.session.process_group.kill_group();
        }
    }
}

struct PendingRequest<'a> {
    session: &'a CodexSession,
    id: u64,
}

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.session.state.lock() {
            state.pending.remove(&self.id);
        }
    }
}

impl CodexSession {
    async fn request(&self, method: &'static str, mut value: Value) -> DriverResult<Value> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        value["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        {
            let mut state = lock(&self.state)?;
            if state.closed {
                return Err(DriverError::SessionClosed);
            }
            state.pending.insert(id, (method, tx));
        }
        let _pending = PendingRequest { session: self, id };
        let response = timeout(self.control_timeout, async {
            {
                let mut stdin = self.stdin.lock().await;
                write_json_line(
                    stdin.as_mut().ok_or(DriverError::MissingPipe("stdin"))?,
                    &value,
                )
                .await?;
            }
            rx.await
                .map_err(|_| protocol_error("codex response channel closed"))?
                .map_err(protocol_error)
        })
        .await;
        match response {
            Ok(Err(error @ DriverError::Io(_))) => {
                self.fail(&format!("codex {method} transport failed: {error}"));
                Err(error)
            }
            Ok(result) => result,
            Err(_) => {
                let reason = format!("codex {method} timed out");
                self.fail(&reason);
                Err(DriverError::RequestTimeout(reason))
            }
        }
    }

    async fn write_control(&self, value: &Value) -> DriverResult<()> {
        write_codex_control(&self.stdin, value, self.control_timeout).await
    }

    fn active_turn(&self) -> DriverResult<(String, String)> {
        let state = lock(&self.state)?;
        if state.closed {
            return Err(DriverError::SessionClosed);
        }
        Ok((
            state
                .thread_id
                .clone()
                .ok_or(DriverError::MissingExternalId)?,
            state
                .active_turn_id
                .clone()
                .ok_or(DriverError::NoActiveTurn)?,
        ))
    }

    fn fail(&self, reason: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            state.active_turn_id = None;
            for (_, (_, tx)) in state.pending.drain() {
                let _ = tx.send(Err(reason.to_string()));
            }
            if !self.events.is_terminal() {
                let _ = self.events.emit(SubagentEvent::Terminal {
                    outcome: TerminalOutcome::Failed {
                        reason: reason.to_string(),
                    },
                });
            }
        }
        self.process_group.kill_group();
    }

    fn receive(&self, value: Value) -> DriverResult<()> {
        let mut state = lock(&self.state)?;
        if value.get("method").is_none()
            && let Some(id) = value.get("id").and_then(Value::as_u64)
        {
            if let Some((method, tx)) = state.pending.remove(&id) {
                let response = if let Some(error) = value.get("error") {
                    Err(format!(
                        "codex {method} rejected: {}",
                        terminal_message(error.to_string())
                    ))
                } else if let Some(result) = value.get("result") {
                    // Root identity comes only from our correlated thread/start.
                    // Child thread/started notifications cannot choose the root.
                    if method == "thread/start" {
                        state.thread_id = result
                            .pointer("/thread/id")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if let Some(external_id) = &state.thread_id {
                            let _ = self.events.emit(SubagentEvent::Started {
                                external_id: external_id.clone(),
                            });
                        }
                    }
                    if method == "turn/start" && !self.events.is_terminal() {
                        state.active_turn_id = result
                            .pointer("/turn/id")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    Ok(result.clone())
                } else {
                    Err(format!("codex {method} returned no result or error"))
                };
                let _ = tx.send(response);
            }
            return Ok(());
        }
        if state.closed || self.events.is_terminal() {
            return Ok(());
        }
        if value.get("method").and_then(Value::as_str) == Some("configWarning") {
            if let Some(summary) = codex_progress(&value) {
                let _ = self.events.emit(SubagentEvent::Progress { summary });
            }
            return Ok(());
        }
        // App-server multiplexes native child threads onto the same stdout.
        // Only notifications explicitly addressed to our root affect its state.
        if state.thread_id.as_deref().is_none()
            || value.pointer("/params/threadId").and_then(Value::as_str)
                != state.thread_id.as_deref()
        {
            return Ok(());
        }
        let method = value.get("method").and_then(Value::as_str);
        if method == Some("turn/started") {
            let turn_id = value.pointer("/params/turn/id").and_then(Value::as_str);
            if state.active_turn_id.is_none() {
                state.active_turn_id = turn_id.map(str::to_string);
            }
        }
        if let Some(turn_id) = value.pointer("/params/turnId").and_then(Value::as_str)
            && state.active_turn_id.as_deref() != Some(turn_id)
        {
            return Ok(());
        }
        if let Some(message) = codex_agent_message(&value) {
            state.last_agent_message = Some(message.to_string());
        }
        if let Some(summary) = codex_progress(&value) {
            let _ = self.events.emit(SubagentEvent::Progress { summary });
        }
        if method == Some("turn/completed")
            && state.active_turn_id.is_some()
            && value.pointer("/params/turn/id").and_then(Value::as_str)
                == state.active_turn_id.as_deref()
            && let Some(outcome) =
                codex_terminal_outcome(&value, state.last_agent_message.as_deref())
        {
            state.active_turn_id = None;
            let _ = self.events.emit(SubagentEvent::Terminal { outcome });
        }
        Ok(())
    }
}

fn protocol_error(reason: impl Into<String>) -> DriverError {
    DriverError::Protocol(reason.into())
}

#[async_trait]
impl SubagentDriver for CodexDriver {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle> {
        let disabled_mcp_servers = codex_mcp_disable_names();
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
        for name in &disabled_mcp_servers {
            command
                .arg("-c")
                .arg(format!("mcp_servers.{name}.enabled=false"));
        }
        self.spawn_command(task, command, disabled_mcp_servers, CONTROL_TIMEOUT)
            .await
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        let (thread_id, turn_id) = session.active_turn()?;
        let response = session
            .request(
                "turn/steer",
                json!({
                    "jsonrpc": "2.0", "method": "turn/steer",
                    "params": { "threadId": thread_id, "expectedTurnId": turn_id,
                        "input": [{ "type": "text", "text": text, "text_elements": [] }] }
                }),
            )
            .await?;
        if response.get("turnId").and_then(Value::as_str) != Some(turn_id.as_str()) {
            let reason = "codex turn/steer acknowledged a different or missing turn id";
            session.fail(reason);
            return Err(protocol_error(reason));
        }
        Ok(())
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        let (thread_id, turn_id) = session.active_turn()?;
        session
            .request(
                "turn/interrupt",
                json!({
                    "jsonrpc": "2.0", "method": "turn/interrupt",
                    "params": { "threadId": thread_id, "turnId": turn_id }
                }),
            )
            .await?;
        Ok(())
    }

    async fn retire(&self, handle: &SessionHandle) -> DriverResult<()> {
        if let Some(session) = self.sessions.remove(handle)? {
            session.fail("codex session retired");
            drop(session.stdin.lock().await.take());
        }
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        self.sessions.get(handle)?.events.stream()
    }
}

impl CodexDriver {
    async fn spawn_command(
        &self,
        task: SpawnSpec,
        mut command: Command,
        disabled_mcp_servers: Vec<String>,
        control_timeout: Duration,
    ) -> DriverResult<SessionHandle> {
        command
            .current_dir(&task.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        start_in_process_group(&mut command);
        let mut child = command.spawn()?;
        let process_group = ProcessGroup::new(child.id().map(|id| id as i32).unwrap_or_default());
        let stdin = child
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
        let session = Arc::new(CodexSession {
            events: EventHub::new(256),
            stdin: Arc::new(tokio::sync::Mutex::new(Some(stdin))),
            state: Mutex::new(CodexState::default()),
            next_request_id: AtomicU64::new(1),
            control_timeout,
            process_group,
        });
        let mut startup_guard = StartupGuard {
            session: &session,
            committed: false,
        };
        let stderr_task = tokio::spawn(drain_stderr(stderr));
        tokio::spawn(read_codex_stdout(
            BufReader::new(stdout).lines(),
            child,
            Arc::downgrade(&session),
            stderr_task,
        ));
        session
            .request("initialize", codex_initialize_request(0))
            .await?;
        session
            .write_control(&json!({ "jsonrpc": "2.0", "method": "initialized" }))
            .await?;
        let opened = session
            .request(
                "thread/start",
                codex_thread_start_request(0, &task.cwd, &disabled_mcp_servers),
            )
            .await?;
        let thread_id = opened
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or(DriverError::MissingExternalId)?;
        let started = session
            .request(
                "turn/start",
                codex_turn_start_request(0, thread_id, &task.prompt, &task.cwd),
            )
            .await?;
        if started
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(protocol_error("codex turn/start returned no turn id"));
        }
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: AgentKind::Codex,
        };
        self.sessions.insert(handle.id.clone(), session.clone())?;
        startup_guard.committed = true;
        Ok(handle)
    }
}

fn codex_mcp_disable_names() -> Vec<String> {
    let Some(config_path) = codex_config_path() else {
        return Vec::new();
    };
    let Ok(config_text) = std::fs::read_to_string(config_path) else {
        return Vec::new();
    };
    mcp_disable_names(&config_text)
}

fn codex_config_path() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(codex_home).join("config.toml"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex/config.toml"))
}

pub(crate) fn mcp_disable_names(config_text: &str) -> Vec<String> {
    let mut names = BTreeSet::new();
    for line in config_text.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("[mcp_servers.") else {
            continue;
        };
        let Some(name) = mcp_server_name(rest) else {
            continue;
        };
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }
    names.into_iter().collect()
}

fn mcp_server_name(rest: &str) -> Option<&str> {
    let (name, tail) = match rest.as_bytes().first().copied() {
        Some(b'"') | Some(b'\'') => {
            let quote = rest.as_bytes()[0] as char;
            let end = rest[1..].find(quote)? + 1;
            (&rest[1..end], &rest[end + 1..])
        }
        Some(_) => {
            let end = rest.find(['.', ']'])?;
            (&rest[..end], &rest[end..])
        }
        None => return None,
    };
    let tail = tail.trim_start();
    if !tail.starts_with('.') && !tail.starts_with(']') {
        return None;
    }
    Some(name.trim())
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

fn codex_thread_start_request(
    id: u64,
    cwd: &std::path::Path,
    disabled_mcp_servers: &[String],
) -> Value {
    let mcp_servers = disabled_mcp_servers
        .iter()
        .map(|name| (name.clone(), json!({ "enabled": false })))
        .collect::<serde_json::Map<_, _>>();
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
                "mcp_servers": mcp_servers
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

async fn write_codex_control(
    stdin: &tokio::sync::Mutex<Option<ChildStdin>>,
    value: &Value,
    limit: Duration,
) -> DriverResult<()> {
    timeout(limit, async {
        let mut stdin = stdin.lock().await;
        write_json_line(
            stdin.as_mut().ok_or(DriverError::MissingPipe("stdin"))?,
            value,
        )
        .await
    })
    .await
    .map_err(|_| DriverError::RequestTimeout("codex control response write".to_string()))?
}

async fn read_codex_stdout(
    mut lines: Lines<BufReader<ChildStdout>>,
    mut child: Child,
    session: Weak<CodexSession>,
    stderr_task: tokio::task::JoinHandle<()>,
) {
    let mut exit = None;
    let mut drain_deadline = None;
    let mut reply_failure = None;
    let mut pending_reply: Option<futures_util::future::BoxFuture<'static, DriverResult<()>>> =
        None;
    let reason = loop {
        tokio::select! {
            status = child.wait(), if exit.is_none() => {
                let status = status.map(|status| status.to_string()).unwrap_or_else(|error| error.to_string());
                exit = Some(status);
                // A reply cannot reach an exited server. Cancel its write so
                // buffered stdout can drain even when a descendant holds stdin.
                pending_reply = None;
                if let Some(session) = session.upgrade() {
                    session.process_group.kill_group();
                }
                // Descendants can inherit stdout after the direct child exits.
                drain_deadline.get_or_insert(tokio::time::Instant::now() + EXIT_DRAIN_TIMEOUT);
            }
            line = lines.next_line(), if pending_reply.is_none() => {
                match line {
                    Ok(Some(line)) => {
                        let Some(session) = session.upgrade() else { break "codex session dropped".to_string(); };
                        let result = match serde_json::from_str::<Value>(&line) {
                            Ok(value) if value.get("method").is_some() && value.get("id").is_some() => {
                                // Native requests have their own ID namespace. Never
                                // mistake one for our response, even if IDs collide.
                                if exit.is_some() || reply_failure.is_some() {
                                    // Keep draining final output after the write side
                                    // closes; no request can be answered there now.
                                    continue;
                                }
                                let stdin = Arc::clone(&session.stdin);
                                let limit = session.control_timeout;
                                let response = json!({
                                    "jsonrpc": "2.0", "id": value["id"],
                                    "error": { "code": -32601, "message": "Hirsel does not support this server request" }
                                });
                                pending_reply = Some(Box::pin(async move {
                                    write_codex_control(&stdin, &response, limit).await
                                }));
                                let _ = session.events.emit(SubagentEvent::Progress {
                                    summary: short_line(format!("unsupported codex server request: {}", value["method"].as_str().unwrap_or("unknown"))),
                                });
                                Ok(())
                            }
                            Ok(value) => session.receive(value),
                            Err(error) => Err(error.into()),
                        };
                        if let Err(error) = result { break format!("codex protocol error: {error}"); }
                    }
                    Ok(None) => break "codex stdout ended without terminal notification".to_string(),
                    Err(error) => break format!("codex stdout error: {error}"),
                }
            }
            result = async {
                match pending_reply.as_mut() {
                    Some(reply) => reply.await,
                    None => std::future::pending().await,
                }
            } => {
                pending_reply = None;
                if let Err(error) = result {
                    // A broken stdin can precede final buffered stdout. Give it
                    // the same bounded drain before synthesizing failure.
                    reply_failure = Some(format!("codex server response failed: {error}"));
                    drain_deadline.get_or_insert(tokio::time::Instant::now() + EXIT_DRAIN_TIMEOUT);
                }
            }
            () = async {
                match drain_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => break format!("codex exited without terminal notification: {}", exit.as_deref().unwrap_or("unknown status")),
        }
    };
    drop(pending_reply);
    let reason = reply_failure.unwrap_or(reason);
    if let Some(session) = session.upgrade() {
        session.fail(&reason);
    }
    if exit.is_none() {
        let _ = child.start_kill();
        let _ = timeout(EXIT_DRAIN_TIMEOUT, child.wait()).await;
    }
    stderr_task.abort();
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

#[cfg(test)]
mod tests {
    use super::mcp_disable_names;

    #[test]
    fn mcp_discovery_finds_server_headers_and_quoted_names() {
        let config = r#"
[mcp_servers.figments]
command = "bun"

[mcp_servers."openaiDeveloperDocs"]
url = "https://example.invalid"

[mcp_servers.'runpod-docs']
url = "https://example.invalid"
"#;

        assert_eq!(
            mcp_disable_names(config),
            vec![
                "figments".to_string(),
                "openaiDeveloperDocs".to_string(),
                "runpod-docs".to_string(),
            ]
        );
    }

    #[test]
    fn mcp_discovery_uses_first_path_segment_and_skips_subtables() {
        let config = r#"
[mcp_servers.linear]
command = "linear"

[mcp_servers.linear.env]
TOKEN = "redacted"

[mcp_servers.linear.tools.search]
enabled = true

[mcp_servers.linear.http_headers]
Authorization = "redacted"

[other_servers.linear]
enabled = true
"#;

        assert_eq!(mcp_disable_names(config), vec!["linear".to_string()]);
    }

    #[test]
    fn mcp_discovery_returns_empty_for_empty_or_unrelated_config() {
        assert!(mcp_disable_names("").is_empty());
        assert!(mcp_disable_names("# no MCP servers\n[provider]\nname = \"codex\"\n").is_empty());
    }
}

#[cfg(test)]
#[path = "codex_native_tests.rs"]
mod native_tests;

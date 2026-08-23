//! Driver for the `codex app-server` JSON-RPC protocol.

use std::{
    collections::BTreeSet,
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
        EventHub, ProcessGroup, SessionRegistry, drain_stderr, finish_child, lock, short_line,
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

struct CodexSession {
    events: Arc<EventHub>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    thread_id: String,
    cwd: PathBuf,
    active_turn_id: Mutex<Option<String>>,
    next_request_id: AtomicU64,
    // Field order keeps implicit teardown aligned with retire: close stdin, then kill the group.
    process_group: ProcessGroup,
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
        write_json_line(
            &mut stdin,
            &codex_thread_start_request(2, &task.cwd, &disabled_mcp_servers),
        )
        .await?;
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
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            thread_id: thread_id.clone(),
            cwd: task.cwd.clone(),
            active_turn_id: Mutex::new(None),
            next_request_id: AtomicU64::new(4),
            process_group: ProcessGroup::new(pgid),
        });
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: AgentKind::Codex,
        };
        self.sessions.insert(handle.id.clone(), session.clone())?;

        {
            let mut stdin = session.stdin.lock().await;
            write_json_line(
                stdin.as_mut().ok_or(DriverError::MissingPipe("stdin"))?,
                &codex_turn_start_request(3, &thread_id, &task.prompt, &task.cwd),
            )
            .await?;
        }
        tokio::spawn(drain_stderr(stderr));
        tokio::spawn(read_codex_stdout(lines, child, session.clone()));
        Ok(handle)
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        let request_id = session.next_request_id.fetch_add(1, Ordering::SeqCst);
        let mut stdin = session.stdin.lock().await;
        write_json_line(
            stdin.as_mut().ok_or(DriverError::MissingPipe("stdin"))?,
            &codex_turn_start_request(request_id, &session.thread_id, &text, &session.cwd),
        )
        .await
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        let turn_id = lock(&session.active_turn_id)?
            .clone()
            .ok_or(DriverError::NoActiveTurn)?;
        let request_id = session.next_request_id.fetch_add(1, Ordering::SeqCst);
        let mut stdin = session.stdin.lock().await;
        write_json_line(
            stdin.as_mut().ok_or(DriverError::MissingPipe("stdin"))?,
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
        if let Some(session) = self.sessions.remove(handle)? {
            drop(session.stdin.lock().await.take());
            session.process_group.kill_group();
        }
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let session = self.sessions.get(handle)?;
        session.events.stream()
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
    child: Child,
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
                        // Deliberate protocol difference: Codex reuses one process for multiple
                        // turns, so each turn can emit its own terminal event. Claude never resets.
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
    finish_child(
        child,
        terminal_sent,
        &session.events,
        "codex",
        "terminal notification",
    )
    .await;
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

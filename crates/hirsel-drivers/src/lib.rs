use std::{
    collections::HashMap,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use async_stream::stream;
use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::broadcast,
    time::{Duration, sleep, timeout},
};
use uuid::Uuid;

pub type EventStream = Pin<Box<dyn Stream<Item = SubagentEvent> + Send>>;
pub type DriverResult<T> = Result<T, DriverError>;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("sub-agent session not found: {0}")]
    SessionNotFound(String),
    #[error("sub-agent session has no active turn")]
    NoActiveTurn,
    #[error("driver state lock was poisoned")]
    StatePoisoned,
    #[error("missing child pipe: {0}")]
    MissingPipe(&'static str),
    #[error("CLI did not return an external id before timeout")]
    MissingExternalId,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub agent: AgentKind,
    pub prompt: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub fake_fixture: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionHandle {
    pub id: String,
    pub agent: AgentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentEvent {
    Started { external_id: String },
    Progress { summary: String },
    Terminal { outcome: TerminalOutcome },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TerminalOutcome {
    Done { summary: String },
    Failed { reason: String },
    Interrupted,
}

#[async_trait]
pub trait SubagentDriver: Send + Sync {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle>;
    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()>;
    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()>;
    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream>;
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> DriverResult<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| DriverError::StatePoisoned)
}

fn receiver_stream(mut rx: broadcast::Receiver<SubagentEvent>) -> EventStream {
    Box::pin(stream! {
        loop {
            match rx.recv().await {
                Ok(event) => yield event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn short_line(text: impl AsRef<str>) -> String {
    let compact = text
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const MAX_CHARS: usize = 240;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

async fn write_json_line(stdin: &mut ChildStdin, value: &Value) -> DriverResult<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await?;
    Ok(())
}

fn start_in_process_group(command: &mut Command) {
    // A Sub-agent Driver owns the whole CLI process tree; setsid lets hard cleanup target the group.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn kill_process_group(pgid: i32) {
    if pgid > 0 {
        // Best-effort cleanup for externally spawned CLIs.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[derive(Default)]
pub struct FakeDriver {
    sessions: Mutex<HashMap<String, Arc<FakeSession>>>,
}

struct FakeSession {
    tx: broadcast::Sender<SubagentEvent>,
    interrupted: AtomicBool,
    terminal_sent: AtomicBool,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeFixture {
    #[serde(default = "default_fake_external_id")]
    external_id: String,
    #[serde(default = "default_fake_progress")]
    progress: Vec<String>,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default = "default_fake_terminal")]
    terminal: TerminalOutcome,
}

fn default_fake_external_id() -> String {
    "fake-external-session".to_string()
}

fn default_fake_progress() -> Vec<String> {
    vec![
        "fake driver started".to_string(),
        "fake driver working".to_string(),
    ]
}

fn default_fake_terminal() -> TerminalOutcome {
    TerminalOutcome::Done {
        summary: "fake driver completed".to_string(),
    }
}

impl Default for FakeFixture {
    fn default() -> Self {
        Self {
            external_id: default_fake_external_id(),
            progress: default_fake_progress(),
            delay_ms: 10,
            terminal: default_fake_terminal(),
        }
    }
}

#[async_trait]
impl SubagentDriver for FakeDriver {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle> {
        let fixture = match task.fake_fixture {
            Some(path) => serde_json::from_str(&tokio::fs::read_to_string(path).await?)?,
            None => FakeFixture::default(),
        };
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: task.agent,
        };
        let (tx, _) = broadcast::channel(128);
        let session = Arc::new(FakeSession {
            tx: tx.clone(),
            interrupted: AtomicBool::new(false),
            terminal_sent: AtomicBool::new(false),
        });
        lock(&self.sessions)?.insert(handle.id.clone(), session.clone());

        tokio::spawn(async move {
            let _ = tx.send(SubagentEvent::Started {
                external_id: fixture.external_id,
            });
            for progress in fixture.progress {
                if fixture.delay_ms > 0 {
                    sleep(Duration::from_millis(fixture.delay_ms)).await;
                }
                if session.interrupted.load(Ordering::SeqCst) {
                    if !session.terminal_sent.swap(true, Ordering::SeqCst) {
                        let _ = tx.send(SubagentEvent::Terminal {
                            outcome: TerminalOutcome::Interrupted,
                        });
                    }
                    return;
                }
                let _ = tx.send(SubagentEvent::Progress {
                    summary: short_line(progress),
                });
            }
            if fixture.delay_ms > 0 {
                sleep(Duration::from_millis(fixture.delay_ms)).await;
            }
            if session.interrupted.load(Ordering::SeqCst) {
                if !session.terminal_sent.swap(true, Ordering::SeqCst) {
                    let _ = tx.send(SubagentEvent::Terminal {
                        outcome: TerminalOutcome::Interrupted,
                    });
                }
            } else if !session.terminal_sent.swap(true, Ordering::SeqCst) {
                let _ = tx.send(SubagentEvent::Terminal {
                    outcome: fixture.terminal,
                });
            }
        });

        Ok(handle)
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        let _ = session.tx.send(SubagentEvent::Progress {
            summary: short_line(format!("prompt: {text}")),
        });
        Ok(())
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        session.interrupted.store(true, Ordering::SeqCst);
        if !session.terminal_sent.swap(true, Ordering::SeqCst) {
            let _ = session.tx.send(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Interrupted,
            });
        }
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        Ok(receiver_stream(session.tx.subscribe()))
    }
}

#[derive(Default)]
pub struct ClaudeCodeDriver {
    sessions: Mutex<HashMap<String, Arc<ProcessSession>>>,
}

struct ProcessSession {
    tx: broadcast::Sender<SubagentEvent>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pgid: i32,
}

impl Drop for ProcessSession {
    fn drop(&mut self) {
        kill_process_group(self.pgid);
    }
}

#[async_trait]
impl SubagentDriver for ClaudeCodeDriver {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle> {
        let mut command = Command::new("claude");
        command
            .arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--dangerously-skip-permissions")
            .arg("--verbose")
            .current_dir(&task.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        start_in_process_group(&mut command);

        let mut child = command.spawn()?;
        let pgid = child.id().map(|id| id as i32).unwrap_or_default();
        let stdin = child
            .stdin
            .take()
            .ok_or(DriverError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(DriverError::MissingPipe("stdout"))?;
        let (tx, _) = broadcast::channel(256);
        let session = Arc::new(ProcessSession {
            tx: tx.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            pgid,
        });
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: AgentKind::Claude,
        };
        lock(&self.sessions)?.insert(handle.id.clone(), session.clone());
        tokio::spawn(read_claude_stdout(stdout, child, tx));

        let mut stdin = session.stdin.lock().await;
        write_json_line(&mut stdin, &claude_user_message(&task.prompt)).await?;
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
        let mut stdin = session.stdin.lock().await;
        write_json_line(&mut stdin, &claude_user_message(&text)).await
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let session = {
            let sessions = lock(&self.sessions)?;
            sessions
                .get(&handle.id)
                .cloned()
                .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?
        };
        let request_id = format!("hirsel-interrupt-{}", Uuid::new_v4());
        let mut stdin = session.stdin.lock().await;
        write_json_line(
            &mut stdin,
            &json!({
                "type": "control_request",
                "request_id": request_id,
                "request": { "subtype": "interrupt" }
            }),
        )
        .await
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        Ok(receiver_stream(session.tx.subscribe()))
    }
}

fn claude_user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
}

async fn read_claude_stdout(
    stdout: ChildStdout,
    mut child: Child,
    tx: broadcast::Sender<SubagentEvent>,
) {
    let mut terminal_sent = false;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    for event in claude_events(&value) {
                        terminal_sent |= matches!(event, SubagentEvent::Terminal { .. });
                        let _ = tx.send(event);
                    }
                }
                Err(error) => {
                    let _ = tx.send(SubagentEvent::Progress {
                        summary: short_line(format!("unparsed claude output: {error}")),
                    });
                }
            },
            Ok(None) => break,
            Err(error) => {
                if !terminal_sent {
                    let _ = tx.send(SubagentEvent::Terminal {
                        outcome: TerminalOutcome::Failed {
                            reason: format!("claude stdout error: {error}"),
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
            let _ = tx.send(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: format!("claude exited without terminal result: {status}"),
                },
            });
        }
        Err(error) if !terminal_sent => {
            let _ = tx.send(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: format!("claude wait failed: {error}"),
                },
            });
        }
        Err(_) => {}
    }
}

fn claude_events(value: &Value) -> Vec<SubagentEvent> {
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    match kind {
        "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => value
            .get("session_id")
            .and_then(Value::as_str)
            .map(|external_id| SubagentEvent::Started {
                external_id: external_id.to_string(),
            })
            .into_iter()
            .collect(),
        "assistant" => claude_assistant_progress(value)
            .into_iter()
            .map(|summary| SubagentEvent::Progress { summary })
            .collect(),
        "user" => claude_tool_result(value)
            .into_iter()
            .map(|summary| SubagentEvent::Progress { summary })
            .collect(),
        "result" => vec![SubagentEvent::Terminal {
            outcome: claude_terminal_outcome(value),
        }],
        "rate_limit_event" => vec![SubagentEvent::Progress {
            summary: "claude rate limit status updated".to_string(),
        }],
        _ => Vec::new(),
    }
}

fn claude_assistant_progress(value: &Value) -> Vec<String> {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(
            |content| match content.get("type").and_then(Value::as_str) {
                Some("text") => content.get("text").and_then(Value::as_str).map(short_line),
                Some("tool_use") => {
                    let name = content
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let detail = content
                        .pointer("/input/command")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| content.get("input").map(Value::to_string))
                        .unwrap_or_default();
                    Some(short_line(format!("tool {name}: {detail}")))
                }
                _ => None,
            },
        )
        .collect()
}

fn claude_tool_result(value: &Value) -> Option<String> {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|content| content.get("content"))
        .and_then(Value::as_str)
        .map(|content| short_line(format!("tool result: {content}")))
}

fn claude_terminal_outcome(value: &Value) -> TerminalOutcome {
    let summary = value
        .get("result")
        .and_then(Value::as_str)
        .map(short_line)
        .unwrap_or_else(|| "claude turn completed".to_string());
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_error {
        TerminalOutcome::Done { summary }
    } else if value.get("terminal_reason").and_then(Value::as_str) == Some("aborted_streaming") {
        TerminalOutcome::Interrupted
    } else {
        let reason = value
            .get("terminal_reason")
            .and_then(Value::as_str)
            .or_else(|| value.get("stop_reason").and_then(Value::as_str))
            .map(|reason| format!("{reason}: {summary}"))
            .unwrap_or(summary);
        TerminalOutcome::Failed { reason }
    }
}

#[derive(Default)]
pub struct CodexDriver {
    sessions: Mutex<HashMap<String, Arc<CodexSession>>>,
}

struct CodexSession {
    tx: broadcast::Sender<SubagentEvent>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    thread_id: String,
    active_turn_id: Mutex<Option<String>>,
    next_request_id: AtomicU64,
    pgid: i32,
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
        let mut lines = BufReader::new(stdout).lines();
        let (tx, _) = broadcast::channel(256);

        write_json_line(&mut stdin, &codex_initialize_request(1)).await?;
        write_json_line(&mut stdin, &codex_thread_start_request(2, &task.cwd)).await?;
        let thread_id = timeout(
            Duration::from_secs(30),
            read_codex_thread_id(&mut lines, tx.clone()),
        )
        .await
        .map_err(|_| DriverError::MissingExternalId)??;
        let _ = tx.send(SubagentEvent::Started {
            external_id: thread_id.clone(),
        });

        let session = Arc::new(CodexSession {
            tx: tx.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            thread_id: thread_id.clone(),
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
        let cwd = std::env::current_dir()?;
        let mut stdin = session.stdin.lock().await;
        write_json_line(
            &mut stdin,
            &codex_turn_start_request(request_id, &session.thread_id, &text, &cwd),
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

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        Ok(receiver_stream(session.tx.subscribe()))
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
    tx: broadcast::Sender<SubagentEvent>,
) -> DriverResult<String> {
    while let Some(line) = lines.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        if let Some(summary) = codex_progress(&value) {
            let _ = tx.send(SubagentEvent::Progress { summary });
        }
        if value.get("id").and_then(Value::as_u64) == Some(2) {
            if let Some(thread_id) = value.pointer("/result/thread/id").and_then(Value::as_str) {
                return Ok(thread_id.to_string());
            }
        }
        if value.get("method").and_then(Value::as_str) == Some("thread/started") {
            if let Some(thread_id) = value.pointer("/params/thread/id").and_then(Value::as_str) {
                return Ok(thread_id.to_string());
            }
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
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    if let Some(turn_id) = value.pointer("/result/turn/id").and_then(Value::as_str)
                    {
                        if let Ok(mut active_turn_id) = lock(&session.active_turn_id) {
                            *active_turn_id = Some(turn_id.to_string());
                        }
                    }
                    if value.get("method").and_then(Value::as_str) == Some("turn/started") {
                        if let Some(turn_id) =
                            value.pointer("/params/turn/id").and_then(Value::as_str)
                        {
                            if let Ok(mut active_turn_id) = lock(&session.active_turn_id) {
                                *active_turn_id = Some(turn_id.to_string());
                            }
                        }
                    }
                    if let Some(summary) = codex_progress(&value) {
                        let _ = session.tx.send(SubagentEvent::Progress { summary });
                    }
                    if let Some(outcome) = codex_terminal_outcome(&value) {
                        terminal_sent = true;
                        if let Ok(mut active_turn_id) = lock(&session.active_turn_id) {
                            *active_turn_id = None;
                        }
                        let _ = session.tx.send(SubagentEvent::Terminal { outcome });
                    }
                }
                Err(error) => {
                    let _ = session.tx.send(SubagentEvent::Progress {
                        summary: short_line(format!("unparsed codex output: {error}")),
                    });
                }
            },
            Ok(None) => break,
            Err(error) => {
                if !terminal_sent {
                    let _ = session.tx.send(SubagentEvent::Terminal {
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
            let _ = session.tx.send(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: format!("codex exited without terminal notification: {status}"),
                },
            });
        }
        Err(error) if !terminal_sent => {
            let _ = session.tx.send(SubagentEvent::Terminal {
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

fn codex_terminal_outcome(value: &Value) -> Option<TerminalOutcome> {
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
            reason: turn
                .get("error")
                .map(Value::to_string)
                .unwrap_or_else(|| "codex turn failed".to_string()),
        }),
        _ => Some(TerminalOutcome::Done {
            summary: "codex turn completed".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::time::Duration;

    #[tokio::test]
    async fn fake_driver_emits_started_progress_and_done() {
        let driver = FakeDriver::default();
        let handle = driver
            .spawn(SpawnSpec {
                agent: AgentKind::Claude,
                prompt: "fix it".to_string(),
                cwd: std::env::current_dir().unwrap(),
                fake_fixture: None,
            })
            .await
            .unwrap();
        let mut events = driver.events(&handle).unwrap();

        let first = events.next().await.unwrap();
        assert!(matches!(first, SubagentEvent::Started { .. }));
        let mut saw_progress = false;
        loop {
            let event = timeout(Duration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap();
            match event {
                SubagentEvent::Progress { .. } => saw_progress = true,
                SubagentEvent::Terminal {
                    outcome: TerminalOutcome::Done { .. },
                } => break,
                other => panic!("unexpected fake event: {other:?}"),
            }
        }
        assert!(saw_progress);
    }

    #[tokio::test]
    async fn fake_driver_interrupt_is_terminal() {
        let fixture = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            fixture.path(),
            serde_json::to_string(&json!({
                "external_id": "fake-long",
                "progress": ["one", "two"],
                "delay_ms": 200,
                "terminal": { "status": "done", "summary": "should not happen" }
            }))
            .unwrap(),
        )
        .unwrap();
        let driver = FakeDriver::default();
        let handle = driver
            .spawn(SpawnSpec {
                agent: AgentKind::Codex,
                prompt: "wait".to_string(),
                cwd: std::env::current_dir().unwrap(),
                fake_fixture: Some(fixture.path().to_path_buf()),
            })
            .await
            .unwrap();
        let mut events = driver.events(&handle).unwrap();
        let _ = events.next().await.unwrap();
        driver.interrupt(&handle).await.unwrap();

        let event = timeout(Duration::from_secs(1), events.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event,
            SubagentEvent::Terminal {
                outcome: TerminalOutcome::Interrupted
            }
        );
    }

    #[tokio::test]
    #[ignore = "requires the real claude CLI and may spend model tokens"]
    async fn claude_code_driver_real_cli_smoke() {
        let driver = ClaudeCodeDriver::default();
        let handle = driver
            .spawn(SpawnSpec {
                agent: AgentKind::Claude,
                prompt: "Reply with exactly: driver-smoke".to_string(),
                cwd: std::env::current_dir().unwrap(),
                fake_fixture: None,
            })
            .await
            .unwrap();
        let mut events = driver.events(&handle).unwrap();
        while let Some(event) = events.next().await {
            if matches!(event, SubagentEvent::Terminal { .. }) {
                return;
            }
        }
        panic!("claude CLI exited without a terminal event");
    }

    #[tokio::test]
    #[ignore = "requires the real codex CLI and may spend model tokens"]
    async fn codex_driver_real_cli_smoke() {
        let driver = CodexDriver::default();
        let handle = driver
            .spawn(SpawnSpec {
                agent: AgentKind::Codex,
                prompt: "Reply with exactly: driver-smoke".to_string(),
                cwd: std::env::current_dir().unwrap(),
                fake_fixture: None,
            })
            .await
            .unwrap();
        let mut events = driver.events(&handle).unwrap();
        while let Some(event) = events.next().await {
            if matches!(event, SubagentEvent::Terminal { .. }) {
                return;
            }
        }
        panic!("codex CLI exited without a terminal event");
    }
}

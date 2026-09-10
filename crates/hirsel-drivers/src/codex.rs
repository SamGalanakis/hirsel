//! Driver for the `codex app-server` JSON-RPC protocol.

use std::{
    collections::{BTreeSet, HashMap},
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

#[path = "codex_config.rs"]
mod config;
#[path = "codex_io.rs"]
mod io;
pub(crate) use io::{codex_agent_message, codex_terminal_outcome};
use io::{codex_progress, read_codex_stdout};

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
    final_agent_message: Option<String>,
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
        if state.closed || self.events.is_terminal() {
            return Err(DriverError::NoActiveTurn);
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
                let _ = self.events.complete(
                    TerminalOutcome::Failed {
                        reason: reason.to_string(),
                    },
                    state.final_agent_message.clone(),
                );
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
                        error
                            .get("code")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "unknown code".into())
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
            let _ = self.events.emit(SubagentEvent::Progress {
                summary: "Codex configuration warning".into(),
            });
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
        if matches!(method, Some("item/started" | "item/completed"))
            && (state.active_turn_id.is_none()
                || value.pointer("/params/turnId").and_then(Value::as_str)
                    != state.active_turn_id.as_deref())
        {
            return Ok(());
        }
        if let Some(message) = codex_agent_message(&value) {
            state.last_agent_message = Some(message.to_string());
            if value.pointer("/params/item/phase").and_then(Value::as_str) == Some("final_answer") {
                state.final_agent_message = Some(message.to_string());
            }
        } else if matches!(method, Some("item/started" | "item/completed")) {
            // An unknown-phase message preceding more work is not a final answer.
            state.last_agent_message = None;
        }
        if let Some(summary) = codex_progress(&value) {
            let _ = self.events.emit(SubagentEvent::Progress { summary });
        }
        if method == Some("turn/completed") {
            let turn_id = value
                .pointer("/params/turn/id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| protocol_error("codex completion missing turn identity"))?;
            if state.active_turn_id.as_deref() != Some(turn_id) {
                return Ok(());
            }
            let completed =
                value.pointer("/params/turn/status").and_then(Value::as_str) == Some("completed");
            // Unknown phase is valid in the installed protocol, but only an
            // actual successful terminal can promote it to final output.
            let output = state.final_agent_message.clone().or_else(|| {
                completed
                    .then(|| state.last_agent_message.clone())
                    .flatten()
            });
            if let Some(outcome) = codex_terminal_outcome(&value, output.as_deref()) {
                state.active_turn_id = None;
                let _ = self.events.complete(outcome, output);
            }
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
        config::validate_launch(&task)?;
        let mut command = Command::new("codex");
        config::configure_command(&mut command, &task);
        self.spawn_command(task, command, CONTROL_TIMEOUT).await
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
        if let Err(error) = session
            .request(
                "turn/interrupt",
                json!({
                    "jsonrpc": "2.0", "method": "turn/interrupt",
                    "params": { "threadId": thread_id, "turnId": turn_id }
                }),
            )
            .await
        {
            session.fail("codex interrupt request failed");
            return Err(error);
        }
        match timeout(session.control_timeout, session.events.wait_terminal()).await {
            Ok(result) => result?,
            Err(_) => {
                session.fail("codex interrupt was acknowledged without terminal completion");
                return Err(DriverError::RequestTimeout(
                    "codex terminal after interrupt".into(),
                ));
            }
        }
        session.process_group.kill_group();
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
        control_timeout: Duration,
    ) -> DriverResult<SessionHandle> {
        config::validate_launch(&task)?;
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
        let effective = session
            .request(
                "config/read",
                json!({"method":"config/read", "params":{
                    "includeLayers":true, "cwd":task.cwd
                }}),
            )
            .await?;
        let inherited = config::inherited_names(&effective)?;
        let bridge = format!("hirsel_thread_{}", Uuid::new_v4().simple());
        if inherited.contains(&bridge) {
            return Err(protocol_error("Codex scoped bridge name collision"));
        }
        let opened = session
            .request(
                "thread/start",
                config::thread_start_request(&task, &inherited, &bridge),
            )
            .await?;
        let thread_id = opened
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or(DriverError::MissingExternalId)?;
        config::verify_catalog(&session, thread_id, &bridge, &task.scoped_mcp).await?;
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

#[cfg(test)]
#[path = "codex_native_tests.rs"]
mod native_tests;

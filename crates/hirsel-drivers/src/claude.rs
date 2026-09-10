//! Driver for the `claude` CLI in headless stream-json mode.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, Mutex, Weak},
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::oneshot,
    time::{Duration, Instant, timeout},
};
use uuid::Uuid;

#[path = "claude_config.rs"]
mod config;
#[path = "claude_events.rs"]
mod events;
use events::ClaudeOutput;
#[cfg(test)]
pub(crate) use events::claude_terminal_outcome;

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

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DRAIN_GRACE: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct ClaudeCodeDriver {
    sessions: SessionRegistry<ProcessSession>,
}

struct PendingRequest {
    input: bool,
    sender: oneshot::Sender<DriverResult<()>>,
}

struct ProcessSession {
    events: Arc<EventHub>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<String, PendingRequest>>,
    process_group: ProcessGroup,
    output: Mutex<ClaudeOutput>,
    ready: tokio::sync::Notify,
}

// A cancelled request cannot leave a pending waiter alive in the session.
struct RequestGuard<'a> {
    session: &'a ProcessSession,
    id: String,
}
impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.session.pending.lock() {
            pending.remove(&self.id);
        }
    }
}

impl ProcessSession {
    async fn request(
        &self,
        value: Value,
        id: String,
        input: bool,
        limit: Duration,
    ) -> DriverResult<()> {
        if self.events.is_terminal() {
            return Err(DriverError::SessionClosed);
        }
        let (tx, rx) = oneshot::channel();
        lock(&self.pending)?.insert(id.clone(), PendingRequest { input, sender: tx });
        let _guard = RequestGuard { session: self, id };
        timeout(limit, async {
            {
                let mut stdin = self.stdin.lock().await;
                if self.events.is_terminal() {
                    return Err(DriverError::SessionClosed);
                }
                write_json_line(stdin.as_mut().ok_or(DriverError::SessionClosed)?, &value).await?;
            }
            rx.await.unwrap_or(Err(DriverError::SessionClosed))
        })
        .await
        .map_err(|_| {
            DriverError::RequestTimeout(
                if input {
                    "claude input acknowledgement"
                } else {
                    "claude interrupt"
                }
                .into(),
            )
        })?
    }

    async fn interrupt_with_timeout(&self, id: String, limit: Duration) -> DriverResult<()> {
        let result = self
            .request(
                json!({"type":"control_request", "request_id":id,
                    "request":{"subtype":"interrupt"}}),
                id,
                false,
                limit,
            )
            .await;
        let completed = if result.is_ok() {
            timeout(limit, self.events.wait_terminal())
                .await
                .unwrap_or_else(|_| {
                    Err(DriverError::RequestTimeout(
                        "claude interrupt completion".into(),
                    ))
                })
        } else {
            result
        };
        if completed.is_err() {
            self.process_group.kill_group();
            self.reject_pending(false);
            self.events.complete(TerminalOutcome::Interrupted, None)?;
        }
        // A result ends this execution even if the CLI keeps its input loop open.
        self.process_group.kill_group();
        self.reject_pending(false);
        drop(self.stdin.lock().await.take());
        completed
    }

    fn reject_pending(&self, inputs_only: bool) {
        if let Ok(mut pending) = self.pending.lock() {
            let ids = pending
                .iter()
                .filter(|(_, request)| !inputs_only || request.input)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in ids {
                if let Some(request) = pending.remove(&id) {
                    let _ = request.sender.send(Err(DriverError::SessionClosed));
                }
            }
        }
    }

    fn handle(&self, value: &Value) {
        let kind = value.get("type").and_then(Value::as_str);
        let receipt = if kind == Some("control_response") {
            value.get("response").and_then(|response| {
                let id = response.get("request_id")?.as_str()?;
                let result = match response.get("subtype").and_then(Value::as_str) {
                    Some("success") => Ok(()),
                    Some("error") => Err(DriverError::Protocol(
                        response
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("Claude rejected interrupt")
                            .to_owned(),
                    )),
                    _ => Err(DriverError::Protocol(
                        "invalid Claude control response".into(),
                    )),
                };
                Some((id, false, result))
            })
        } else if kind == Some("user") && value.get("parent_tool_use_id").is_none_or(Value::is_null)
        {
            value
                .get("uuid")
                .and_then(Value::as_str)
                .map(|id| (id, true, Ok(())))
        } else {
            None
        };
        if let Some((id, input, result)) = receipt
            && let Ok(mut pending) = self.pending.lock()
            && pending
                .get(id)
                .is_some_and(|request| request.input == input)
            && let Some(request) = pending.remove(id)
        {
            let _ = request.sender.send(result);
        }
        let result = lock(&self.output).and_then(|mut output| output.handle(value, &self.events));
        if let Err(error) = result {
            let _ = self.events.complete(
                TerminalOutcome::Failed {
                    reason: error.to_string(),
                },
                None,
            );
        }
        self.ready.notify_one();
        if self.events.is_terminal() {
            self.reject_pending(true);
        }
    }
}

impl ClaudeCodeDriver {
    async fn spawn_command(
        &self,
        task: SpawnSpec,
        mut command: Command,
        limit: Duration,
    ) -> DriverResult<SessionHandle> {
        config::preflight(&task.scoped_mcp, limit).await?;
        config::configure(&mut command, &task)?;
        command
            .arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--replay-user-messages")
            .arg("--include-partial-messages")
            .arg("--dangerously-skip-permissions")
            .arg("--verbose")
            .current_dir(&task.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(model) = task.model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(variant) = task.variant.as_deref() {
            command.arg("--effort").arg(variant);
        }
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
        let stderr_task = tokio::spawn(drain_stderr(stderr));
        let events = EventHub::new(256);
        let session = Arc::new(ProcessSession {
            events: events.clone(),
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(HashMap::new()),
            process_group,
            output: Mutex::new(ClaudeOutput::new(&task.scoped_mcp.expected_tools)),
            ready: tokio::sync::Notify::new(),
        });
        tokio::spawn(read_claude_stdout(
            stdout,
            child,
            Arc::downgrade(&session),
            events,
            stderr_task,
        ));
        let id = Uuid::new_v4().to_string();
        // Publish no handle until the CLI has echoed this exact input UUID.
        session
            .request(claude_user_message(&task.prompt, &id), id, true, limit)
            .await?;
        timeout(limit, async {
            loop {
                if lock(&session.output)?.initialized() {
                    return Ok(());
                }
                if session.events.is_terminal() {
                    return Err(DriverError::Protocol(
                        "Claude failed before scoped initialization".into(),
                    ));
                }
                session.ready.notified().await;
            }
        })
        .await
        .map_err(|_| DriverError::RequestTimeout("claude scoped initialization".into()))??;
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: AgentKind::Claude,
        };
        self.sessions.insert(handle.id.clone(), session)?;
        Ok(handle)
    }
}

#[async_trait]
impl SubagentDriver for ClaudeCodeDriver {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle> {
        self.spawn_command(task, Command::new("claude"), REQUEST_TIMEOUT)
            .await
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        let id = Uuid::new_v4().to_string();
        session
            .request(claude_user_message(&text, &id), id, true, REQUEST_TIMEOUT)
            .await
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        let id = Uuid::new_v4().to_string();
        if session.events.is_terminal() {
            return Err(DriverError::SessionClosed);
        }
        session.interrupt_with_timeout(id, REQUEST_TIMEOUT).await
    }

    async fn retire(&self, handle: &SessionHandle) -> DriverResult<()> {
        if let Some(session) = self.sessions.remove(handle)? {
            session.process_group.kill_group();
            session
                .events
                .complete(TerminalOutcome::Interrupted, None)?;
            session.reject_pending(false);
            drop(session.stdin.lock().await.take());
        }
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        self.sessions.get(handle)?.events.stream()
    }
}

fn claude_user_message(text: &str, id: &str) -> Value {
    json!({"type":"user", "uuid":id, "parent_tool_use_id":null,
        "message":{"role":"user", "content":[{"type":"text","text":text}]}})
}

async fn read_claude_stdout(
    stdout: ChildStdout,
    mut child: Child,
    session: Weak<ProcessSession>,
    events: Arc<EventHub>,
    stderr_task: tokio::task::JoinHandle<()>,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut exit = None;
    let mut drain_deadline = None;
    let mut failure = None;
    loop {
        tokio::select! {
            status = child.wait(), if exit.is_none() => {
                exit = Some(status);
                drain_deadline = Some(Instant::now() + DRAIN_GRACE);
            }
            _ = async { if let Some(deadline) = drain_deadline { tokio::time::sleep_until(deadline).await } else { std::future::pending::<()>().await } } => break,
            line = lines.next_line() => match line {
                Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(value) => if let Some(session) = session.upgrade() { session.handle(&value); },
                    Err(error) => { let _ = events.emit(SubagentEvent::Progress { summary: short_line(format!("unparsed claude output: {error}")) }); }
                },
                Ok(None) => break,
                Err(error) => { failure = Some(format!("claude stdout error: {error}")); break; }
            }
        }
    }
    if let Some(session) = session.upgrade() {
        session.process_group.kill_group();
    }
    if exit.is_none() {
        // EOF does not establish child exit: close the owned process too.
        let _ = child.start_kill();
        exit = Some(
            timeout(DRAIN_GRACE, child.wait())
                .await
                .unwrap_or_else(|_| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "child did not reap after kill",
                    ))
                }),
        );
    }
    if !events.is_terminal() {
        let reason = failure.unwrap_or_else(|| match exit {
            Some(Ok(status)) => format!("claude exited without terminal result: {status}"),
            Some(Err(error)) => format!("claude wait failed: {error}"),
            None => "claude stream ended without terminal result".into(),
        });
        let output = session
            .upgrade()
            .and_then(|session| lock(&session.output).ok()?.take_final());
        let _ = events.complete(TerminalOutcome::Failed { reason }, output);
    }
    if let Some(session) = session.upgrade() {
        session.reject_pending(false);
    }
    stderr_task.abort();
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod lifecycle_tests;

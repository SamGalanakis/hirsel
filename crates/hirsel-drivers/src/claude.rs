//! Driver for the `claude` CLI in headless stream-json mode.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
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
pub struct ClaudeCodeDriver {
    sessions: Mutex<HashMap<String, Arc<ProcessSession>>>,
}

struct ProcessSession {
    events: Arc<EventHub>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pgid: i32,
}

impl ProcessSession {
    fn kill_group(&self) {
        kill_process_group(self.pgid);
    }
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
        if let Some(model) = task.model.as_deref() {
            command.arg("--model").arg(model);
        }
        // `claude --effort <low|medium|high|xhigh|max>` sets the session's
        // reasoning effort in headless `-p` mode, mirroring codex's
        // `-c model_reasoning_effort=`. Registry variants are a subset of that
        // enum, so a resolved variant is always a value the CLI accepts.
        if let Some(variant) = task.variant.as_deref() {
            command.arg("--effort").arg(variant);
        }
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
        let stderr = child
            .stderr
            .take()
            .ok_or(DriverError::MissingPipe("stderr"))?;
        let events = EventHub::new(256);
        let session = Arc::new(ProcessSession {
            events: events.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            pgid,
        });
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: AgentKind::Claude,
        };
        lock(&self.sessions)?.insert(handle.id.clone(), session.clone());
        tokio::spawn(drain_stderr(stderr));
        tokio::spawn(read_claude_stdout(stdout, child, events));

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

fn claude_user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
}

async fn read_claude_stdout(stdout: ChildStdout, mut child: Child, events: Arc<EventHub>) {
    let mut terminal_sent = false;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    for event in claude_events(&value) {
                        terminal_sent |= matches!(event, SubagentEvent::Terminal { .. });
                        let _ = events.emit(event);
                    }
                }
                Err(error) => {
                    let _ = events.emit(SubagentEvent::Progress {
                        summary: short_line(format!("unparsed claude output: {error}")),
                    });
                }
            },
            Ok(None) => break,
            Err(error) => {
                if !terminal_sent {
                    let _ = events.emit(SubagentEvent::Terminal {
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
            let _ = events.emit(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: format!("claude exited without terminal result: {status}"),
                },
            });
        }
        Err(error) if !terminal_sent => {
            let _ = events.emit(SubagentEvent::Terminal {
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

pub(crate) fn claude_terminal_outcome(value: &Value) -> TerminalOutcome {
    let summary = value
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "claude turn completed".to_string());
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_error {
        TerminalOutcome::Done {
            summary: terminal_message(summary),
        }
    } else if value.get("terminal_reason").and_then(Value::as_str) == Some("aborted_streaming") {
        TerminalOutcome::Interrupted
    } else {
        let reason = value
            .get("terminal_reason")
            .and_then(Value::as_str)
            .or_else(|| value.get("stop_reason").and_then(Value::as_str))
            .map(|reason| format!("{reason}: {summary}"))
            .unwrap_or(summary);
        TerminalOutcome::Failed {
            reason: terminal_message(reason),
        }
    }
}

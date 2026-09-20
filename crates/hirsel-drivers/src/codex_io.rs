//! Codex stream supervision and terminal decoding.
use super::*;
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

const TOOL_EVENT_VALUE_BYTES: usize = 4096;
const TOOL_EVENT_TRUNCATION_MARKER: &str = "…[truncated]";

#[derive(Default)]
pub(super) struct CodexToolState {
    turn_id: Option<String>,
    next_synthetic_id: u64,
    active: Vec<ActiveCodexTool>,
    used_event_ids: HashSet<String>,
    completed_source_ids: HashSet<String>,
}

struct ActiveCodexTool {
    source_id: Option<String>,
    event_id: String,
    name: String,
    fingerprint: u64,
}

enum ToolStartDisposition {
    Emit(String),
    Duplicate,
    Conflict,
}

impl CodexToolState {
    pub(super) fn begin_turn(&mut self, turn_id: &str) {
        if self.turn_id.as_deref() == Some(turn_id) {
            return;
        }
        *self = Self::default();
        self.turn_id = Some(turn_id.to_string());
    }

    fn start(
        &mut self,
        source_id: Option<&str>,
        name: &str,
        fingerprint: u64,
    ) -> ToolStartDisposition {
        let Some(source_id) = source_id else {
            return ToolStartDisposition::Emit(self.unique_event_id(None));
        };
        if let Some(active) = self
            .active
            .iter()
            .find(|active| active.source_id.as_deref() == Some(source_id))
        {
            return if active.name == name && active.fingerprint == fingerprint {
                ToolStartDisposition::Duplicate
            } else {
                ToolStartDisposition::Conflict
            };
        }
        if self.completed_source_ids.contains(source_id) || self.used_event_ids.contains(source_id)
        {
            return ToolStartDisposition::Conflict;
        }
        self.used_event_ids.insert(source_id.to_string());
        ToolStartDisposition::Emit(source_id.to_string())
    }

    fn completion_id(
        &mut self,
        source_id: Option<&str>,
        name: &str,
        fingerprint: u64,
    ) -> Option<String> {
        // Provider identity is authoritative when present. Anonymous events
        // have no stronger identity than their repeated fields: pair the
        // earliest exact match, then the earliest same-name call. A completion
        // with no active match remains visible under a fresh synthetic ID.
        let position = source_id
            .and_then(|source_id| {
                self.active
                    .iter()
                    .position(|active| active.source_id.as_deref() == Some(source_id))
            })
            .or_else(|| {
                source_id.is_none().then(|| {
                    self.active.iter().position(|active| {
                        active.source_id.is_none() && active.fingerprint == fingerprint
                    })
                })?
            })
            .or_else(|| {
                source_id.is_none().then(|| {
                    self.active
                        .iter()
                        .position(|active| active.source_id.is_none() && active.name == name)
                })?
            });
        if let Some(position) = position {
            let active = self.active.remove(position);
            if let Some(source_id) = source_id {
                self.completed_source_ids.insert(source_id.to_string());
            }
            return Some(active.event_id);
        }
        if source_id.is_some_and(|id| self.completed_source_ids.contains(id)) {
            return None;
        }
        let event_id = self.unique_event_id(source_id);
        if let Some(source_id) = source_id {
            self.completed_source_ids.insert(source_id.to_string());
        }
        Some(event_id)
    }

    fn unique_event_id(&mut self, preferred: Option<&str>) -> String {
        if let Some(preferred) = preferred.filter(|id| !id.is_empty())
            && self.used_event_ids.insert(preferred.to_string())
        {
            return preferred.to_string();
        }
        loop {
            self.next_synthetic_id += 1;
            let candidate = format!("codex:{}", self.next_synthetic_id);
            if self.used_event_ids.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

pub(super) async fn read_codex_stdout(
    mut lines: Lines<BufReader<ChildStdout>>,
    mut child: Child,
    session: Weak<CodexSession>,
    stderr_task: tokio::task::JoinHandle<()>,
) {
    let mut exit = None;
    let mut drain_deadline = None;
    let mut reply_failure = None;
    let mut replies: FuturesUnordered<futures_util::future::BoxFuture<'static, DriverResult<()>>> =
        FuturesUnordered::new();
    let reason = loop {
        tokio::select! {
            status = child.wait(), if exit.is_none() => {
                let status = status.map(|status| status.to_string()).unwrap_or_else(|error| error.to_string());
                exit = Some(status);
                // A reply cannot reach an exited server. Cancel its write so
                // buffered stdout can drain even when a descendant holds stdin.
                replies.clear();
                if let Some(session) = session.upgrade() {
                    session.process_group.kill_group();
                }
                // Descendants can inherit stdout after the direct child exits.
                drain_deadline.get_or_insert(tokio::time::Instant::now() + EXIT_DRAIN_TIMEOUT);
            }
            line = lines.next_line() => {
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
                                if replies.len() >= 32 { break "too many unsupported Codex server requests".to_string(); }
                                replies.push(Box::pin(async move {
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
            Some(result) = replies.next(), if !replies.is_empty() => {
                if let Err(error) = result {
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
    drop(replies);
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

pub(super) fn codex_progress(value: &Value) -> Option<String> {
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
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage" | "plan") => item
            .get("text")
            .and_then(Value::as_str)
            .map(short_line)
            .filter(|summary| !summary.is_empty()),
        Some("reasoning") => {
            let text = ["summary", "content"]
                .into_iter()
                .filter_map(|key| item.get(key).and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then(|| short_line(text))
        }
        _ => None,
    }
}

pub(super) fn codex_timeline_event(value: &Value) -> Option<SubagentEvent> {
    if value.get("method").and_then(Value::as_str) != Some("item/completed") {
        return None;
    }
    let item = value.pointer("/params/item")?;
    let text = match item.get("type").and_then(Value::as_str)? {
        "agentMessage" | "plan" => item.get("text").and_then(Value::as_str)?.to_string(),
        "reasoning" => ["summary", "content"]
            .into_iter()
            .filter_map(|key| item.get(key).and_then(Value::as_array))
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    let block_id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Some(match item.get("type").and_then(Value::as_str) {
        Some("reasoning") => SubagentEvent::ReasoningDelta { text, block_id },
        _ => SubagentEvent::ProseDelta { text, block_id },
    })
}

pub(super) fn codex_tool_event(state: &mut CodexToolState, value: &Value) -> Option<SubagentEvent> {
    let method = value.get("method").and_then(Value::as_str)?;
    let completed = match method {
        "item/started" => false,
        "item/completed" => true,
        _ => return None,
    };
    let item = value.pointer("/params/item")?;
    let source_id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let item_type = item.get("type").and_then(Value::as_str)?;
    let fingerprint = codex_tool_fingerprint(item_type, item);
    let (name, args, output, ok) = match item_type {
        "commandExecution" => (
            "shell_run".to_string(),
            bounded_tool_value(json!({
                "cmd": item.get("command").cloned().unwrap_or(Value::Null),
                "cwd": item.get("cwd").cloned().unwrap_or(Value::Null),
                "command_actions": item.get("commandActions").cloned().unwrap_or_else(|| json!([]))
            })),
            bounded_tool_value(json!({
                "stdout": item.get("aggregatedOutput").cloned().unwrap_or(Value::Null),
                "status": item.get("exitCode").cloned().unwrap_or(Value::Null),
                "duration_ms": item.get("durationMs").cloned().unwrap_or(Value::Null)
            })),
            item.get("status").and_then(Value::as_str) == Some("completed"),
        ),
        "fileChange" => (
            "file_change".to_string(),
            bounded_tool_value(
                json!({"changes": item.get("changes").cloned().unwrap_or_else(|| json!([]))}),
            ),
            bounded_tool_value(json!({
                "changes": item.get("changes").cloned().unwrap_or_else(|| json!([])),
                "status": item.get("status").cloned().unwrap_or(Value::Null)
            })),
            item.get("status").and_then(Value::as_str) == Some("completed"),
        ),
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str)?;
            let tool = item.get("tool").and_then(Value::as_str)?;
            let server = if server.starts_with("hirsel_thread_") {
                "hirsel"
            } else {
                server
            };
            (
                format!("mcp__{server}__{tool}"),
                bounded_tool_value(item.get("arguments").cloned().unwrap_or_else(|| json!({}))),
                bounded_tool_value(
                    item.get("result")
                        .filter(|result| !result.is_null())
                        .cloned()
                        .or_else(|| item.get("error").cloned())
                        .unwrap_or(Value::Null),
                ),
                item.get("status").and_then(Value::as_str) == Some("completed")
                    && item.get("error").is_none_or(Value::is_null),
            )
        }
        _ => return None,
    };
    if completed {
        let call_id = state.completion_id(source_id, &name, fingerprint)?;
        Some(SubagentEvent::ToolCompleted {
            call_id,
            name,
            ok,
            output,
        })
    } else {
        let call_id = match state.start(source_id, &name, fingerprint) {
            ToolStartDisposition::Emit(call_id) => call_id,
            ToolStartDisposition::Duplicate => return None,
            ToolStartDisposition::Conflict => {
                return Some(SubagentEvent::Progress {
                    summary: short_line(format!(
                        "conflicting Codex tool start reused id {}",
                        source_id.unwrap_or("unknown")
                    )),
                });
            }
        };
        state.active.push(ActiveCodexTool {
            source_id: source_id.map(str::to_string),
            event_id: call_id.clone(),
            name: name.clone(),
            fingerprint,
        });
        Some(SubagentEvent::ToolStarted {
            call_id,
            name,
            args,
        })
    }
}

fn codex_tool_fingerprint(item_type: &str, item: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    item_type.hash(&mut hasher);
    let keys: &[&str] = match item_type {
        "commandExecution" => &["command", "cwd", "commandActions"],
        "fileChange" => &["changes"],
        "mcpToolCall" => &["server", "tool", "arguments"],
        _ => &[],
    };
    for key in keys {
        key.hash(&mut hasher);
        if let Some(value) = item.get(key) {
            hash_json(value, &mut hasher);
        }
    }
    hasher.finish()
}

fn hash_json(value: &Value, hasher: &mut DefaultHasher) {
    match value {
        Value::Null => 0_u8.hash(hasher),
        Value::Bool(value) => {
            1_u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Number(value) => {
            2_u8.hash(hasher);
            value.to_string().hash(hasher);
        }
        Value::String(value) => {
            3_u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Array(values) => {
            4_u8.hash(hasher);
            values.len().hash(hasher);
            for value in values {
                hash_json(value, hasher);
            }
        }
        Value::Object(values) => {
            5_u8.hash(hasher);
            values.len().hash(hasher);
            for (key, value) in values {
                key.hash(hasher);
                hash_json(value, hasher);
            }
        }
    }
}

fn bounded_tool_value(value: Value) -> Value {
    let encoded = value.to_string();
    if encoded.len() <= TOOL_EVENT_VALUE_BYTES {
        return value;
    }
    let mut end = TOOL_EVENT_VALUE_BYTES.min(encoded.len());
    loop {
        while !encoded.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = json!({
            "_hirsel_truncated": true,
            "preview": format!("{}{}", &encoded[..end], TOOL_EVENT_TRUNCATION_MARKER),
        });
        let candidate_len = candidate.to_string().len();
        if candidate_len <= TOOL_EVENT_VALUE_BYTES {
            return candidate;
        }
        let reduction = (candidate_len - TOOL_EVENT_VALUE_BYTES).max(1);
        end = end.saturating_sub(reduction);
    }
}

pub(crate) fn codex_agent_message(value: &Value) -> Option<&str> {
    if value.get("method").and_then(Value::as_str) != Some("item/completed") {
        return None;
    }
    let item = value.pointer("/params/item")?;
    if item.get("type").and_then(Value::as_str) != Some("agentMessage")
        || !matches!(
            item.get("phase").and_then(Value::as_str),
            None | Some("final_answer")
        )
    {
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
    match turn.get("status").and_then(Value::as_str) {
        Some("interrupted") => Some(TerminalOutcome::Interrupted),
        Some("failed") => Some(TerminalOutcome::Failed {
            reason: terminal_message(
                turn.get("error")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "codex turn failed".to_string()),
            ),
        }),
        Some("completed") => Some(TerminalOutcome::Done {
            summary: terminal_message(last_agent_message.unwrap_or("")),
        }),
        _ => Some(TerminalOutcome::Failed {
            reason: "codex terminal status missing or invalid".into(),
        }),
    }
}

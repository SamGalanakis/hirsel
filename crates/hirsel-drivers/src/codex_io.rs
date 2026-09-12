//! Codex stream supervision and terminal decoding.
use super::*;
use futures_util::{StreamExt, stream::FuturesUnordered};

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

pub(super) fn codex_tool_event(value: &Value) -> Option<SubagentEvent> {
    let method = value.get("method").and_then(Value::as_str)?;
    let completed = match method {
        "item/started" => false,
        "item/completed" => true,
        _ => return None,
    };
    let item = value.pointer("/params/item")?;
    let call_id = item.get("id").and_then(Value::as_str)?.to_string();
    let item_type = item.get("type").and_then(Value::as_str)?;
    let (name, args, output, ok) = match item_type {
        "commandExecution" => (
            "shell_run".to_string(),
            json!({
                "cmd": item.get("command").cloned().unwrap_or(Value::Null),
                "cwd": item.get("cwd").cloned().unwrap_or(Value::Null),
                "command_actions": item.get("commandActions").cloned().unwrap_or_else(|| json!([]))
            }),
            json!({
                "stdout": item.get("aggregatedOutput").cloned().unwrap_or(Value::Null),
                "status": item.get("exitCode").cloned().unwrap_or(Value::Null),
                "duration_ms": item.get("durationMs").cloned().unwrap_or(Value::Null)
            }),
            item.get("status").and_then(Value::as_str) == Some("completed"),
        ),
        "fileChange" => (
            "file_change".to_string(),
            json!({"changes": item.get("changes").cloned().unwrap_or_else(|| json!([]))}),
            json!({
                "changes": item.get("changes").cloned().unwrap_or_else(|| json!([])),
                "status": item.get("status").cloned().unwrap_or(Value::Null)
            }),
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
                item.get("arguments").cloned().unwrap_or_else(|| json!({})),
                item.get("result")
                    .filter(|result| !result.is_null())
                    .cloned()
                    .or_else(|| item.get("error").cloned())
                    .unwrap_or(Value::Null),
                item.get("status").and_then(Value::as_str) == Some("completed")
                    && item.get("error").is_none_or(Value::is_null),
            )
        }
        _ => return None,
    };
    if completed {
        Some(SubagentEvent::ToolCompleted {
            call_id,
            name,
            ok,
            output,
        })
    } else {
        Some(SubagentEvent::ToolStarted {
            call_id,
            name,
            args,
        })
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

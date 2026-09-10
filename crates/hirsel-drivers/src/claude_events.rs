//! Claude's provider messages remain diagnostics until an actual final result.
use std::collections::BTreeSet;

use super::*;

pub(super) struct ClaudeOutput {
    expected: BTreeSet<String>,
    session_id: Option<String>,
    assistant: Option<String>,
}

impl ClaudeOutput {
    pub(super) fn new(expected: &[String]) -> Self {
        Self {
            expected: expected
                .iter()
                .map(|name| format!("mcp__hirsel__{name}"))
                .collect(),
            session_id: None,
            assistant: None,
        }
    }

    pub(super) fn initialized(&self) -> bool {
        self.session_id.is_some()
    }

    pub(super) fn take_final(&mut self) -> Option<String> {
        self.assistant.take()
    }

    pub(super) fn handle(&mut self, value: &Value, events: &EventHub) -> DriverResult<()> {
        if events.is_terminal() {
            return Ok(());
        }
        if value
            .get("parent_tool_use_id")
            .is_some_and(|id| !id.is_null())
        {
            return Err(DriverError::Protocol(
                "unexpected native Claude subagent output".into(),
            ));
        }
        match value.get("type").and_then(Value::as_str) {
            Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
                let id = value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        DriverError::Protocol("Claude init missing session_id".into())
                    })?;
                if self
                    .session_id
                    .as_deref()
                    .is_some_and(|previous| previous != id)
                {
                    return Err(DriverError::Protocol(
                        "Claude changed session identity".into(),
                    ));
                }
                let tools = value
                    .get("tools")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        DriverError::Protocol("Claude init missing tool inventory".into())
                    })?;
                let names = tools
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .ok_or_else(|| DriverError::Protocol("invalid Claude tool name".into()))
                    })
                    .collect::<DriverResult<Vec<_>>>()?;
                let actual: BTreeSet<_> = names
                    .iter()
                    .filter(|n| n.starts_with("mcp__"))
                    .map(|n| n.to_string())
                    .collect();
                let valid_servers = value
                    .get("mcp_servers")
                    .and_then(Value::as_array)
                    .is_some_and(|servers| {
                        servers.len() == 1
                            && servers[0]["name"] == "hirsel"
                            && servers[0]["status"] == "connected"
                    });
                let no_plugins = value
                    .get("plugins")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                if actual != self.expected
                    || !valid_servers
                    || !no_plugins
                    || names
                        .iter()
                        .any(|name| config::FORBIDDEN_TOOLS.contains(name))
                {
                    return Err(DriverError::Protocol(
                        "Claude scoped tool inventory does not match host configuration".into(),
                    ));
                }
                if self.session_id.is_none() {
                    self.session_id = Some(id.into());
                    events.emit(SubagentEvent::Started {
                        external_id: id.into(),
                    })?;
                }
            }
            Some("assistant") => {
                self.check_session(value)?;
                let content = value.pointer("/message/content").and_then(Value::as_array);
                let text: String = content
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "text")
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                // Only a completed final block can become a failure's real output;
                // prior tool-planning commentary is never promoted on an error.
                self.assistant = (value
                    .pointer("/message/stop_reason")
                    .and_then(Value::as_str)
                    == Some("end_turn")
                    && !text.is_empty())
                .then_some(text);
                for summary in claude_assistant_progress(value) {
                    events.emit(SubagentEvent::Progress { summary })?;
                }
            }
            Some("stream_event") => {
                self.check_session(value)?;
                if let Some(text) = value.pointer("/event/delta/text").and_then(Value::as_str) {
                    events.emit(SubagentEvent::Progress {
                        summary: short_line(text),
                    })?;
                }
            }
            Some("user") => {
                if let Some(summary) = claude_tool_result(value) {
                    events.emit(SubagentEvent::Progress { summary })?;
                }
            }
            Some("result") => {
                self.check_session(value)?;
                let outcome = claude_terminal_outcome(value);
                let output = if matches!(outcome, TerminalOutcome::Done { .. }) {
                    value
                        .get("result")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                } else {
                    self.take_final()
                };
                events.complete(outcome, output)?;
            }
            Some("rate_limit_event") => events.emit(SubagentEvent::Progress {
                summary: "claude rate limit status updated".into(),
            })?,
            _ => {}
        }
        Ok(())
    }

    fn check_session(&self, value: &Value) -> DriverResult<()> {
        let Some(id) = self.session_id.as_deref() else {
            return Err(DriverError::Protocol(
                "Claude output before scoped initialization".into(),
            ));
        };
        if value.get("session_id").and_then(Value::as_str) != Some(id) {
            return Err(DriverError::Protocol(
                "Claude output belongs to another session".into(),
            ));
        }
        Ok(())
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
                Some("tool_use") => content
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| short_line(format!("tool {name}"))),
                _ => None,
            },
        )
        .collect()
}

fn claude_tool_result(value: &Value) -> Option<String> {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.iter().find(|block| block["type"] == "tool_result"))
        .map(|block| {
            short_line(if block["is_error"] == true {
                "tool failed"
            } else {
                "tool completed"
            })
        })
}

pub(crate) fn claude_terminal_outcome(value: &Value) -> TerminalOutcome {
    let subtype = value.get("subtype").and_then(Value::as_str);
    let Some(is_error) = value.get("is_error").and_then(Value::as_bool) else {
        return TerminalOutcome::Failed {
            reason: "malformed Claude result: missing is_error".into(),
        };
    };
    if !is_error && subtype.is_none_or(|s| s == "success") {
        return match value.get("result").and_then(Value::as_str) {
            Some(text) => TerminalOutcome::Done {
                summary: terminal_message(text),
            },
            None => TerminalOutcome::Failed {
                reason: "malformed Claude success: missing result text".into(),
            },
        };
    }
    if value.get("terminal_reason").and_then(Value::as_str) == Some("aborted_streaming") {
        return TerminalOutcome::Interrupted;
    }
    let details = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.is_empty())
        .or_else(|| {
            value
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Claude execution failed without error details".into());
    let reason = value
        .get("terminal_reason")
        .and_then(Value::as_str)
        .or_else(|| value.get("stop_reason").and_then(Value::as_str))
        .or(subtype)
        .map(|reason| format!("{reason}: {details}"))
        .unwrap_or(details);
    TerminalOutcome::Failed {
        reason: terminal_message(reason),
    }
}

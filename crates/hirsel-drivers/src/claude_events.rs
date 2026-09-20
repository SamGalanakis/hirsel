//! Claude's stream is translated into semantic deltas while only a completed final block is
//! eligible to become the terminal assistant output.
use std::collections::{BTreeMap, BTreeSet};

use super::*;

pub(super) struct ClaudeOutput {
    expected: BTreeSet<String>,
    session_id: Option<String>,
    assistant: Option<String>,
    started_tools: BTreeMap<String, String>,
    message_sequence: u64,
    message_active: bool,
    assistant_message_id: Option<String>,
    text_blocks: Vec<ClaudeTextBlock>,
    anonymous_block_sequence: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClaudeTextKind {
    Prose,
    Reasoning,
}

struct ClaudeTextBlock {
    kind: ClaudeTextKind,
    index: Option<u64>,
    text: String,
    block_id: Option<String>,
    completed: bool,
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
            started_tools: BTreeMap::new(),
            message_sequence: 0,
            message_active: false,
            assistant_message_id: None,
            text_blocks: Vec::new(),
            anonymous_block_sequence: 0,
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
        let kind = value.get("type").and_then(Value::as_str);
        if matches!(kind, Some("assistant" | "user" | "stream_event"))
            && value
                .get("parent_tool_use_id")
                .is_some_and(|id| !id.is_null())
        {
            return Err(DriverError::Protocol(
                "unexpected native Claude subagent output".into(),
            ));
        }
        match kind {
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
                self.reconcile_message_identity(
                    value
                        .pointer("/message/id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty()),
                );
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
                let api_block_index = value.get("apiBlockIndex").and_then(Value::as_u64);
                let content_len = content.map_or(0, Vec::len);
                for (index, block) in content.into_iter().flatten().enumerate() {
                    // Claude normally emits one assistant envelope per completed
                    // block. Only a multi-block envelope makes its array position
                    // an original content-block index; index zero in a singleton
                    // envelope is not provider identity.
                    let block_index =
                        api_block_index.or_else(|| (content_len > 1).then_some(index as u64));
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str)
                                && !text.is_empty()
                                && let Some((text, block_id)) = self.reconcile_envelope(
                                    ClaudeTextKind::Prose,
                                    block_index,
                                    text,
                                )
                            {
                                events.emit(SubagentEvent::ProseDelta { text, block_id })?;
                            }
                        }
                        Some("thinking") => {
                            if let Some(text) = block.get("thinking").and_then(Value::as_str)
                                && !text.is_empty()
                                && let Some((text, block_id)) = self.reconcile_envelope(
                                    ClaudeTextKind::Reasoning,
                                    block_index,
                                    text,
                                )
                            {
                                events.emit(SubagentEvent::ReasoningDelta { text, block_id })?;
                            }
                        }
                        Some("tool_use") => {
                            let Some(call_id) = block.get("id").and_then(Value::as_str) else {
                                continue;
                            };
                            let Some(name) = block.get("name").and_then(Value::as_str) else {
                                continue;
                            };
                            let args = block.get("input").cloned().unwrap_or_else(|| json!({}));
                            match self.started_tools.get(call_id) {
                                Some(previous) if previous != name => {
                                    return Err(DriverError::Protocol(
                                        "Claude reused a tool call id with another name".into(),
                                    ));
                                }
                                Some(_) => continue,
                                None => {}
                            }
                            self.started_tools.insert(call_id.into(), name.into());
                            events.emit(SubagentEvent::ToolStarted {
                                call_id: call_id.into(),
                                name: name.into(),
                                args,
                            })?;
                        }
                        _ => {}
                    }
                }
            }
            Some("stream_event") => {
                self.check_session(value)?;
                if value.pointer("/event/type").and_then(Value::as_str) == Some("message_start") {
                    self.start_message(
                        value
                            .pointer("/event/message/id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty()),
                    );
                }
                if let Some(text) = value.pointer("/event/delta/text").and_then(Value::as_str) {
                    let block_id = self.record_stream_delta(
                        ClaudeTextKind::Prose,
                        value.pointer("/event/index").and_then(Value::as_u64),
                        text,
                    );
                    events.emit(SubagentEvent::ProseDelta {
                        text: text.to_string(),
                        block_id,
                    })?;
                }
                if let Some(text) = value
                    .pointer("/event/delta/thinking")
                    .and_then(Value::as_str)
                {
                    let block_id = self.record_stream_delta(
                        ClaudeTextKind::Reasoning,
                        value.pointer("/event/index").and_then(Value::as_u64),
                        text,
                    );
                    events.emit(SubagentEvent::ReasoningDelta {
                        text: text.to_string(),
                        block_id,
                    })?;
                }
            }
            Some("user") => {
                for block in value
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|block| block["type"] == "tool_result")
                {
                    let Some(call_id) = block.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(name) = self.started_tools.remove(call_id) else {
                        events.emit(SubagentEvent::Progress {
                            summary: short_line(format!("tool result for unknown call {call_id}")),
                        })?;
                        continue;
                    };
                    events.emit(SubagentEvent::ToolCompleted {
                        call_id: call_id.into(),
                        name,
                        ok: block.get("is_error").and_then(Value::as_bool) != Some(true),
                        output: Value::String(bounded_tool_result(block.get("content"))),
                    })?;
                }
            }
            Some("tool_progress") => {
                self.check_session(value)?;
                let parent = value.get("parent_tool_use_id").and_then(Value::as_str);
                let elapsed = value.get("elapsed_time_seconds").and_then(Value::as_u64);
                let known_name = parent.and_then(|id| self.started_tools.get(id));
                let provider_name = value.get("tool_name").and_then(Value::as_str);
                let summary = match (known_name.map(String::as_str).or(provider_name), elapsed) {
                    (Some(name), Some(seconds)) if known_name.is_some() => {
                        format!("{name} running · {seconds}s")
                    }
                    (Some(name), None) if known_name.is_some() => format!("{name} running"),
                    (Some(name), Some(seconds)) => format!(
                        "{name} progress for unknown call {} · {seconds}s",
                        parent.unwrap_or("unknown")
                    ),
                    (Some(name), None) => format!(
                        "{name} progress for unknown call {}",
                        parent.unwrap_or("unknown")
                    ),
                    (None, Some(seconds)) => format!(
                        "tool progress for unknown call {} · {seconds}s",
                        parent.unwrap_or("unknown")
                    ),
                    (None, None) => format!(
                        "tool progress for unknown call {}",
                        parent.unwrap_or("unknown")
                    ),
                };
                events.emit(SubagentEvent::Progress {
                    summary: short_line(summary),
                })?;
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

    fn block_id(&self, index: u64) -> String {
        match self.assistant_message_id.as_deref() {
            Some(message_id) => format!("claude:{message_id}:{index}"),
            None => format!("claude:{}:{index}", self.message_sequence),
        }
    }

    fn start_message(&mut self, message_id: Option<&str>) {
        if self.message_active {
            self.message_sequence += 1;
        }
        self.message_active = true;
        self.assistant_message_id = message_id.map(str::to_owned);
        self.text_blocks.clear();
        self.anonymous_block_sequence = 0;
    }

    fn reconcile_message_identity(&mut self, message_id: Option<&str>) {
        self.message_active = true;
        let Some(message_id) = message_id else {
            return;
        };
        match self.assistant_message_id.as_deref() {
            None => self.assistant_message_id = Some(message_id.to_owned()),
            Some(current) if current == message_id => {}
            Some(_) => self.start_message(Some(message_id)),
        }
    }

    fn record_stream_delta(
        &mut self,
        kind: ClaudeTextKind,
        index: Option<u64>,
        text: &str,
    ) -> Option<String> {
        self.message_active = true;
        if let Some(index) = index {
            if let Some(block) =
                self.text_blocks.iter_mut().rev().find(|block| {
                    block.kind == kind && block.index == Some(index) && !block.completed
                })
            {
                block.text.push_str(text);
                return block.block_id.clone();
            }
            let block_id = Some(self.block_id(index));
            self.text_blocks.push(ClaudeTextBlock {
                kind,
                index: Some(index),
                text: text.to_owned(),
                block_id: block_id.clone(),
                completed: false,
            });
            return block_id;
        }

        // Without an index we cannot know whether adjacent deltas are chunks
        // or blocks. Preserve them verbatim as legacy no-id events and let a
        // later envelope consume the shortest matching prefix of this ledger.
        self.text_blocks.push(ClaudeTextBlock {
            kind,
            index: None,
            text: text.to_owned(),
            block_id: None,
            completed: false,
        });
        None
    }

    fn reconcile_envelope(
        &mut self,
        kind: ClaudeTextKind,
        index: Option<u64>,
        text: &str,
    ) -> Option<(String, Option<String>)> {
        if let Some(index) = index
            && let Some(position) = self.text_blocks.iter().position(|block| {
                block.kind == kind && block.index == Some(index) && text.starts_with(&block.text)
            })
        {
            return self.complete_block(position, text);
        }

        // An absent index is not index zero. Match only an oldest, contiguous
        // prefix of still-uncompleted streamed text. This handles both a block
        // split across chunks and legacy streams where every delta lacks an
        // index, without suppressing an unrelated envelope-only block.
        let pending = self
            .text_blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                block.kind == kind && !block.completed && (index.is_none() || block.index.is_none())
            })
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        let mut accumulated = String::new();
        let mut matched = Vec::new();
        for position in pending {
            accumulated.push_str(&self.text_blocks[position].text);
            if !text.starts_with(&accumulated) {
                matched.clear();
                break;
            }
            matched.push(position);
            if accumulated == text {
                break;
            }
        }
        if !matched.is_empty() && text.starts_with(&accumulated) {
            let block_id = matched
                .iter()
                .map(|position| self.text_blocks[*position].block_id.as_deref())
                .reduce(|left, right| if left == right { left } else { None })
                .flatten()
                .map(str::to_owned);
            for position in matched {
                self.text_blocks[position].completed = true;
            }
            let suffix = &text[accumulated.len()..];
            return (!suffix.is_empty()).then(|| (suffix.to_owned(), block_id));
        }

        let block_id = match index {
            Some(index) => Some(self.block_id(index)),
            None => {
                let sequence = self.anonymous_block_sequence;
                self.anonymous_block_sequence += 1;
                Some(match self.assistant_message_id.as_deref() {
                    Some(message_id) => format!("claude:{message_id}:anonymous-{sequence}"),
                    None => format!("claude:{}:anonymous-{sequence}", self.message_sequence),
                })
            }
        };
        self.text_blocks.push(ClaudeTextBlock {
            kind,
            index,
            text: text.to_owned(),
            block_id: block_id.clone(),
            completed: true,
        });
        Some((text.to_owned(), block_id))
    }

    fn complete_block(&mut self, position: usize, text: &str) -> Option<(String, Option<String>)> {
        let block = &mut self.text_blocks[position];
        block.completed = true;
        let suffix = text.strip_prefix(&block.text).unwrap_or(text).to_owned();
        block.text = text.to_owned();
        (!suffix.is_empty()).then(|| (suffix, block.block_id.clone()))
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

fn bounded_tool_result(content: Option<&Value>) -> String {
    let text = match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => value.to_string(),
        None => String::new(),
    };
    const LIMIT: usize = 4096;
    const MARKER: &str = "…[truncated]";
    if text.len() <= LIMIT {
        return text;
    }
    let mut end = LIMIT.saturating_sub(MARKER.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = text[..end].to_string();
    bounded.push_str(MARKER);
    bounded
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

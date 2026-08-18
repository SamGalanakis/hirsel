use super::*;

pub(super) fn condense_args(name: &str, payload: &Value) -> Option<String> {
    let summary = match name {
        "shell_run" => labeled_scalar(payload, "cmd", "cmd"),
        "events_judgment" => labeled_scalar(payload, "question", "question"),
        "events_notify" | "events_summary" => {
            labeled_first_scalar(payload, &["description", "name"], "event")
        }
        "events_archive" => scalar_field(payload, "event_id").map(|id| format!("event {id}")),
        "events_clear" => None,
        "pings_send" => {
            labeled_first_scalar(payload, &["content_md", "content", "body"], "content")
        }
        "pings_resolve" => scalar_any(payload, &["ping_id", "id"]).map(|id| format!("ping {id}")),
        "views_show" => scalar_any(payload, &["template_id", "instance_id"])
            .map(|value| format!("view {value}")),
        "views_update" | "views_clear" => {
            scalar_field(payload, "instance_id").map(|id| format!("view {}", tail_identifier(&id)))
        }
        "views_list_templates" => None,
        "subagents_spawn" => {
            let agent = scalar_field(payload, "agent").unwrap_or_else(|| "subagent".to_string());
            scalar_any(payload, &["prompt", "task"]).map(|prompt| format!("{agent}: {prompt}"))
        }
        "subagents_prompt" => process_summary(payload).map(|process| {
            match scalar_any(payload, &["text", "prompt", "message"]) {
                Some(text) => format!("{process}: {text}"),
                None => process,
            }
        }),
        "subagents_interrupt" | "subagents_progress" | "subagents_wait" => process_summary(payload),
        "monitors_create" => labeled_first_scalar(payload, &["label", "cmd"], "monitor"),
        "monitors_cancel" => scalar_any(payload, &["monitor_id", "process_id", "id"])
            .map(|id| format!("monitor {}", tail_identifier(&id))),
        "monitors_list" | "subagents_list" => None,
        _ => first_string_field(payload).map(|(key, value)| format!("{key}: {value}")),
    };
    clean_summary(summary)
}

pub(super) fn condense_result(name: &str, args: &Value, output: &Value) -> Option<String> {
    let ok = tool_output_ok(output);
    let prefix = if ok { "ok" } else { "err" };
    let payload = tool_output_payload(output).unwrap_or(output);
    let detail = match name {
        "shell_run" => shell_result_summary(payload),
        "events_judgment" | "events_notify" | "events_summary" | "events_archive" => {
            scalar_field(payload, "event_id").map(|id| format!("event {id}"))
        }
        "events_clear" => scalar_field(payload, "count").map(|count| format!("{count} archived")),
        "pings_send" => scalar_field(payload, "ping_id").map(|id| format!("ping {id}")),
        "pings_resolve" => payload
            .get("ping")
            .and_then(|ping| scalar_field(ping, "ping_id"))
            .or_else(|| scalar_any(args, &["ping_id", "id"]))
            .map(|id| format!("ping {id}")),
        "views_show" | "views_update" | "views_clear" => scalar_field(payload, "instance_id")
            .or_else(|| scalar_field(args, "instance_id"))
            .map(|id| format!("view {}", tail_identifier(&id))),
        "views_list_templates" => payload
            .as_array()
            .map(|templates| format!("{} templates", templates.len())),
        "subagents_spawn" => scalar_any(payload, &["process_id"])
            .or_else(|| {
                payload
                    .get("handle")
                    .and_then(|handle| scalar_field(handle, "process_id"))
            })
            .map(|id| format!("process {}", tail_identifier(&id))),
        "subagents_prompt" => process_summary(args),
        "subagents_interrupt" => process_summary(args),
        "subagents_progress" => process_summary(args),
        "subagents_wait" => scalar_any(payload, &["process_id"])
            .or_else(|| scalar_any(args, &["process_id"]))
            .map(|id| format!("process {}", tail_identifier(&id))),
        "monitors_create" => scalar_any(payload, &["monitor_id", "process_id"])
            .map(|id| format!("monitor {}", tail_identifier(&id))),
        "monitors_cancel" => scalar_any(payload, &["monitor_id"])
            .or_else(|| scalar_any(args, &["monitor_id", "process_id", "id"]))
            .map(|id| format!("monitor {}", tail_identifier(&id))),
        "monitors_list" => {
            scalar_count(payload, "monitors").map(|count| format!("{count} monitors"))
        }
        "subagents_list" => {
            scalar_count(payload, "processes").map(|count| format!("{count} processes"))
        }
        _ => first_scalar_field(payload).map(|(_, value)| value),
    }
    .or_else(|| failure_message(output));

    clean_summary(Some(match detail {
        Some(detail) if !detail.trim().is_empty() => format!("{prefix} {detail}"),
        _ => prefix.to_string(),
    }))
}

pub(super) fn tool_output_ok(output: &Value) -> bool {
    output
        .pointer("/outcome/status")
        .and_then(Value::as_str)
        .map(|status| status == "success")
        .or_else(|| {
            output
                .get("status")
                .and_then(Value::as_str)
                .map(|status| status == "success" || status == "ok")
        })
        .or_else(|| output.get("ok").and_then(Value::as_bool))
        .unwrap_or(false)
}

pub(super) fn tool_output_payload(output: &Value) -> Option<&Value> {
    output.pointer("/outcome/payload")
}

pub(super) fn shell_result_summary(payload: &Value) -> Option<String> {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("timed out".to_string());
    }
    scalar_field(payload, "status")
        .map(|status| format!("status {status}"))
        .or_else(|| {
            first_non_empty_string(payload, &["stderr", "stdout"])
                .map(|text| format!("output {text}"))
        })
}

pub(super) fn failure_message(output: &Value) -> Option<String> {
    output
        .pointer("/outcome/payload/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            output
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

pub(super) fn scalar_count(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.len().to_string())
}

pub(super) fn process_summary(value: &Value) -> Option<String> {
    scalar_any(value, &["process_id", "id"]).map(|id| format!("process {}", tail_identifier(&id)))
}

pub(super) fn labeled_scalar(value: &Value, key: &str, label: &str) -> Option<String> {
    scalar_field(value, key).map(|text| format!("{label}: {text}"))
}

pub(super) fn labeled_first_scalar(value: &Value, keys: &[&str], label: &str) -> Option<String> {
    scalar_any(value, keys).map(|text| format!("{label}: {text}"))
}

pub(super) fn scalar_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| scalar_field(value, key))
}

pub(super) fn scalar_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(scalar_value)
}

pub(super) fn scalar_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn first_non_empty_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .find_map(latest_line)
}

pub(super) fn first_string_field(value: &Value) -> Option<(String, String)> {
    value.as_object()?.iter().find_map(|(key, value)| {
        value
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(|text| (key.clone(), text.to_string()))
    })
}

pub(super) fn first_scalar_field(value: &Value) -> Option<(String, String)> {
    value.as_object()?.iter().find_map(|(key, value)| {
        scalar_value(value)
            .filter(|text| !text.trim().is_empty())
            .map(|text| (key.clone(), text))
    })
}

pub(super) fn tail_identifier(value: &str) -> String {
    const MAX_ID_CHARS: usize = 24;
    if value.chars().count() <= MAX_ID_CHARS {
        return value.to_string();
    }
    let tail = value
        .chars()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}

/// Agent code is rendered as source, so it is passed through verbatim; only a
/// pathological cell is clipped, and then the flag says so.
pub(super) fn clamp_code(code: &str) -> (String, bool) {
    if code.len() <= TURN_EVENT_CODE_BYTES {
        return (code.to_string(), false);
    }
    let mut end = TURN_EVENT_CODE_BYTES;
    while end > 0 && !code.is_char_boundary(end) {
        end -= 1;
    }
    (code[..end].to_string(), true)
}

/// The condensed counterpart to `clamp_code`: a cell's outcome is a one-liner
/// (duration, plus the failure's first line), never its output.
pub(super) fn code_done_summary(
    success: bool,
    error: Option<&str>,
    duration_ms: u64,
) -> Option<String> {
    let duration = format!("{duration_ms}ms");
    let detail = match (success, error.and_then(first_line)) {
        (true, _) => duration,
        (false, Some(message)) => format!("{duration} {message}"),
        (false, None) => format!("{duration} failed"),
    };
    clean_summary(Some(detail))
}

pub(super) fn clean_summary(summary: Option<String>) -> Option<String> {
    let without_braces = summary?
        .chars()
        .map(|ch| match ch {
            '{' | '}' => ' ',
            _ => ch,
        })
        .collect::<String>();
    let compact = without_braces
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        None
    } else {
        Some(truncate_chars(&compact, TURN_EVENT_SUMMARY_CHARS))
    }
}

pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut truncated = text.chars().take(max_chars - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

pub(super) fn agent_activity(state: AgentActivityState, text: Option<String>) -> HostToClient {
    HostToClient::AgentActivity {
        state,
        text,
        sc: None,
    }
}

pub(super) fn publish(
    broadcast_log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
    event: HostToClient,
) {
    broadcast_log.record(event.clone());
    let _ = broadcaster.send(event);
}

pub(super) fn latest_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(clip_line)
}

/// A failure's headline: the first non-empty line, which is where the message
/// lives (trailing lines are usually stack or context).
pub(super) fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(clip_line)
}

pub(super) fn clip_line(line: &str) -> String {
    const MAX_CHARS: usize = 180;
    if line.chars().count() <= MAX_CHARS {
        line.to_string()
    } else {
        let mut truncated = line.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

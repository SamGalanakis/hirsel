use super::*;

pub(super) fn empty_object_input_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{}})
}

pub(super) fn view_instance_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["instance_id"],
        "properties": {
            "instance_id": { "type": "string", "minLength": 1 }
        }
    })
}

pub(super) fn view_clear_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok", "instance_id"],
        "properties": {
            "ok": { "const": true },
            "instance_id": { "type": "string", "minLength": 1 }
        }
    })
}

pub(super) fn views_list_templates_output_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "title"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "title": { "type": "string", "minLength": 1 }
            }
        }
    })
}

pub(super) fn subagents_spawn_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["process_id"],
        "properties": {
            "process_id": { "type": "string", "minLength": 1 }
        }
    })
}

pub(super) fn acknowledgement_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok"],
        "properties": {
            "ok": { "const": true }
        }
    })
}

pub(super) fn subagents_list_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["processes"],
        "properties": {
            "processes": {
                "type": "array",
                "items": subagent_process_output_schema()
            }
        }
    })
}

pub(super) fn subagents_progress_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["process", "events"],
        "properties": {
            "process": {
                "oneOf": [
                    subagent_process_output_schema(),
                    { "type": "null" }
                ]
            },
            "events": {
                "type": "array",
                "items": subagent_event_output_schema()
            }
        }
    })
}

pub(super) fn subagent_process_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "process_id",
            "agent",
            "handle",
            "prompt",
            "cwd",
            "external_id",
            "status",
            "events",
            "started_ts",
            "last_event_ts"
        ],
        "properties": {
            "process_id": { "type": "string", "minLength": 1 },
            "agent": { "type": "string", "enum": ["claude", "codex"] },
            "model": { "type": "string" },
            "handle": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "agent"],
                "properties": {
                    "id": { "type": "string", "minLength": 1 },
                    "agent": { "type": "string", "enum": ["claude", "codex"] }
                }
            },
            "prompt": { "type": "string" },
            "cwd": { "type": "string" },
            "external_id": { "type": ["string", "null"] },
            "status": {
                "type": "string",
                "enum": ["running", "done", "failed", "interrupted", "abandoned"]
            },
            "events": {
                "type": "array",
                "items": subagent_event_output_schema()
            },
            "started_ts": timestamp_output_schema(),
            "last_event_ts": timestamp_output_schema()
        }
    })
}

pub(super) fn subagent_event_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "external_id"],
                "properties": {
                    "type": { "const": "started" },
                    "external_id": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "summary"],
                "properties": {
                    "type": { "const": "progress" },
                    "summary": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "outcome"],
                "properties": {
                    "type": { "const": "terminal" },
                    "outcome": terminal_outcome_output_schema()
                }
            }
        ]
    })
}

pub(super) fn terminal_outcome_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["status", "summary"],
                "properties": {
                    "status": { "const": "done" },
                    "summary": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["status", "reason"],
                "properties": {
                    "status": { "const": "failed" },
                    "reason": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["status"],
                "properties": {
                    "status": { "const": "interrupted" }
                }
            }
        ]
    })
}

pub(super) fn subagents_wait_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["process_id", "outcome"],
        "properties": {
            "process_id": { "type": "string", "minLength": 1 },
            "outcome": process_await_output_schema()
        }
    })
}

pub(super) fn process_await_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "terminal_label", "pruned_at_ms"],
                "properties": {
                    "type": { "const": "no_longer_retained" },
                    "terminal_label": { "type": "string" },
                    "pruned_at_ms": { "type": "integer", "minimum": 0 }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "value"],
                "properties": {
                    "type": { "const": "success" },
                    "value": true
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "class", "code", "message"],
                "properties": {
                    "type": { "const": "failure" },
                    "class": {
                        "type": "string",
                        "enum": [
                            "invalid_request",
                            "io",
                            "unavailable",
                            "permission_denied",
                            "timeout",
                            "execution",
                            "external",
                            "resource_limit",
                            "internal"
                        ]
                    },
                    "code": { "type": "string" },
                    "message": { "type": "string" },
                    "raw": true
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "message"],
                "properties": {
                    "type": { "const": "cancelled" },
                    "message": { "type": "string" },
                    "raw": true
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "evidence"],
                "properties": {
                    "type": { "const": "abandoned" },
                    "evidence": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["writer", "epoch_ms"],
                        "properties": {
                            "writer": {
                                "type": "string",
                                "enum": ["owner_drain", "sweep", "reconciled_request", "engine_gave_up"]
                            },
                            "epoch_ms": { "type": "integer", "minimum": 0 }
                        }
                    }
                }
            }
        ]
    })
}

pub(super) fn monitors_create_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["monitor_id", "monitor"],
        "properties": {
            "monitor_id": { "type": "string", "minLength": 1 },
            "monitor": monitor_output_schema()
        }
    })
}

pub(super) fn monitors_list_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["monitors"],
        "properties": {
            "monitors": {
                "type": "array",
                "items": monitor_output_schema()
            }
        }
    })
}

pub(super) fn monitor_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "monitor_id",
            "cmd",
            "every_secs",
            "wake_on",
            "label",
            "created_ts",
            "last_event_ts"
        ],
        "properties": {
            "monitor_id": { "type": "string", "minLength": 1 },
            "cmd": { "type": "string" },
            "every_secs": { "type": "integer", "minimum": 30 },
            "wake_on": {
                "type": "string",
                "enum": ["changed", "exit_zero", "exit_nonzero", "regex"]
            },
            "pattern": { "type": "string" },
            "label": { "type": "string" },
            "created_ts": timestamp_output_schema(),
            "last_event_ts": timestamp_output_schema(),
            "last_run_ts": timestamp_output_schema(),
            "last_output": { "type": "string" },
            "summary": { "type": "string" },
            "cancelled_ts": timestamp_output_schema()
        }
    })
}

pub(super) fn monitors_cancel_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok", "monitor_id"],
        "properties": {
            "ok": { "const": true },
            "monitor_id": { "type": "string", "minLength": 1 }
        }
    })
}

pub(super) fn shell_run_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "stdout", "stderr", "timed_out"],
        "properties": {
            "status": { "type": ["integer", "null"] },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "timed_out": { "type": "boolean" }
        }
    })
}

pub(super) fn timestamp_output_schema() -> Value {
    json!({ "type": "string", "format": "date-time" })
}

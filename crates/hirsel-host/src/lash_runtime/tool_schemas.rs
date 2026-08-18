use super::*;

pub(super) fn events_judgment_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["question", "options"],
        "properties": {
            "question": {
                "type": "string",
                "minLength": 1,
                "description": "The one-line decision question shown as the judgment heading."
            },
            "context": {
                "type": "string",
                "description": "Optional stakes or constraints that add information beyond the question. Omit it instead of paraphrasing the question."
            },
            "options": {
                "type": "array",
                "minItems": 2,
                "maxItems": 4,
                "description": "Two to four real choices with tradeoff details. Mark one recommended; if none is marked, the first is recommended.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["label", "detail"],
                    "properties": {
                        "label": { "type": "string", "minLength": 1 },
                        "detail": { "type": "string", "minLength": 1 },
                        "recommended": { "type": "boolean", "default": false },
                        "key": {
                            "type": "string",
                            "pattern": "^[A-Z]$",
                            "description": "Optional presentation key. Omitted keys are assigned A, B, C… by position."
                        }
                    }
                }
            },
            "unblocks": {
                "type": "integer",
                "minimum": 0,
                "description": "Optional count of agents this decision unblocks."
            },
            "view": {
                "type": ["object", "array", "null"],
                "description": "Optional accompanying constrained view embedded in the blessed card."
            }
        }
    })
}

pub(super) fn empty_object_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false
    })
}

pub(super) fn event_archive_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event_id"],
        "properties": {
            "event_id": { "type": "integer", "minimum": 1 }
        }
    })
}

pub(super) fn events_notify_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "description"],
        "properties": {
            "name": { "type": "string", "minLength": 1, "maxLength": 32 },
            "description": {
                "type": "string",
                "minLength": 1,
                "description": "One-line notification text."
            },
            "content_md": {
                "type": "string",
                "description": "Optional supporting markdown; the description is used when omitted."
            }
        }
    })
}

pub(super) fn events_summary_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "description"],
        "properties": {
            "name": { "type": "string", "minLength": 1, "maxLength": 32 },
            "description": {
                "type": "string",
                "minLength": 1,
                "description": "One-line digest outcome."
            },
            "content_md": {
                "type": "string",
                "minLength": 1,
                "description": "Digest markdown used to build the standard summary card."
            },
            "ui": {
                "type": "object",
                "description": "A constrained-JSON UI component tree validated by the host."
            }
        },
        "oneOf": [
            { "required": ["content_md"], "not": { "required": ["ui"] } },
            { "required": ["ui"], "not": { "required": ["content_md"] } }
        ]
    })
}

pub(super) fn events_recompose_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event_id", "ui"],
        "properties": {
            "event_id": {
                "type": "integer",
                "minimum": 1,
                "description": "Exact Task id supplied in the active generated-action context."
            },
            "description": {
                "type": "string",
                "minLength": 1,
                "description": "Optional updated one-line Task description."
            },
            "ui": {
                "oneOf": [
                    { "type": "object" },
                    { "type": "array", "minItems": 1 }
                ],
                "description": "Complete next constrained-JSON Task instrument."
            }
        }
    })
}

pub(super) fn events_recompose_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event_id", "status"],
        "properties": {
            "event_id": { "type": "integer", "minimum": 1 },
            "status": { "const": "open" }
        }
    })
}

pub(super) fn event_send_output_schema(kind: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event_id", "anchor", "kind"],
        "properties": {
            "event_id": { "type": "integer", "minimum": 1 },
            "anchor": { "type": "integer", "minimum": 1 },
            "kind": { "const": kind }
        }
    })
}

pub(super) fn event_archive_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event_id", "status", "archived"],
        "properties": {
            "event_id": { "type": "integer", "minimum": 1 },
            "status": { "const": "done" },
            "archived": { "const": true }
        }
    })
}

pub(super) fn events_clear_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["count"],
        "properties": {
            "count": { "type": "integer", "minimum": 0 }
        }
    })
}

pub(super) fn pings_send_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ping_id", "anchor", "requires_response"],
        "properties": {
            "ping_id": { "type": "integer", "minimum": 1 },
            "anchor": { "type": "integer", "minimum": 1 },
            "requires_response": { "type": "boolean" }
        }
    })
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

pub(super) fn pings_resolve_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ping"],
        "properties": {
            "ping": {
                "oneOf": [
                    ping_output_schema(),
                    { "type": "null" }
                ]
            }
        }
    })
}

pub(super) fn ping_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "ping_id",
            "kind",
            "source",
            "name",
            "description",
            "ui",
            "anchor",
            "requires_response",
            "quick_replies",
            "status",
            "read",
            "archived",
            "snoozed_until",
            "archived_at",
            "fork_sc",
            "ts"
        ],
        "properties": {
            "ping_id": { "type": "integer", "minimum": 1 },
            "kind": { "type": "string", "enum": ["judgment", "summary", "info"] },
            "source": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "ref"],
                "properties": {
                    "kind": { "type": "string", "enum": ["agent", "subagent", "scheduled", "monitor"] },
                    "ref": { "type": ["string", "null"] }
                }
            },
            "name": { "type": "string", "minLength": 1, "maxLength": 32 },
            "description": { "type": "string", "minLength": 1 },
            "ui": {},
            "anchor": { "type": "integer", "minimum": 1 },
            "requires_response": { "type": "boolean" },
            "quick_replies": {
                "type": "array",
                "items": quick_reply_output_schema()
            },
            "status": { "type": "string", "enum": ["open", "done"] },
            "read": { "type": "boolean" },
            "archived": { "type": "boolean" },
            "snoozed_until": { "oneOf": [timestamp_output_schema(), { "type": "null" }] },
            "archived_at": { "oneOf": [timestamp_output_schema(), { "type": "null" }] },
            "fork_sc": { "type": ["string", "null"] },
            "ts": timestamp_output_schema()
        }
    })
}

pub(super) fn quick_reply_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["value", "label"],
        "properties": {
            "value": { "type": "string" },
            "label": { "type": "string" }
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
                                "enum": ["owner_drain", "sweep", "reconciled_request"]
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

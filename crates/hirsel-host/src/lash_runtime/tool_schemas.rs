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

pub(super) fn monitors_create_input_schema() -> Value {
    json!({
        "type": "object",
        "oneOf": [
            monitor_create_input_variant("changed", false),
            monitor_create_input_variant("exit_zero", false),
            monitor_create_input_variant("exit_nonzero", false),
            monitor_create_input_variant("regex", true)
        ]
    })
}

fn monitor_create_input_variant(wake_on: &str, regex: bool) -> Value {
    let mut required = vec!["cmd", "wake_on", "label"];
    let mut properties = json!({
        "cmd": { "type": "string", "minLength": 1 },
        "every_secs": { "type": "integer", "minimum": 30 },
        "wake_on": { "const": wake_on },
        "label": { "type": "string", "minLength": 1 }
    });
    if regex {
        required.push("pattern");
        properties["pattern"] = json!({
            "type": "string",
            "minLength": 1,
            "description": "A valid regular expression matched against command output."
        });
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties
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
        "oneOf": [
            monitor_output_variant("changed", false),
            monitor_output_variant("exit_zero", false),
            monitor_output_variant("exit_nonzero", false),
            monitor_output_variant("regex", true)
        ]
    })
}

fn monitor_output_variant(wake_on: &str, regex: bool) -> Value {
    let mut required = vec![
        "monitor_id",
        "thread_id",
        "cmd",
        "every_secs",
        "wake_on",
        "label",
        "created_ts",
        "last_event_ts",
    ];
    let mut properties = json!({
        "monitor_id": { "type": "string", "minLength": 1 },
        "thread_id": { "type": "integer", "minimum": 0 },
        "cmd": { "type": "string" },
        "every_secs": { "type": "integer", "minimum": 30 },
        "wake_on": { "const": wake_on },
        "label": { "type": "string" },
        "created_ts": timestamp_output_schema(),
        "last_event_ts": timestamp_output_schema(),
        "last_run_ts": timestamp_output_schema(),
        "last_output": { "type": "string" },
        "summary": { "type": "string" },
        "cancelled_ts": timestamp_output_schema()
    });
    if regex {
        required.push("pattern");
        properties["pattern"] = json!({ "type": "string", "minLength": 1 });
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties
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

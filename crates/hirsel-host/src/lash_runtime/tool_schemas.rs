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

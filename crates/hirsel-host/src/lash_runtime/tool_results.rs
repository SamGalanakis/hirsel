use super::*;

pub(super) fn view_instance_result(view: &hirsel_proto::ViewInstance) -> Value {
    json!({ "instance_id": view.instance_id })
}

pub(super) fn subagent_spawn_result(process_id: &str) -> Value {
    json!({ "process_id": process_id })
}

pub(super) fn acknowledgement_result() -> Value {
    json!({ "ok": true })
}

pub(super) fn subagents_list_result(
    processes: &[crate::processes::ProcessRecord],
) -> Result<Value, String> {
    let processes = processes
        .iter()
        .map(subagent_process_result)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "processes": processes }))
}

pub(super) fn subagents_progress_result(
    process: Option<&crate::processes::ProcessRecord>,
    events: &[hirsel_drivers::SubagentEvent],
) -> Result<Value, String> {
    let process = process.map(subagent_process_result).transpose()?;
    Ok(json!({
        "process": process,
        "events": events,
    }))
}

pub(super) fn subagent_process_result(
    process: &crate::processes::ProcessRecord,
) -> Result<Value, String> {
    let mut value = serde_json::to_value(process).map_err(|error| error.to_string())?;
    rename_result_id(&mut value, "process_id")?;
    Ok(value)
}

pub(super) fn subagents_wait_result(
    process_id: &str,
    outcome: &ProcessAwaitOutput,
) -> Result<Value, String> {
    // Hirsel's tool contract is plain JSON; Lash's durable representation
    // carries trust envelopes and control metadata that are not model output.
    let outcome = match outcome {
        ProcessAwaitOutput::Settled { output } => match &output.outcome {
            lash_core::ToolCallOutcome::Success(value) => json!({
                "type": "success", "value": value.to_json_value(),
            }),
            lash_core::ToolCallOutcome::Failure(failure) => json!({
                "type": "failure", "class": failure.class, "code": failure.code,
                "message": failure.message, "raw": failure.raw.as_ref().map(lash_core::ToolValue::to_json_value),
            }),
            lash_core::ToolCallOutcome::Cancelled(cancellation) => json!({
                "type": "cancelled", "message": cancellation.message,
                "raw": cancellation.raw.as_ref().map(lash_core::ToolValue::to_json_value),
            }),
        },
        ProcessAwaitOutput::Abandoned { evidence, .. } => json!({
            "type": "abandoned", "evidence": { "writer": evidence.writer, "epoch_ms": evidence.epoch_ms },
        }),
        ProcessAwaitOutput::NoLongerRetained {
            terminal_label,
            pruned_at_ms,
        } => json!({
            "type": "no_longer_retained", "terminal_label": terminal_label, "pruned_at_ms": pruned_at_ms,
        }),
    };
    Ok(json!({ "process_id": process_id, "outcome": outcome }))
}

pub(super) fn shell_run_result(output: &crate::tools::ShellRunOutput) -> Result<Value, String> {
    serde_json::to_value(output).map_err(|error| error.to_string())
}

pub(super) fn monitors_create_result(record: &MonitorRecord) -> Result<Value, String> {
    Ok(json!({
        "monitor_id": record.id,
        "monitor": monitor_result(record)?,
    }))
}

pub(super) fn monitors_list_result(monitors: &[MonitorRecord]) -> Result<Value, String> {
    let monitors = monitors
        .iter()
        .map(monitor_result)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "monitors": monitors }))
}

pub(super) fn monitor_result(record: &MonitorRecord) -> Result<Value, String> {
    let mut value = serde_json::to_value(record).map_err(|error| error.to_string())?;
    rename_result_id(&mut value, "monitor_id")?;
    Ok(value)
}

pub(super) fn monitors_cancel_result(monitor_id: &str) -> Value {
    json!({ "ok": true, "monitor_id": monitor_id })
}

pub(super) fn rename_result_id(value: &mut Value, result_name: &str) -> Result<(), String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "tool result record must serialize as an object".to_string())?;
    let id = object
        .remove("id")
        .ok_or_else(|| "tool result record is missing its id".to_string())?;
    object.insert(result_name.to_string(), id);
    Ok(())
}

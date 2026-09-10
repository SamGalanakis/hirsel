use super::*;

pub(super) fn view_instance_result(view: &hirsel_proto::ViewInstance) -> Value {
    json!({ "instance_id": view.instance_id })
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

use super::*;

pub(super) fn view_instance_result(view: &hirsel_proto::ViewInstance) -> Value {
    json!({ "instance_id": view.instance_id })
}

pub(super) fn shell_run_result(output: &crate::tools::ShellRunOutput) -> Result<Value, String> {
    serde_json::to_value(output).map_err(|error| error.to_string())
}

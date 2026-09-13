use super::*;

pub(super) fn view_instance_result(view: &hirsel_proto::ViewInstance) -> Value {
    json!({ "instance_id": view.instance_id })
}

pub(super) fn shell_run_result(output: &crate::tools::ShellRunOutput) -> Result<Value, ToolError> {
    Ok(serde_json::to_value(output)?)
}

/// A tool call fails in one of two ways: the call was wrong, which is a
/// transport error the model must fix, or the call was well formed and
/// addressed something outside this Thread's reach, which is a *result* — a
/// typed refusal the model can read, log and ask its requester to widen.
#[derive(Debug)]
pub(crate) enum ToolError {
    Message(String),
    Refused(crate::storage::OutsideGrant),
}
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Refused(refusal) => write!(f, "{refusal}"),
        }
    }
}
impl From<String> for ToolError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}
impl From<&str> for ToolError {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}
impl From<serde_json::Error> for ToolError {
    fn from(value: serde_json::Error) -> Self {
        Self::Message(value.to_string())
    }
}
impl From<anyhow::Error> for ToolError {
    fn from(value: anyhow::Error) -> Self {
        match value.downcast::<crate::storage::OutsideGrant>() {
            Ok(refusal) => Self::Refused(refusal),
            Err(other) => Self::Message(other.to_string()),
        }
    }
}

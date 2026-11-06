//! Execution is imported by the source acceptance join for both fresh and replay consumers.
use super::TirExecution;
use hirsel_engine::{FrontLowerQueries, FrontRunOutput, ToolError};
use std::sync::Arc;
pub(super) fn materialize(
    facts: Arc<FrontRunOutput>,
    _lower: Arc<FrontLowerQueries>,
) -> Result<Arc<TirExecution>, ToolError> {
    facts
        .accepted_execution()
        .ok_or_else(|| ToolError::Runtime("accepted execution product was not published".into()))
}

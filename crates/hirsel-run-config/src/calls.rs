//! Call record validation.
use crate::invalid;
use hirsel_run_schema::{CallForm, CallRecord, RunRefusal};
pub(super) fn validate(call: &CallRecord) -> Result<(), RunRefusal> {
    if !matches!(
        CallForm::parse(&call.form),
        Some(CallForm::Compute | CallForm::Call)
    ) {
        return Err(invalid("call.form", "expected compute or call"));
    }
    call.cause()?;
    call.argument()?;
    Ok(())
}

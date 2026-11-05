//! Ordinary addressed dispatch recipes. Reopening conveys no C authority.
use super::*;
use hirsel_engine::store::{Domain, ResultStore};
use serde::{Deserialize, Serialize};

const PLAN: Domain = Domain {
    kind: 0x4e00_0035,
    schema: 1,
};
pub(crate) fn put<T: Serialize>(store: &dyn ResultStore, value: &T) -> Result<Reference, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let expected = PLAN
        .reference(&bytes)
        .ok_or("dispatch recipe exceeds record bound")?;
    let record = store
        .put_content(PLAN, &bytes)
        .ok_or("dispatch recipe publication failed")?;
    if record != expected {
        return Err("dispatch recipe identity differs".into());
    }
    Ok(record)
}
pub(crate) fn read<T: for<'de> Deserialize<'de>>(
    store: &dyn ResultStore,
    record: Reference,
) -> Result<T, String> {
    let bytes = store.get_content(record).ok_or("missing dispatch recipe")?;
    if record.domain != PLAN || PLAN.reference(&bytes) != Some(record) {
        return Err("dispatch recipe identity differs".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

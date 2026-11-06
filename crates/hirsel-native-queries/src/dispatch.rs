//! Ordinary addressed dispatch recipes. Reopening conveys no C authority.
use super::*;
use hirsel_engine_store::{Domain, ResultStore};
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

#[derive(Serialize, Deserialize)]
pub(crate) struct AddressRecord {
    definition: hirsel_syntax::DefPath,
    body: u32,
    encoding: Vec<u8>,
    target: ContentHash,
    policy: ContentHash,
}
impl AddressRecord {
    pub(crate) fn capture(key: &InstanceAddress) -> Self {
        Self {
            definition: key.instance.definition().clone(),
            body: key.instance.body(),
            encoding: key.instance.encoding().to_vec(),
            target: key.target_abi,
            policy: key.policy,
        }
    }
    pub(crate) fn bind(self) -> Result<InstanceAddress, String> {
        Ok(InstanceAddress {
            instance: hirsel_layout::instance::InstanceKey::from_encoding(
                &self.definition,
                self.body,
                self.encoding,
            )
            .map_err(|e| format!("dispatch owner encoding: {e:?}"))?,
            target_abi: self.target,
            policy: self.policy,
        })
    }
}

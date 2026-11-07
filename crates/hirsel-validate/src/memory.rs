//! Allocation ownership for admitted, immutable validation inputs.
use crate::{ViewRefusal, Violation, ViolationKind};
use std::sync::{Arc, Mutex};

/// The host's existing working-memory allowance and signature allocation visitor.
pub trait ValidationMemory: Send + Sync {
    fn retain(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, ViewRefusal>;
    fn signature_bytes(&self, signature: &hirsel_types::SignatureRef) -> u64;
}

#[derive(Default)]
pub(crate) struct ControlRetention(Mutex<Option<Arc<dyn Send + Sync>>>);
impl Clone for ControlRetention {
    fn clone(&self) -> Self {
        Self::default()
    }
}
impl std::fmt::Debug for ControlRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControlRetention")
    }
}
impl PartialEq for ControlRetention {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for ControlRetention {}
impl ControlRetention {
    pub(crate) fn retain(
        &self,
        memory: &dyn ValidationMemory,
        bytes: impl FnOnce() -> Option<u64>,
    ) -> Result<(), ViewRefusal> {
        let mut retained = self.0.lock().unwrap();
        if retained.is_none() {
            *retained = Some(memory.retain(bytes().ok_or_else(overflow)?)?);
        }
        Ok(())
    }
    pub(crate) fn set(&self, owner: Arc<dyn Send + Sync>) {
        self.0.lock().unwrap().get_or_insert(owner);
    }
}
pub(crate) fn overflow() -> ViewRefusal {
    ViewRefusal::Unreadable {
        definition: hirsel_syntax::DefId::intern(&hirsel_syntax::DefPath::core(
            "validation-storage",
        )),
        cause: hirsel_types::QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "validation retention".into(),
            message: "retained allocation size overflow".into(),
        }),
    }
}
pub(crate) fn arc<T>() -> u64 {
    (size_of::<T>() + 2 * size_of::<usize>()) as u64
}
pub(crate) fn vector<T>(values: &Vec<T>) -> Option<u64> {
    Some(values.capacity().checked_mul(size_of::<T>())? as u64)
}
// hashbrown's table has at most twice capacity's buckets; a third entry and
// 128 bytes cover control bytes, the terminal group and allocation alignment.
pub(crate) fn map<K, V>(capacity: usize) -> Option<u64> {
    Some(
        capacity
            .checked_mul(size_of::<(K, V)>().checked_mul(3)?.checked_add(1)?)?
            .checked_add(128)? as u64,
    )
}
pub(crate) fn violation(value: &Violation) -> Option<u64> {
    match &value.kind {
        ViolationKind::MissingSignature(path) | ViolationKind::InvalidSignature(path) => {
            crate::input::path_bytes(path)
        }
        ViolationKind::CleanupMismatch {
            expected, actual, ..
        } => vector(expected)?.checked_add(vector(actual)?),
        _ => Some(0),
    }
}
pub(crate) fn errors(values: &Vec<Violation>) -> Option<u64> {
    values.iter().try_fold(vector(values)?, |bytes, value| {
        bytes.checked_add(violation(value)?)
    })
}

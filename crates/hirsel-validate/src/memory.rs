//! Allocation ownership for admitted, immutable validation inputs.
use crate::{ViewRefusal, Violation, ViolationKind};
use std::sync::{Arc, Mutex};

/// The host's existing working-memory allowance and signature allocation visitor.
pub trait ValidationMemory: Send + Sync {
    fn retain(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, ViewRefusal>;
    /// Admit a demanded view after reclaiming optional cached allocations.
    fn retain_read(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, ViewRefusal> {
        self.retain(bytes)
    }
    fn retain_signature(
        &self,
        signature: &hirsel_types::SignatureRef,
    ) -> Result<Arc<dyn Send + Sync>, ViewRefusal>;
}

/// Required view reads retain against the same allowance as optional caching,
/// with permission to reclaim idle entries before refusing the read.
pub(crate) struct ReadMemory<'a>(pub(crate) &'a dyn ValidationMemory);
impl ValidationMemory for ReadMemory<'_> {
    fn retain(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, ViewRefusal> {
        self.0.retain_read(bytes)
    }
    fn retain_signature(
        &self,
        signature: &hirsel_types::SignatureRef,
    ) -> Result<Arc<dyn Send + Sync>, ViewRefusal> {
        self.0.retain_signature(signature)
    }
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
    pub(crate) fn retain_with(
        &self,
        make: impl FnOnce() -> Result<Arc<dyn Send + Sync>, ViewRefusal>,
    ) -> Result<(), ViewRefusal> {
        let mut retained = self.0.lock().unwrap();
        if retained.is_none() {
            *retained = Some(make()?);
        }
        Ok(())
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

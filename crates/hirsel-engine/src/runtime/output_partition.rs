use hirsel_engine_admission::{MemoryReservation, MemoryReservationPool};
use std::sync::Arc;

/// Source inputs keep their fixed grant; retained products draw from one
/// independently owned output partition. Neither path reserves process bytes.
pub struct ProducerAdmission {
    inputs: MemoryReservation,
    outputs: Option<Arc<OutputPartition>>,
}
impl From<MemoryReservation> for ProducerAdmission {
    fn from(inputs: MemoryReservation) -> Self {
        Self {
            inputs,
            outputs: None,
        }
    }
}
impl ProducerAdmission {
    pub(crate) fn shared(inputs: MemoryReservation, outputs: Arc<OutputPartition>) -> Self {
        Self {
            inputs,
            outputs: Some(outputs),
        }
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.inputs.bytes()
    }
    pub(crate) fn partition(
        mut self,
        retained: u64,
    ) -> Option<(MemoryReservation, Arc<OutputPartition>)> {
        let remaining = self.inputs.bytes().checked_sub(retained)?;
        let outputs = match self.outputs {
            Some(outputs) => outputs,
            None => OutputPartition::new(self.inputs.split(remaining)?)?,
        };
        Some((self.inputs, outputs))
    }
}

pub(crate) struct OutputPartition {
    pub(crate) pool: MemoryReservationPool,
    _allocation: MemoryReservation,
}
impl OutputPartition {
    pub(crate) fn new(mut allowance: MemoryReservation) -> Option<Arc<Self>> {
        let bytes = crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<Self>::uninit());
        let allocation = allowance.split(bytes)?;
        let pool = MemoryReservationPool::new(allowance).ok()?;
        Some(Arc::new(Self {
            pool,
            _allocation: allocation,
        }))
    }
}

/// A borrowed value view whose clones retain its charged source/output owner.
/// It cannot detach the backing Arc from that ownership.
pub struct RetainedView<T> {
    value: Arc<T>,
    owner: Arc<dyn Send + Sync>,
}
impl<T> RetainedView<T> {
    pub(crate) fn new(value: Arc<T>, owner: Arc<impl Send + Sync + 'static>) -> Self {
        Self { value, owner }
    }
}
impl<T> Clone for RetainedView<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            owner: self.owner.clone(),
        }
    }
}
impl<T> std::ops::Deref for RetainedView<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

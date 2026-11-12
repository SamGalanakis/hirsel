//! Large settled read sets reuse their existing immutable Lower descriptor.
use super::*;

pub(super) type Descriptor = (
    DefId,
    Reference,
    Vec<Diagnostic>,
    PackedDependencies,
    Reference,
    Reads,
    DepthEvidence<Span>,
    bool,
    staged::Selections,
    Reference,
);
pub(super) type ConstantDescriptor = (
    Materialized,
    Vec<(hirsel_syntax::ExprId, Materialized)>,
    Reads,
    DepthEvidence<Span>,
    bool,
    staged::Selections,
    Reference,
);
#[derive(Clone)]
pub(super) struct Archive {
    pub reference: Reference,
    pub decode_bytes: u64,
    pub len: usize,
    pub dependencies: usize,
}
impl Archive {
    pub(super) fn load(
        &self,
        records: &crate::NativeRecords,
        anchors: &crate::NativeRecordContext,
        store: &dyn ResultStore,
        allowance: u64,
    ) -> Option<Reads> {
        if self.reference.domain == CONSTANT_DESCRIPTOR {
            if self.reference.len > allowance || self.decode_bytes > allowance {
                return None;
            }
            let bytes = store.get_content(self.reference)?;
            let ((_, _, reads, _, _, _, _), _): (ConstantDescriptor, u64) =
                records.decode_admitted(anchors, bytes, allowance)?;
            return (reads.len() == self.len && self.dependencies == 0).then_some(reads);
        }
        Some(self.descriptor(records, anchors, store, allowance)?.5)
    }
    pub(super) fn dependencies(
        &self,
        records: &crate::NativeRecords,
        anchors: &crate::NativeRecordContext,
        store: &dyn ResultStore,
        allowance: u64,
    ) -> Option<Vec<SignatureDependency>> {
        Some(
            self.descriptor(records, anchors, store, allowance)?
                .3
                .to_vec(),
        )
    }
    fn descriptor(
        &self,
        records: &crate::NativeRecords,
        anchors: &crate::NativeRecordContext,
        store: &dyn ResultStore,
        allowance: u64,
    ) -> Option<Descriptor> {
        if self.reference.domain != LOWER_DESCRIPTOR
            || self.reference.len > allowance
            || self.decode_bytes > allowance
        {
            return None;
        }
        let bytes = store.get_content(self.reference)?;
        let (descriptor, _): (Descriptor, u64) =
            records.decode_admitted(anchors, bytes, allowance)?;
        (descriptor.5.len() == self.len && descriptor.3.len() == self.dependencies)
            .then_some(descriptor)
    }
}

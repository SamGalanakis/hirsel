//! Retained constants use the ordinary type/anchor codec.
//! Small values keep their decoded form; large values retain their published record and
//! reconstruct only when a consuming task or admitted inspector needs them.
use super::*;

#[derive(Clone)]
pub(super) struct Constants {
    data: Data,
    pub(super) archived_reads: Option<Box<reads::Archive>>,
    root_bytes: u64,
    data_bytes: u64,
    decode_bytes: u64,
}
#[derive(Clone)]
enum Data {
    Decoded(Materialized, Vec<(hirsel_syntax::ExprId, Materialized)>),
    Packed(Reference),
}
impl Constants {
    pub(super) fn new(
        value: Materialized,
        substitutions: Vec<(hirsel_syntax::ExprId, Materialized)>,
    ) -> Self {
        let mut root = hirsel_query_products::retained::MetadataWeight::default();
        root.materialized(&value);
        let mut rest = hirsel_query_products::retained::MetadataWeight::default();
        rest.vec(&substitutions, |weight, (_, value)| {
            weight.materialized(value)
        });
        let data_bytes = root.bytes().saturating_add(rest.bytes());
        Self {
            data: Data::Decoded(value, substitutions),
            archived_reads: None,
            root_bytes: root.bytes(),
            data_bytes,
            decode_bytes: 0,
        }
    }
    pub(super) fn decoded(
        &self,
        records: &crate::NativeRecords,
        anchors: &crate::NativeRecordContext,
        store: &dyn ResultStore,
        allowance: u64,
    ) -> Option<(Materialized, Vec<(hirsel_syntax::ExprId, Materialized)>)> {
        match &self.data {
            Data::Decoded(value, substitutions) => Some((value.clone(), substitutions.clone())),
            Data::Packed(record) => {
                if self.decode_bytes > allowance {
                    return None;
                }
                let bytes = store.get_content(*record)?;
                let ((value, substitutions, _, _, _, _, _), _): (reads::ConstantDescriptor, u64) =
                    records.decode_admitted(anchors, bytes, allowance)?;
                Some((value, substitutions))
            }
        }
    }
    pub(super) fn visit_types_mut(
        &mut self,
        mut visit: impl FnMut(&mut hirsel_types::TypeId),
    ) -> Option<()> {
        let Data::Decoded(value, substitutions) = &mut self.data else {
            return None;
        };
        super::carry::materialized_types(value, &mut visit);
        for (_, value) in substitutions {
            super::carry::materialized_types(value, &mut visit);
        }
        Some(())
    }
    pub(super) fn pack(&mut self, record: Reference) {
        if self.data_bytes >= 4096 {
            // The store already owns this immutable descriptor, as it owns
            // Lower bodies. A settled value needs only its typed reference.
            self.data = Data::Packed(record);
            self.data_bytes = 0;
        }
    }
    pub(super) fn working_bytes(&self) -> u64 {
        self.decode_bytes
    }
    pub(super) fn set_working_bytes(&mut self, bytes: u64) {
        self.decode_bytes = bytes;
    }
    pub(super) fn retained_bytes(&self) -> u64 {
        self.data_bytes
    }
    pub(super) fn root_clone_bytes(&self) -> u64 {
        self.root_bytes
    }
    pub(super) fn decode_bytes(&self) -> u64 {
        if matches!(self.data, Data::Packed(_)) {
            self.decode_bytes
        } else {
            0
        }
    }
}

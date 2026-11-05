//! Portable records are materialized only by an explicit archive consumer.
use super::*;
use crate::runtime::*;
use crate::store::Domain;

pub(super) struct LiveRecord<T> {
    pub value: T,
    pub domain: Domain,
    pub records: crate::NativeRecords,
    pub source: crate::NativeRecordContext,
    pub serialization_working: u64,
    pub _admission: Arc<dyn Send + Sync>,
}
impl<T: serde::Serialize + Send + Sync + 'static> LiveProductValue for LiveRecord<T> {
    fn domain(&self) -> Domain {
        self.domain
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> std::io::Result<ProductArchive> {
        ProductArchive::materialize(self.domain, admission, |grant| {
            self.records
                .encode_with_admission(&self.source, &self.value, grant, self.serialization_working)
                .ok_or_else(|| std::io::ErrorKind::OutOfMemory.into())
        })
    }
}

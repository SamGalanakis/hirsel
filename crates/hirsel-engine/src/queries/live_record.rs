//! Portable records are materialized only by an explicit archive consumer.
use super::*;
use crate::store::Domain;
use hirsel_engine_runtime::*;

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

pub(super) fn import_types(
    source: &hirsel_types::TypeInterner,
    target: &hirsel_types::TypeInterner,
    roots: &[hirsel_types::TypeId],
) -> Option<std::collections::HashMap<hirsel_types::TypeId, hirsel_types::TypeId>> {
    // hirsel-check-protocol/src/rows.rs:46: iterative postorder. A cyclic or
    // missing graph declines optional carry and uses the normal query path.
    let mut map = std::collections::HashMap::new();
    let mut active = std::collections::HashSet::new();
    let mut pending: Vec<_> = roots.iter().map(|id| (*id, false)).collect();
    while let Some((id, expanded)) = pending.pop() {
        if map.contains_key(&id) {
            continue;
        }
        let row = source.get(id)?;
        if expanded {
            let mut row = row.clone();
            let mut complete = true;
            hirsel_check_protocol::rows::edges_mut(&mut row.kind, |edge| match map.get(edge) {
                Some(mapped) => *edge = *mapped,
                None => complete = false,
            });
            if !complete {
                return None;
            }
            map.insert(id, target.intern(row));
            active.remove(&id);
        } else {
            if !active.insert(id) {
                return None;
            }
            pending.push((id, true));
            pending.extend(
                row.kind
                    .graph_edges()
                    .into_iter()
                    .rev()
                    .map(|edge| (edge.ty, false)),
            );
        }
    }
    Some(map)
}

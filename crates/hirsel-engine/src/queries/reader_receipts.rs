//! Exact per-root observations in request transport. Reductions keep only one
//! row per key, retaining the first observation and every disagreement.
use super::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};

const READERS: Domain = Domain {
    kind: 0x4600_003c,
    schema: 1,
};

fn decoding_bytes(rows: u32, max_key: u32) -> Option<u64> {
    // Reads::decode starts both vectors at min(rows, 65536). Above that,
    // geometric growth can overlap two capacities with the final Arc copy.
    // Provider bytes retain their transport grant; scratch covers native rows,
    // permutation/seen buffers and one decoded key's temporary names.
    let per_row = if rows > 1 << 16 {
        3 * size_of::<(ReadKey, ContentHash)>() + 13
    } else {
        2 * size_of::<(ReadKey, ContentHash)>() + 5
    };
    (rows as u64)
        .checked_mul(per_row as u64)?
        .checked_add((max_key as u64).checked_mul(8)?)?
        .checked_add(256)
}

#[derive(Clone)]
pub(super) struct ReaderReceipt {
    backing: Arc<ReaderBacking>,
    live: Option<ReaderSource>,
}
#[derive(Clone, Copy)]
pub(super) enum ReaderSource {
    Front(DefId, bool),
    Lower(hirsel_engine_runtime::QueryId),
}
enum ReaderBacking {
    Live {
        reads: Arc<Reads>,
        _admission: Arc<dyn Send + Sync>,
    },
    Spilled {
        reference: Reference,
        rows: u32,
        max_key: u32,
    },
}
impl std::fmt::Debug for ReaderReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderReceipt")
            .field("rows", &self.len())
            .finish()
    }
}
impl PartialEq for ReaderReceipt {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.backing, &other.backing)
            || match (&*self.backing, &*other.backing) {
                (ReaderBacking::Live { reads: a, .. }, ReaderBacking::Live { reads: b, .. }) => {
                    a == b && a.observed().eq(b.observed())
                }
                (
                    ReaderBacking::Spilled { reference: a, .. },
                    ReaderBacking::Spilled { reference: b, .. },
                ) => a == b,
                _ => false,
            }
    }
}
impl Eq for ReaderReceipt {}
impl ReaderReceipt {
    pub(super) fn capture(store: &dyn ResultStore, reads: &Reads, scratch: u64) -> Option<Self> {
        // A pooled clone keeps Front's refundable leases alive. Spill those
        // exact observations to request transport instead of duplicating their
        // wide native rows in its bounded live partition. Flat rows carry no
        // producer lease and can keep their existing shared allocation.
        if !reads.pooled() {
            let charge = reads
                .allocations()
                .try_fold(
                    hirsel_engine_admission::retained_arc_bytes(reads),
                    |bytes, (_, allocation)| bytes.checked_add(allocation as u64),
                )?
                .checked_add(hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<ReaderBacking>::uninit(),
                ))?;
            if let Some(admission) = store.try_admit_product_memory(charge) {
                return Some(Self {
                    backing: Arc::new(ReaderBacking::Live {
                        reads: Arc::new(reads.clone()),
                        _admission: admission,
                    }),
                    live: None,
                });
            }
        }
        let encoded = reads.encoded_bytes()?;
        let native = (reads.len() as u64)
            .checked_mul((size_of::<(ReadKey, ContentHash)>() + 4) as u64)?
            .checked_add(hirsel_engine_admission::retained_arc_bytes(reads))?;
        if encoded.checked_mul(2)?.checked_add(native)? > scratch {
            return None;
        }
        let max_key = reads
            .iter()
            .try_fold(0usize, |max, (key, _)| Some(max.max(key.encoded_bytes()?)))?;
        let rows = u32::try_from(reads.len()).ok()?;
        let max_key = u32::try_from(max_key).ok()?;
        store.archive_encoded(hirsel_engine_store::ArchiveBoundary::RequiredSpill);
        let mut bytes = Vec::with_capacity(usize::try_from(encoded).ok()?);
        reads.encode(&mut bytes);
        let reference = store.put_content(READERS, &bytes)?;
        (reference.domain == READERS).then(|| Self {
            backing: Arc::new(ReaderBacking::Spilled {
                reference,
                rows,
                max_key,
            }),
            live: None,
        })
    }
    pub(super) fn len(&self) -> usize {
        match &*self.backing {
            ReaderBacking::Live { reads, .. } => reads.len(),
            ReaderBacking::Spilled { rows, .. } => *rows as usize,
        }
    }
    pub(super) fn decoding_bytes(&self) -> Option<u64> {
        match &*self.backing {
            ReaderBacking::Live { .. } => Some(0),
            ReaderBacking::Spilled { rows, max_key, .. } => decoding_bytes(*rows, *max_key),
        }
    }
    fn bytes(&self, store: &dyn ResultStore) -> Option<hirsel_engine_store::Bytes> {
        let ReaderBacking::Spilled { reference, .. } = &*self.backing else {
            return None;
        };
        let bytes = store.get_content(*reference)?;
        (READERS.reference(&bytes)? == *reference).then_some(bytes)
    }
    pub(super) fn load(&self, store: &dyn ResultStore) -> Option<Arc<Reads>> {
        if let ReaderBacking::Live { reads, .. } = &*self.backing {
            return Some(reads.clone());
        }
        let bytes = self.bytes(store)?;
        let mut input = bytes.as_ref();
        let reads = Reads::decode(&mut input)?;
        (input.is_empty() && reads.len() == self.len()).then(|| Arc::new(reads))
    }
    fn merge(&self, union: &mut ReadUnion, store: &dyn ResultStore, scratch: u64) -> Option<()> {
        match &*self.backing {
            ReaderBacking::Live { reads, .. } => union.merge_rows(reads, scratch, 0),
            ReaderBacking::Spilled { rows, max_key, .. } => {
                union.merge_encoded(&self.bytes(store)?, *rows, *max_key, scratch)
            }
        }
    }
}

#[derive(Default)]
pub(super) struct ReadUnion {
    rows: BTreeMap<ReadKey, (ContentHash, usize)>,
    conflict: Option<ReadKey>,
}
impl ReadUnion {
    pub(super) fn required(&self, reads: &Reads) -> Option<u64> {
        let missing = reads
            .iter()
            .filter(|(key, _)| !self.rows.contains_key(key))
            .count();
        Self::bytes(self.rows.len().checked_add(missing)?)
    }
    fn bytes(len: usize) -> Option<u64> {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.tree::<ReadKey, (ContentHash, usize)>(len);
        Some(weight.bytes())
    }
    pub(super) fn merge(&mut self, reads: &Reads, scratch: u64) -> Option<()> {
        let held = reads
            .allocations()
            .try_fold(0u64, |sum, (_, bytes)| sum.checked_add(bytes as u64))?;
        self.merge_rows(reads, scratch, held)
    }
    fn merge_live(&mut self, reads: &parallel::LiveFrontReads, scratch: u64) -> Option<()> {
        // The guard retains Front's original allocation grant throughout this
        // active reduction, including concurrent eviction of its live slot.
        self.merge_rows(reads, scratch, 0)
    }
    fn merge_rows(&mut self, reads: &Reads, scratch: u64, held: u64) -> Option<()> {
        if self.required(reads)?.checked_add(held)? > scratch {
            return None;
        }
        use std::collections::btree_map::Entry;
        // Rust alloc btree/map/entry.rs: insertion preserves the first ordinal.
        // The final codec performs the existing canonical key sort.
        for &(key, value) in reads.observed() {
            let ordinal = self.rows.len();
            match self.rows.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert((value, ordinal));
                }
                Entry::Occupied(entry) => {
                    if entry.get().0 != value && self.conflict.is_none_or(|old| key < old) {
                        self.conflict = Some(key);
                    }
                }
            }
        }
        self.fits_scratch(scratch)?.then_some(())
    }
    pub(super) fn finish(self) -> Result<Reads, hirsel_query_products::reads::Inconsistent> {
        if let Some(key) = self.conflict {
            return Err(hirsel_query_products::reads::Inconsistent(key));
        }
        let rows = self
            .rows
            .into_iter()
            .map(|(key, (value, ordinal))| (key, value, ordinal))
            .collect();
        Ok(Reads::from_canonical_observations(rows))
    }
}

/// A read reference in Front's existing per-root metadata. The source keeps
/// that original grant and the required request transport alive.
pub(crate) struct ArchivedReader {
    receipt: ReaderReceipt,
    source: Arc<crate::products::GrowthPublicationSource>,
    _owned: Option<Arc<hirsel_engine_admission::MemoryReservation>>,
}
impl ArchivedReader {
    pub(crate) fn source(&self) -> &Arc<crate::products::GrowthPublicationSource> {
        &self.source
    }
    pub(crate) fn capture(
        source: Arc<crate::products::GrowthPublicationSource>,
        reads: &Reads,
    ) -> Option<Arc<Self>> {
        let receipt = ReaderReceipt::capture(&*source.store, reads, source.scratch)?;
        Some(Self::from_receipt(source, receipt))
    }
    pub(super) fn from_receipt(
        source: Arc<crate::products::GrowthPublicationSource>,
        receipt: ReaderReceipt,
    ) -> Arc<Self> {
        Arc::new(Self {
            receipt,
            source,
            _owned: None,
        })
    }
    pub(crate) fn capture_owned(
        source: Arc<crate::products::GrowthPublicationSource>,
        reads: &Reads,
        pool: &hirsel_engine_admission::MemoryReservationPool,
    ) -> Option<Arc<Self>> {
        let bytes =
            hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<Self>::uninit())
                .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<hirsel_engine_admission::MemoryReservation>::uninit(),
            ))?;
        let allowance = Arc::new(pool.take(bytes)?);
        let mut reader = Self::capture(source, reads)?;
        Arc::get_mut(&mut reader)?._owned = Some(allowance);
        Some(reader)
    }
    pub(crate) fn load(&self) -> Option<Arc<Reads>> {
        if self.working_bytes()? > self.source.scratch {
            return None;
        }
        self.receipt.load(&*self.source.store)
    }
    pub(crate) fn working_bytes(&self) -> Option<u64> {
        self.receipt.decoding_bytes()
    }
    pub(crate) fn encoding_working_bytes(&self) -> Option<u64> {
        match &*self.receipt.backing {
            ReaderBacking::Live { reads, .. } => reads.encoding_working_bytes(),
            ReaderBacking::Spilled { reference, .. } => self
                .working_bytes()?
                .checked_add(reference.len.checked_mul(4)?),
        }
    }
}
impl serde::Serialize for ArchivedReader {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = match &*self.receipt.backing {
            ReaderBacking::Live { reads, .. } => {
                self.source
                    .store
                    .archive_encoded(hirsel_engine_store::ArchiveBoundary::Export);
                let mut bytes = Vec::with_capacity(
                    reads
                        .encoded_bytes()
                        .ok_or_else(|| serde::ser::Error::custom("reader size overflow"))?
                        as usize,
                );
                reads.encode(&mut bytes);
                hirsel_engine_store::Bytes::from(bytes)
            }
            ReaderBacking::Spilled { .. } => self
                .receipt
                .bytes(&*self.source.store)
                .ok_or_else(|| serde::ser::Error::custom("unavailable archived reader receipt"))?,
        };
        // Capture validated these exact HRD3 bytes. Authenticate the reference
        // and preserve the wire bytes without decoding and encoding them again.
        serializer.serialize_bytes(&bytes)
    }
}
impl ReadUnion {
    pub(super) fn reduce(
        readers: &[Arc<ArchivedReader>],
        scratch: u64,
        mut live: impl FnMut(usize) -> Option<parallel::LiveFrontReads>,
    ) -> Option<Self> {
        let mut union = Self::default();
        for (index, reader) in readers.iter().enumerate() {
            if let Some(reads) = live(index) {
                union.merge_live(&reads, scratch)?;
            } else {
                reader
                    .receipt
                    .merge(&mut union, &*reader.source.store, scratch)?;
            }
        }
        if !union.fits_scratch(scratch)? {
            return None;
        }
        Some(union)
    }
}

#[derive(Default)]
pub(super) struct ReaderSequence(Vec<ReaderReceipt>);
impl ReaderSequence {
    pub(super) fn record(
        &mut self,
        store: &dyn ResultStore,
        reads: &Reads,
        scratch: u64,
        retained: u64,
        live: Option<ReaderSource>,
    ) -> Option<ReaderReceipt> {
        let reader = ReaderReceipt::capture(store, reads, scratch)?;
        self.record_receipt(reader, retained, live)
    }
    pub(super) fn record_receipt(
        &mut self,
        mut reader: ReaderReceipt,
        retained: u64,
        live: Option<ReaderSource>,
    ) -> Option<ReaderReceipt> {
        if self.0.len() == self.0.capacity() {
            let slots = usize::try_from(retained / size_of::<ReaderReceipt>() as u64).ok()?;
            let capacity = self.0.len().checked_add(128)?.min(slots);
            if capacity <= self.0.len() {
                return None;
            }
            self.0.reserve_exact(capacity - self.0.len());
        }
        reader.live = live;
        self.0.push(reader.clone());
        Some(reader)
    }
    pub(super) fn refresh_lower(
        &mut self,
        store: &dyn ResultStore,
        scratch: u64,
        lower: &FrontLowerQueries,
    ) -> Result<Option<()>, QueryError> {
        for reader in &mut self.0 {
            let Some(ReaderSource::Lower(id)) = reader.live else {
                continue;
            };
            if reader.decoding_bytes().is_none_or(|bytes| bytes > scratch) {
                return Ok(None);
            }
            let Some(reads) = reader.load(store) else {
                return Ok(None);
            };
            let current = lower.check_observations(id, reads)?;
            let Some(mut refreshed) = ReaderReceipt::capture(store, &current, scratch) else {
                return Ok(None);
            };
            refreshed.live = reader.live;
            *reader = refreshed;
        }
        Ok(Some(()))
    }
    pub(super) fn lower_reader(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<&ReaderReceipt> {
        self.0.iter().find(
            |reader| matches!(reader.live, Some(ReaderSource::Lower(selected)) if selected == id),
        )
    }
    pub(super) fn finish(
        &self,
        store: &dyn ResultStore,
        scratch: u64,
        front: &FrontBodyQueries,
    ) -> Option<Result<Reads, hirsel_query_products::reads::Inconsistent>> {
        let mut union = ReadUnion::default();
        for reader in &self.0 {
            let live = reader.live.and_then(|source| match source {
                ReaderSource::Front(id, checked) => front.live_reads(id, checked),
                ReaderSource::Lower(_) => None,
            });
            if let Some(reads) = live {
                union.merge_live(&reads, scratch)?;
            } else {
                reader.merge(&mut union, store, scratch)?;
            }
        }
        if !union.fits_scratch(scratch)? {
            return None;
        }
        Some(union.finish())
    }
}
impl ReadUnion {
    fn merge_encoded(&mut self, bytes: &[u8], rows: u32, max_key: u32, scratch: u64) -> Option<()> {
        // Provider bytes retain their transport grant. Scratch owns only the
        // pending new rows, one decoded key and the permutation's seen bits.
        let fixed = (rows as u64)
            .checked_add((max_key as u64).checked_mul(8)?)?
            .checked_add(256)?;
        let pending_bytes = |len| {
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.tree::<usize, (ReadKey, ContentHash)>(len);
            weight.bytes()
        };
        let mut pending = BTreeMap::new();
        if Self::bytes(self.rows.len())?
            .checked_add(pending_bytes(0))?
            .checked_add(fixed)?
            > scratch
        {
            return None;
        }
        let count = Reads::visit_encoded(bytes, |index, row| {
            if let Some((key, value)) = row {
                if let Some(&(previous, _)) = self.rows.get(&key) {
                    if previous != value && self.conflict.is_none_or(|old| key < old) {
                        self.conflict = Some(key);
                    }
                } else {
                    if Self::bytes(self.rows.len())?
                        .checked_add(pending_bytes(pending.len().checked_add(1)?))?
                        .checked_add(fixed)?
                        > scratch
                    {
                        return None;
                    }
                    pending.insert(index, (key, value));
                }
            } else if let Some(&(key, value)) = pending.get(&index) {
                if Self::bytes(self.rows.len().checked_add(1)?)?
                    .checked_add(pending_bytes(pending.len() - 1))?
                    .checked_add(fixed)?
                    > scratch
                {
                    return None;
                }
                pending.remove(&index);
                let ordinal = self.rows.len();
                self.rows.insert(key, (value, ordinal));
            }
            Some(())
        })?;
        (count == rows as usize && pending.is_empty()).then_some(())
    }
    fn fits_scratch(&self, scratch: u64) -> Option<bool> {
        let triples = (self.rows.len() as u64)
            .checked_mul(size_of::<(ReadKey, ContentHash, usize)>() as u64)?;
        let flat = (self.rows.len() as u64)
            .checked_mul((size_of::<(ReadKey, ContentHash)>() + 8) as u64)?;
        Some(
            Self::bytes(self.rows.len())?
                .checked_add(triples)?
                .max(triples.checked_add(flat)?)
                <= scratch,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn completed_reader_receipt_releases_pooled_rows_and_preserves_observation_order() {
        let reads = Reads::from_vec(vec![
            (ReadKey::Environment, ContentHash::of_bytes(b"environment")),
            (ReadKey::UniverseErrors, ContentHash::of_bytes(b"errors")),
        ])
        .unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let mut allowance = budget.reserve(1 << 20).unwrap();
        let credit = Arc::new(Mutex::new(1 << 20));
        let mut pool = hirsel_query_products::reads::ReadPool::default();
        let pooled = pool
            .intern_tracked(
                &reads,
                &mut allowance,
                &mut credit.lock().unwrap(),
                credit.clone(),
            )
            .unwrap();
        let store = hirsel_engine_store::MemStore::default();
        let receipt = ReaderReceipt::capture(&store, &pooled, 1 << 20).unwrap();
        drop(pooled);
        *credit.lock().unwrap() += pool.reclaim();
        assert_eq!(*credit.lock().unwrap(), 1 << 20);
        let restored = receipt.load(&store).unwrap();
        assert_eq!(restored.as_ref(), &reads);
        assert!(restored.observed().eq(reads.observed()));
        assert_eq!(restored.digest(), reads.digest());
    }
}

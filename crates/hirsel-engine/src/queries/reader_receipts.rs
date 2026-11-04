//! Exact per-root observations in request transport. Reductions keep only one
//! row per key, retaining the first observation and every disagreement.
use super::*;
use crate::store::{Domain, Reference, ResultStore};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReaderReceipt {
    reference: Reference,
    rows: u32,
    max_key: u32,
}
impl ReaderReceipt {
    pub(super) fn capture(store: &dyn ResultStore, reads: &Reads, scratch: u64) -> Option<Self> {
        let encoded = reads.encoded_bytes()?;
        let native = (reads.len() as u64)
            .checked_mul((size_of::<(ReadKey, ContentHash)>() + 4) as u64)?
            .checked_add(crate::memo::retained_arc_bytes(reads))?;
        // Exact capacity avoids the geometric encoding-buffer peak. Include
        // the current flat rows and a provider copy within the same scratch.
        if encoded.checked_mul(2)?.checked_add(native)? > scratch {
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(encoded).ok()?);
        reads.encode(&mut bytes);
        debug_assert_eq!(bytes.len() as u64, encoded);
        if bytes.len() as u64 > scratch {
            return None;
        }
        Some(Self {
            reference: store.put_content(READERS, &bytes)?,
            rows: u32::try_from(reads.len()).ok()?,
            max_key: u32::try_from(
                reads
                    .iter()
                    .map(|(key, _)| key.encoded_bytes())
                    .try_fold(0usize, |n, next| Some(n.max(next?)))?,
            )
            .ok()?,
        })
    }
    pub(super) fn len(&self) -> usize {
        self.rows as usize
    }
    pub(super) fn decoding_bytes(&self) -> Option<u64> {
        decoding_bytes(self.rows, self.max_key)
    }
    fn bytes(&self, store: &dyn ResultStore) -> Option<crate::store::Bytes> {
        let bytes = store.get_content(self.reference)?;
        (self.reference.domain == READERS && READERS.reference(&bytes)? == self.reference)
            .then_some(bytes)
    }
    pub(super) fn load(&self, store: &dyn ResultStore) -> Option<Arc<Reads>> {
        let bytes = self.bytes(store)?;
        let mut input = bytes.as_ref();
        let reads = Reads::decode(&mut input)?;
        (input.is_empty() && reads.len() == self.rows as usize).then(|| Arc::new(reads))
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
        let mut weight = crate::NativeMetadataWeight::default();
        weight.tree::<ReadKey, (ContentHash, usize)>(len);
        Some(weight.bytes())
    }
    pub(super) fn merge(&mut self, reads: &Reads, scratch: u64) -> Option<()> {
        let held = reads
            .allocations()
            .try_fold(0u64, |sum, (_, bytes)| sum.checked_add(bytes as u64))?;
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
    pub(super) fn finish(self) -> Result<Reads, crate::memo::Inconsistent> {
        if let Some(key) = self.conflict {
            return Err(crate::memo::Inconsistent(key));
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
    output: crate::store::Output,
    len: u64,
    rows: u32,
    max_key: u32,
    source: Arc<crate::runtime::GrowthPublicationSource>,
    _owned: Option<Arc<crate::runtime::MemoryReservation>>,
}
impl ArchivedReader {
    pub(crate) fn source(&self) -> &Arc<crate::runtime::GrowthPublicationSource> {
        &self.source
    }
    pub(crate) fn capture(
        source: Arc<crate::runtime::GrowthPublicationSource>,
        reads: &Reads,
    ) -> Option<Arc<Self>> {
        let receipt = ReaderReceipt::capture(&*source.store, reads, source.scratch)?;
        Some(Arc::new(Self {
            output: receipt.reference.output,
            len: receipt.reference.len,
            rows: u32::try_from(reads.len()).ok()?,
            max_key: u32::try_from(
                reads
                    .iter()
                    .map(|(key, _)| key.encoded_bytes())
                    .try_fold(0usize, |n, next| Some(n.max(next?)))?,
            )
            .ok()?,
            source,
            _owned: None,
        }))
    }
    pub(crate) fn capture_owned(
        source: Arc<crate::runtime::GrowthPublicationSource>,
        reads: &Reads,
        pool: &crate::runtime::MemoryReservationPool,
    ) -> Option<Arc<Self>> {
        let bytes = crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<Self>::uninit())
            .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                crate::runtime::MemoryReservation,
            >::uninit()))?;
        let allowance = Arc::new(pool.take(bytes)?);
        let mut reader = Self::capture(source, reads)?;
        Arc::get_mut(&mut reader)?._owned = Some(allowance);
        Some(reader)
    }
    fn bytes(&self) -> Option<crate::store::Bytes> {
        let reference = Reference {
            output: self.output,
            domain: READERS,
            len: self.len,
        };
        let bytes = self.source.store.get_content(reference)?;
        (READERS.reference(&bytes)? == reference).then_some(bytes)
    }
    pub(crate) fn load(&self) -> Option<Arc<Reads>> {
        if self.working_bytes()? > self.source.scratch {
            return None;
        }
        let bytes = self.bytes()?;
        let mut input = bytes.as_ref();
        let reads = Reads::decode(&mut input)?;
        (input.is_empty() && reads.len() == self.rows as usize).then(|| Arc::new(reads))
    }
    pub(crate) fn working_bytes(&self) -> Option<u64> {
        decoding_bytes(self.rows, self.max_key)
    }
    pub(crate) fn encoding_working_bytes(&self) -> Option<u64> {
        self.working_bytes()?.checked_add(self.len.checked_mul(4)?)
    }
}
impl serde::Serialize for ArchivedReader {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.load()
            .ok_or_else(|| serde::ser::Error::custom("unavailable archived reader receipt"))?
            .serialize(serializer)
    }
}
impl ReadUnion {
    pub(super) fn reduce(readers: &[Arc<ArchivedReader>], scratch: u64) -> Option<Self> {
        let mut union = Self::default();
        for reader in readers {
            let bytes = reader.bytes()?;
            union.merge_encoded(&bytes, reader.rows, reader.max_key, scratch)?;
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
    ) -> Option<()> {
        if self.0.len() == self.0.capacity() {
            let slots = usize::try_from(retained / size_of::<ReaderReceipt>() as u64).ok()?;
            let capacity = self.0.len().checked_add(128)?.min(slots);
            if capacity <= self.0.len() {
                return None;
            }
            self.0.reserve_exact(capacity - self.0.len());
        }
        self.0.push(ReaderReceipt::capture(store, reads, scratch)?);
        Some(())
    }
    pub(super) fn finish(
        self,
        store: &dyn ResultStore,
        scratch: u64,
    ) -> Option<Result<Reads, crate::memo::Inconsistent>> {
        let mut union = ReadUnion::default();
        for reader in &self.0 {
            let bytes = reader.bytes(store)?;
            union.merge_encoded(&bytes, reader.rows, reader.max_key, scratch)?;
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
            let mut weight = crate::NativeMetadataWeight::default();
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

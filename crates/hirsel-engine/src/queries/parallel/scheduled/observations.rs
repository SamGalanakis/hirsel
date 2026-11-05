//! Completed memo observations remain exact when compact facts leave RAM.
//! The existing request transport owns archived bytes, including with none@1.
use super::*;
use bincode::Options;

const OBSERVATIONS: Domain = Domain {
    kind: 0x4600_003b,
    schema: 1,
};
pub(super) enum Rows {
    Retained(Box<[(crate::memo::MemoKey, Arc<Reads>)]>),
    Archived {
        keys: Box<[crate::memo::MemoKey]>,
        reference: Reference,
    },
}
impl Rows {
    pub(super) fn keys(&self) -> impl Iterator<Item = crate::memo::MemoKey> + '_ {
        let len = match self {
            Self::Retained(rows) => rows.len(),
            Self::Archived { keys, .. } => keys.len(),
        };
        (0..len).map(|at| match self {
            Self::Retained(rows) => rows[at].0,
            Self::Archived { keys, .. } => keys[at],
        })
    }
    fn load(&self, store: &dyn ResultStore) -> Option<Vec<(crate::memo::MemoKey, Arc<Reads>)>> {
        match self {
            Self::Retained(rows) => Some(rows.to_vec()),
            Self::Archived { keys, reference } => {
                let bytes = store.get_content(*reference)?;
                if reference.domain != OBSERVATIONS || !bytes.matches_reference(*reference) {
                    return None;
                }
                let reads: Vec<Arc<Reads>> = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(reference.len)
                    .reject_trailing_bytes()
                    .deserialize(&bytes)
                    .ok()?;
                (reads.len() == keys.len()).then(|| keys.iter().copied().zip(reads).collect())
            }
        }
    }
}
impl FrontObservations {
    pub(super) fn load(
        &self,
        store: &dyn ResultStore,
    ) -> Option<Vec<(crate::memo::MemoKey, Arc<Reads>)>> {
        self.rows.load(store)
    }
}
impl FrontBodyQueries {
    // Completed P observations and at most three concurrent Growth reader
    // references per source root use the existing fixed metadata slots.
    const READER_METADATA: () = assert!(
        size_of::<FrontObservations>()
            + 2 * size_of::<usize>()
            + 3 * (size_of::<crate::queries::ArchivedReader>() + 2 * size_of::<usize>())
            <= 32 * size_of::<usize>() + 3 * size_of::<(crate::memo::MemoKey, Arc<Reads>)>()
    );
    pub(super) fn archive_observations(
        &self,
        outcome: &FrontOutcome,
        scratch: u64,
    ) -> Option<Arc<FrontObservations>> {
        let () = Self::READER_METADATA;
        let archived = (|| {
            // Flat observations still borrow their producer's lease. Only
            // pooled readers carry the independent, last-reader refund needed
            // to release that producer while an inspection holds its reads.
            if self.read_pool.get().is_none() || !self.store.transports() {
                return None;
            }
            // Reads' canonical codec preserves both the exact rows and their
            // observation order. Admit its temporary encoding before allocating.
            let working = outcome.memos.iter().try_fold(0u64, |sum, (_, reads)| {
                sum.checked_add(reads.encoding_working_bytes()?)
            })?;
            let vector = (outcome.memos.len() * size_of::<Arc<Reads>>()) as u64;
            if working.checked_add(vector)?.checked_mul(2)? > scratch {
                return None;
            }
            let reads = outcome
                .memos
                .iter()
                .map(|(_, reads)| reads)
                .collect::<Vec<_>>();
            let bytes = bincode::serialize(&reads).ok()?;
            let reference = self.store.put_content(OBSERVATIONS, &bytes)?;
            Some(Rows::Archived {
                keys: outcome.memos.iter().map(|(key, _)| *key).collect(),
                reference,
            })
        })();
        let (rows, backing) = if let Some(rows) = archived {
            // The reference-only owner fits the per-root metadata admitted by
            // input_bytes. It owns no read payload and needs no second lease.
            const {
                assert!(
                    size_of::<FrontObservations>() + 2 * size_of::<usize>()
                        <= 32 * size_of::<usize>()
                )
            };
            (rows, None)
        } else {
            let mut weight = crate::memo::retained::Weight::default();
            for (_, reads) in &outcome.memos {
                weight.reads(reads);
            }
            let charge = weight
                .bytes
                .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                    FrontObservations,
                >::uninit(
                )))?
                .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                    LiveSourceBacking,
                >::uninit(
                )))?;
            (
                Rows::Retained(outcome.memos.clone()),
                Some(outcome._backing.split(charge)?),
            )
        };
        Some(Arc::new(FrontObservations {
            rows,
            _backing: backing,
        }))
    }
}

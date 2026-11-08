//! Exact per-root observations collected by Check and source growth, and
//! their union. A union keeps one row per key with its first ordinal and
//! reports the least key whose observations disagree.
use super::*;

#[derive(Clone)]
pub(super) enum ReaderSource {
    Front(DefId),
    Lower(
        hirsel_engine_runtime::QueryId,
        scheduled_lower::check::CheckProducer,
    ),
    StagedLower(hirsel_engine_runtime::QueryId),
}

/// Rows hash by interned identity; the one key sort happens at `finish`
/// instead of a path comparison per merged row.
#[derive(Default)]
pub(super) struct ReadUnion {
    rows: std::collections::HashMap<ReadKey, (ContentHash, usize), hirsel_intern::IdHasher>,
    conflict: Option<ReadKey>,
}
impl ReadUnion {
    pub(super) fn of<'a>(readers: impl IntoIterator<Item = &'a Reads>) -> Self {
        let mut union = Self::default();
        for reads in readers {
            union.merge(reads);
        }
        union
    }
    pub(super) fn merge(&mut self, reads: &Reads) {
        use std::collections::hash_map::Entry;
        // An occupied entry keeps its first ordinal.
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
    }
    pub(super) fn finish(self) -> Result<Reads, hirsel_query_products::reads::Inconsistent> {
        if let Some(key) = self.conflict {
            return Err(hirsel_query_products::reads::Inconsistent(key));
        }
        let mut rows: Vec<_> = self
            .rows
            .into_iter()
            .map(|(key, (value, ordinal))| (key, value, ordinal))
            .collect();
        rows.sort_unstable_by_key(|&(key, ..)| key);
        Ok(Reads::from_canonical_observations(rows))
    }
}

#[derive(Default)]
pub(super) struct ReaderSequence(Vec<(Arc<Reads>, Option<ReaderSource>)>);
impl ReaderSequence {
    pub(super) fn record(&mut self, reads: Arc<Reads>, source: Option<ReaderSource>) -> Arc<Reads> {
        self.0.push((reads.clone(), source));
        reads
    }
    pub(super) fn front_reader(&self, path: &DefPath) -> Option<&Arc<Reads>> {
        self.0.iter().find_map(|(reads, source)| match source {
            Some(ReaderSource::Front(id)) if id.path() == path => Some(reads),
            _ => None,
        })
    }
    pub(super) fn lower_reader(&self, id: hirsel_engine_runtime::QueryId) -> Option<&Arc<Reads>> {
        self.0.iter().find_map(|(reads, source)| match source {
            Some(ReaderSource::Lower(selected, _)) if *selected == id => Some(reads),
            _ => None,
        })
    }
    pub(super) fn lower_owner(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<&Arc<FrontLowerQueries>> {
        self.0.iter().find_map(|(_, source)| match source {
            Some(ReaderSource::Lower(selected, producer)) if *selected == id => {
                Some(&producer.owner)
            }
            _ => None,
        })
    }
    pub(super) fn finish(&self) -> Result<Reads, hirsel_query_products::reads::Inconsistent> {
        let result = ReadUnion::of(self.0.iter().map(|(reads, _)| &**reads)).finish();
        if let Err(hirsel_query_products::reads::Inconsistent(key)) = &result {
            for (index, (reads, source)) in self.0.iter().enumerate() {
                let Some(value) = reads.get(key) else {
                    continue;
                };
                let source = match source {
                    Some(ReaderSource::Front(id)) => format!("Front {id:?}"),
                    Some(ReaderSource::Lower(id, _)) => format!("Lower {id:?}"),
                    Some(ReaderSource::StagedLower(id)) => format!("staged Lower {id:?}"),
                    None => "Check aggregate".to_owned(),
                };
                eprintln!("Check conflicting observation {index}: {source}; {key:?} = {value:?}");
            }
        }
        result
    }
}

//! Exact per-root observations collected by Check and source growth. Their
//! union is a `ReadsBuilder` over the canonical sets.
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
        let mut union = ReadsBuilder::new();
        for (reads, _) in &self.0 {
            union.merge(reads);
        }
        let result = union.finish();
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

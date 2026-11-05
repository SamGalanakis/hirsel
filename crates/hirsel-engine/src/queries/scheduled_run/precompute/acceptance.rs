//! Durable publication joins the independent source Check and precompute fold.
use super::*;

pub(super) struct Task {
    queries: Arc<FrontRunQueries>,
    query: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl Task {
    pub(super) fn new(queries: Arc<FrontRunQueries>, query: QueryId) -> Self {
        let terminal = queries
            .precomputed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(product, _)| Ok(product.clone()));
        Self {
            queries,
            query,
            terminal,
        }
    }
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<Option<Vec<QueryId>>, Reference> {
        let queries = &self.queries;
        let check = queries.check.as_ref().ok_or(queries.errors.protocol)?;
        let preparation = preparation_query(self.query);
        let checked = cx.read(check.query());
        let prepared = cx.read(preparation);
        let mut waiting = Vec::new();
        if checked.is_pending() {
            waiting.push(check.query());
        }
        if prepared.is_pending() {
            waiting.push(preparation);
        }
        if !waiting.is_empty() {
            return Ok(Some(waiting));
        }
        let Poll::Ready(checked) = checked else {
            unreachable!()
        };
        let checked = checked?;
        if check.accepted(checked.clone()) != Some(true) {
            return Err(check
                .rejection_reference(&checked)
                .ok_or(queries.errors.protocol)?);
        }
        let Poll::Ready(prepared) = prepared else {
            unreachable!()
        };
        let prepared = prepared?;
        if prepared.address != preparation.address {
            return Err(queries.errors.protocol);
        }
        let archive = prepared
            .archive(ArchiveAdmission::Retained)
            .map_err(|_| queries.errors.resource)?;
        let (definitions, digest) =
            bincode::deserialize(archive.bytes()).map_err(|_| queries.errors.protocol)?;
        let output = FrontPrecomputeOutput {
            definitions,
            digest,
        };
        if definitions
            != queries
                .precompute
                .as_ref()
                .ok_or(queries.errors.protocol)?
                .definitions
        {
            return Err(queries.errors.protocol);
        }
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.precompute-acceptance.action.v1",
            [
                prepared.action.0.0.as_slice(),
                checked.action.0.0.as_slice(),
            ],
        ));
        let record = archive.reference();
        let product = ProductRef::import_archive(self.query.address, action, archive);
        if !queries
            .store
            .publish_root(product.address, product.action, record)
            && queries.store.persistence() == crate::store::Persistence::Complete
        {
            return Err(queries.failure_record(operational(
                "precompute closure could not be stored durably",
            )));
        }
        *queries
            .precomputed
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((product, output));
        Ok(None)
    }
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.query
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: (size_of::<Self>() + 2 * size_of::<usize>()) as u64,
            ..self.queries.weight
        }
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = &self.terminal {
            return finish(result.clone());
        }
        let result = match self.advance(cx) {
            Ok(Some(waiting)) => return QueryPoll::Waiting(waiting),
            Ok(None) => self
                .queries
                .precomputed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|(product, _)| product.clone())
                .ok_or(self.queries.errors.protocol),
            Err(error) => Err(error),
        };
        self.terminal = Some(result.clone());
        finish(result)
    }
}
fn finish(result: Result<ProductRef, Reference>) -> QueryPoll {
    match result {
        Ok(product) => QueryPoll::Complete(product),
        Err(error) => QueryPoll::Failed(error),
    }
}

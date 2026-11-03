//! Ordered source acceptance and dependency-ready Run metadata have separate
//! producers. This join publishes launch data only after both have settled.
use super::*;

pub(super) struct Task {
    queries: Arc<FrontRunQueries>,
    root: DefPath,
    query: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl Task {
    pub(super) fn new(queries: Arc<FrontRunQueries>, root: DefPath, query: QueryId) -> Self {
        let terminal = queries
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&query.address.0)
            .map(|(product, _)| Ok(product.clone()));
        Self {
            queries,
            root,
            query,
            terminal,
        }
    }
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<Option<Vec<QueryId>>, Reference> {
        let queries = &self.queries;
        let check = queries.check.as_ref().ok_or(queries.errors.protocol)?;
        let preparation = queries
            .preparation_query(&self.root)
            .ok_or(queries.errors.protocol)?;
        let prepared = cx.read(preparation);
        let checked = cx.read(check.query());
        let mut waiting = Vec::new();
        if prepared.is_pending() {
            waiting.push(preparation);
        }
        if checked.is_pending() {
            waiting.push(check.query());
        }
        if !waiting.is_empty() {
            return Ok(Some(waiting));
        }
        let Poll::Ready(checked) = checked else {
            unreachable!()
        };
        let checked = checked?;
        if check.accepted(checked.clone()) != Some(true) {
            return Err(checked
                .archive(ArchiveAdmission::Retained)
                .map_err(|_| queries.errors.resource)?
                .reference());
        }
        let Poll::Ready(prepared) = prepared else {
            unreachable!()
        };
        let prepared = prepared?;
        if prepared.address != preparation.address {
            return Err(queries.errors.protocol);
        }
        let output = queries
            .outcome(prepared.clone())
            .ok_or(queries.errors.protocol)?;
        let archive = prepared
            .archive(ArchiveAdmission::Retained)
            .map_err(|_| queries.errors.resource)?;
        let record = archive.reference();
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.run-acceptance.action.v1",
            [
                prepared.action.0.0.as_slice(),
                checked.action.0.0.as_slice(),
            ],
        ));
        let product = ProductRef::import_archive(self.query.address, action, archive);
        queries
            .store
            .associate_product(product.address, product.action, record);
        queries
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.query.address.0, (product, output));
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
                .completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&self.query.address.0)
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

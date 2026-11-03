//! Source acceptance joins already prepared native metadata. Native producers
//! use the preparation address; export and executable roots require this join.
use super::*;

pub(super) struct Task {
    owner: Arc<FrontNativeQueries>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl Task {
    pub(super) fn new(owner: Arc<FrontNativeQueries>) -> Self {
        let terminal = owner
            .accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|product| Ok(product.clone()));
        Self { owner, terminal }
    }
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<Option<Vec<QueryId>>, Reference> {
        let owner = &self.owner;
        let check = owner.run.source_check().ok_or(owner.errors.protocol)?;
        let prepared = cx.read(owner.preparation_query());
        let accepted = cx.read(check.query());
        let mut waiting = Vec::new();
        if prepared.is_pending() {
            waiting.push(owner.preparation_query());
        }
        if accepted.is_pending() {
            waiting.push(check.query());
        }
        if !waiting.is_empty() {
            return Ok(Some(waiting));
        }
        let Poll::Ready(accepted) = accepted else {
            unreachable!()
        };
        let checked = accepted?;
        if check.accepted(checked.clone()) != Some(true) {
            return Err(checked
                .archive(ArchiveAdmission::Retained)
                .map_err(|_| owner.errors.resource)?
                .reference());
        }
        let Poll::Ready(prepared) = prepared else {
            unreachable!()
        };
        let prepared = prepared?;
        if prepared.address != owner.preparation_query().address
            || owner.outcome(prepared.clone()).is_none()
        {
            return Err(owner.errors.protocol);
        }
        let archive = prepared
            .archive(ArchiveAdmission::Retained)
            .map_err(|_| owner.errors.resource)?;
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.native-acceptance.action.v1",
            [
                prepared.action.0.0.as_slice(),
                checked.action.0.0.as_slice(),
            ],
        ));
        let record = archive.reference();
        let product = ProductRef::import_archive(owner.query.address, action, archive);
        owner
            .store
            .associate_product(product.address, product.action, record);
        *owner.accepted.lock().unwrap_or_else(|e| e.into_inner()) = Some(product);
        Ok(None)
    }
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.owner.query()
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: (size_of::<Self>() + 2 * size_of::<usize>()) as u64,
            ..self.owner.weight
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
                .owner
                .accepted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or(self.owner.errors.protocol),
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

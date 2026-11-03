//! Compilation follows native preparation; executable completion independently
//! joins complete source acceptance. A failed source still owns diagnostics.
use super::*;

pub(super) struct Task {
    owner: Arc<NativeBootstrap>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl Task {
    pub(super) fn new(owner: Arc<NativeBootstrap>) -> Self {
        let terminal = owner
            .completed
            .get()
            .map(|completed| Ok(completed.product.clone()));
        Self { owner, terminal }
    }
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<Option<Vec<QueryId>>, Reference> {
        let owner = &self.owner;
        let compilation = cx.read(owner.preparation_query());
        let source = cx.read(owner.source.query());
        let mut waiting = Vec::new();
        if compilation.is_pending() {
            waiting.push(owner.preparation_query());
        }
        if source.is_pending() {
            waiting.push(owner.source.query());
        }
        if !waiting.is_empty() {
            return Ok(Some(waiting));
        }
        let Poll::Ready(source) = source else {
            unreachable!()
        };
        let source = source?;
        if source.address != owner.source.query().address || owner.source.outcome(source).is_none()
        {
            return Err(owner.errors.protocol);
        }
        let Poll::Ready(compilation) = compilation else {
            unreachable!()
        };
        let compilation = compilation?;
        let [compilation] = compilation
            .dependency_value()
            .ok_or(owner.errors.protocol)?
            .reads()
        else {
            return Err(owner.errors.protocol);
        };
        owner
            .publish(compilation.clone())
            .map_err(|error| owner.fail(error))?;
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
                .completed
                .get()
                .map(|completed| completed.product.clone())
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

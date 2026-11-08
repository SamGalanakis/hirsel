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
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<(), Reference> {
        let owner = &self.owner;
        let compilation = cx.read(owner.preparation_query());
        let source = cx.read(owner.source.query());
        let source = source?;
        if source.address != owner.source.query().address || owner.source.outcome(source).is_none()
        {
            return Err(owner.errors.protocol);
        }
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
        Ok(())
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
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = &self.terminal {
            return result.clone();
        }
        let result = match self.advance(cx) {
            Ok(()) => self
                .owner
                .completed
                .get()
                .map(|completed| completed.product.clone())
                .ok_or(self.owner.errors.protocol),
            Err(error) => Err(error),
        };
        self.terminal = Some(result.clone());
        result
    }
}

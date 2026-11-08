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
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<(), Reference> {
        let owner = &self.owner;
        let check = owner.run.source_check().ok_or(owner.errors.protocol)?;
        let prepared = cx.read(owner.preparation_query());
        let accepted = cx.read(check.query());
        let checked = accepted?;
        if check.accepted(checked.clone()) != Some(true) {
            return Err(check
                .rejection_reference(&checked)
                .ok_or(owner.errors.protocol)?);
        }
        let prepared = prepared?;
        if prepared.address != owner.preparation_query().address
            || owner.outcome(prepared.clone()).is_none()
        {
            return Err(owner.errors.protocol);
        }
        let facts = owner
            .outcome(prepared.clone())
            .ok_or(owner.errors.protocol)?;
        owner
            .run
            .publish_execution(&facts.products.run, &checked)
            .map_err(|error| owner.fail(error))?;
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
                .accepted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or(self.owner.errors.protocol),
            Err(error) => Err(error),
        };
        self.terminal = Some(result.clone());
        result
    }
}

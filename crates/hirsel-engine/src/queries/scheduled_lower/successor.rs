use super::*;

/// An admitted producer and its immutable capture, explicitly descended from
/// the consumer's capture. Keeping the producer retains its input reservation.
#[derive(Clone)]
pub(crate) struct SuccessorCapture {
    previous: ContentHash,
    owner: Arc<FrontLowerQueries>,
}
impl SuccessorCapture {
    pub(crate) fn new(
        previous: &NamespaceCapture,
        owner: Arc<FrontLowerQueries>,
    ) -> Result<Self, QueryError> {
        if !previous.admits_successor(&owner.namespace) {
            return Err(refused(
                "successor changes existing source bindings or lacks admission",
            ));
        }
        Ok(Self {
            previous: previous.identity(),
            owner,
        })
    }
    pub(crate) fn namespace(&self) -> &Arc<NamespaceCapture> {
        &self.owner.namespace
    }
    pub(super) fn owner(&self) -> &Arc<FrontLowerQueries> {
        &self.owner
    }
    pub(crate) fn follows(&self, previous: &NamespaceCapture) -> bool {
        self.previous == previous.identity() && previous.admits_successor(self.namespace())
    }
    pub(crate) fn revalidate(
        &self,
        owner: &DefPath,
        reads: &Reads,
        execution: Option<&hirsel_validate::ExecutionReceipt>,
    ) -> Result<(Reads, Option<hirsel_validate::ExecutionReceipt>), QueryError> {
        for registration in self.namespace().registrations.values() {
            registration
                .validate_current(self.namespace(), |path, key| self.owner.observe(path, key))?;
        }
        // Semantic reads cannot be refreshed by V. A changed body, shape,
        // header or lookup requires the suspended computation to run again.
        if let Some(key) = reads.first_changed(|key| {
            if matches!(key, ReadKey::ValidationUniverse(_)) && execution.is_some() {
                reads.get(key)
            } else {
                self.owner.observe(owner, key)
            }
        }) {
            return Err(refused(&format!(
                "changed semantic input requires recomputation: {key:?}"
            )));
        }
        let execution = execution
            .map(|receipt| {
                self.namespace()
                    .revalidate_execution(receipt)
                    .ok_or_else(|| refused("V refused execution in the successor environment"))
            })
            .transpose()?;
        let reads = evidence::current_reads(reads.clone(), execution.as_ref())
            .ok_or_else(|| refused("successor execution does not cover its validation reads"))?;
        Ok((reads, execution))
    }
}
pub(super) fn refused(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "namespace capture join".into(),
        message: message.into(),
    })
}

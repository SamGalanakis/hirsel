//! Check reads the admitted outcome after admission retires its runtime node.
use super::*;

pub(in crate::queries) struct CheckFacts {
    pub reads: Arc<Reads>,
    pub phase: Option<QueryKind>,
    pub result: Result<(Vec<Diagnostic>, ProductRef), (Reference, QueryError)>,
}
impl FrontLowerQueries {
    fn check_failure(&self, id: QueryId) -> Option<Reference> {
        let forwarded = self
            .forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .cloned();
        if let Some(row) = forwarded {
            return row.alias.err();
        }
        let outcomes = self.outcomes.lock().unwrap_or_else(|e| e.into_inner());
        let (reference, outcome, _) = outcomes.get(&(id.kind, id.address.0))?;
        outcome.result.as_ref().err()?;
        *reference
    }
    fn check_facts(
        &self,
        id: QueryId,
        selected: Result<ProductRef, Reference>,
    ) -> Option<CheckFacts> {
        let reference = selected.as_ref().err().copied();
        let outcome = self.selected(id, selected)?;
        let result = match &outcome.result {
            Ok(value) => Ok((
                match value {
                    Value::Lower(lower) => lower.diagnostics.clone(),
                    Value::Constant(_) => Vec::new(),
                },
                outcome.observations.clone()?,
            )),
            Err(error) => Err((reference?, error.clone())),
        };
        Some(CheckFacts {
            reads: Arc::new(outcome.reads(self)?),
            phase: outcome.phase,
            result,
        })
    }

    /// The outcome table already owns and charges the complete descriptor.
    /// Inspecting it does not reopen a consumed producer or retain its TIR.
    /// Its owner stays with Check's observations until their final consumer.
    pub(in crate::queries) fn poll_check(
        &self,
        cx: &mut dyn QueryContext,
        id: QueryId,
    ) -> Poll<Result<CheckFacts, Reference>> {
        // Successful live products already survive runtime consumption. Their
        // selected identity still checks publication, which may itself refuse.
        // A failed descriptor returns its exact reference after admission has
        // consumed the node; asking the runtime again would repeat the work.
        let selected = match self.check_failure(id) {
            Some(reference) => Err(reference),
            None => match cx.read(id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(selected) => selected,
            },
        };
        let Some(facts) = self.check_facts(id, selected.clone()) else {
            return Poll::Ready(Err(selected
                .err()
                .unwrap_or_else(|| self.protocol_refusal())));
        };
        cx.consume(id);
        Poll::Ready(Ok(facts))
    }
}

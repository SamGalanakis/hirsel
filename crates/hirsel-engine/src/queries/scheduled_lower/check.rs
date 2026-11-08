//! Check reads the admitted outcome after admission retires its runtime node.
use super::*;

pub(in crate::queries) struct CheckFacts {
    pub query: QueryId,
    pub producer: CheckProducer,
    pub reads: Arc<Reads>,
    pub phase: Option<QueryKind>,
    pub result: Result<(Vec<Diagnostic>, ProductRef), (Reference, QueryError)>,
}
#[derive(Clone)]
pub(in crate::queries) struct CheckProducer {
    pub owner: Arc<FrontLowerQueries>,
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
        self: &Arc<Self>,
        id: QueryId,
        selected: Result<ProductRef, Reference>,
    ) -> Option<CheckFacts> {
        if let Some(row) = self.delegated(id, &selected) {
            let mut facts = row.owner.check_facts(row.id, row.selected)?;
            facts.query = id;
            return Some(facts);
        }
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
            query: id,
            producer: CheckProducer {
                owner: self.clone(),
            },
            reads: Arc::new(outcome.reads(self)?),
            phase: outcome.phase,
            result,
        })
    }

    /// The outcome table already owns and charges the complete descriptor.
    /// Inspecting it does not reopen a consumed producer or retain its TIR.
    /// Its owner stays with Check's observations until their final consumer.
    pub(in crate::queries) fn poll_check(
        self: &Arc<Self>,
        cx: &mut dyn QueryContext,
        id: QueryId,
    ) -> Result<CheckFacts, Reference> {
        // Successful live products already survive runtime consumption. Their
        // selected identity still checks publication, which may itself refuse.
        // A failed descriptor returns its exact reference after admission has
        // consumed the node; asking the runtime again would repeat the work.
        let selected = match self.check_failure(id) {
            Some(reference) => Err(reference),
            None => cx.read(id),
        };
        let Some(facts) = self.check_facts(id, selected.clone()) else {
            return Err(selected.err().unwrap_or_else(|| self.protocol_refusal()));
        };
        cx.consume(id);
        Ok(facts)
    }
}

impl FrontLowerQueries {
    /// Read an outcome already produced or admitted in this fixed capture.
    /// Cache adoption owns V revalidation; handoff cannot refresh its reads.
    pub(in crate::queries) fn check_observations(
        &self,
        id: QueryId,
        reads: Arc<Reads>,
    ) -> Result<Arc<Reads>, QueryError> {
        let forwarded = self
            .forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .cloned();
        if let Some(row) = forwarded {
            return row.owner.check_observations(row.id, reads);
        }
        let path = self
            .path(id)
            .ok_or_else(|| successor::refused("missing Lower observation owner"))?;
        let outcome = self
            .outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .map(|(_, outcome, _)| outcome.clone())
            .ok_or_else(|| successor::refused("missing Lower observation outcome"))?;
        let own_reads = outcome
            .reads(self)
            .ok_or_else(|| successor::refused("missing Lower observations"))?;
        if own_reads != *reads
            || reads
                .first_changed(|key| self.observe(&path, key))
                .is_some()
        {
            return Err(successor::refused(
                "Lower observations differ from the admitted capture",
            ));
        }
        if self.verify == Verify::On && outcome.executed {
            let execution = outcome
                .execution
                .as_ref()
                .ok_or_else(|| successor::refused("executed Lower lacks admitted V evidence"))?;
            evidence::persisted_reads(&reads, Some(execution)).ok_or_else(|| {
                successor::refused("execution does not cover its validation reads")
            })?;
        }
        Ok(reads)
    }
}

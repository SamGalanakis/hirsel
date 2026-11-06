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
    pub query: QueryId,
}
pub(in crate::queries) enum CheckRefresh {
    Joined(Arc<Reads>),
    Waiting(QueryId),
    Recomputed(DefPath, CheckFacts),
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
                query: id,
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

impl FrontLowerQueries {
    pub(in crate::queries) fn check_current(self: &Arc<Self>) -> Result<Arc<Self>, QueryError> {
        match self.demand_producer() {
            Some(producer) => producer.current(),
            None => Ok(self.clone()),
        }
    }

    /// Check includes generated registrations' execution and layout reads,
    /// which Front's deliberately partial cache observer cannot inspect.
    pub(in crate::queries) fn check_read(
        &self,
        path: &DefPath,
        key: &ReadKey,
    ) -> Option<ContentHash> {
        self.observe(path, key)
    }

    /// A failed join is a reuse miss. Demand the ordinary producer in the
    /// committed successor, which performs its normal semantic checks and V.
    pub(in crate::queries) fn refresh_check(
        self: &Arc<Self>,
        cx: &mut dyn QueryContext,
        id: QueryId,
        reads: Arc<Reads>,
    ) -> Result<CheckRefresh, Reference> {
        let error = match self.check_observations(id, reads) {
            Ok(reads) => return Ok(CheckRefresh::Joined(reads)),
            Err(error) => error,
        };
        let protocol = |error: QueryError| {
            self.protocol_failure(
                id,
                &format!("Check Lower observations cannot join: {error:?}"),
            )
        };
        let current = self
            .demand_producer()
            .ok_or_else(|| protocol(error.clone()))?
            .current()
            .map_err(protocol)?;
        let successor = SuccessorCapture::new(&self.namespace, current).map_err(protocol)?;
        let owner = successor.owner();
        let path = self.path(id).ok_or_else(|| protocol(error.clone()))?;
        let actual = match self.staged.selection(id) {
            Some(selection) => owner
                .staged_query(selection.declaration.path(), &selection.arguments)
                .map_err(protocol)?,
            None => owner
                .id(&path, id.kind)
                .ok_or_else(|| protocol(error.clone()))?,
        };
        if actual == id {
            return Err(protocol(error));
        }
        let selected = match cx.read(actual) {
            Poll::Pending => return Ok(CheckRefresh::Waiting(actual)),
            Poll::Ready(selected) => selected,
        };
        let mut facts = owner.check_facts(actual, selected.clone()).ok_or_else(|| {
            selected
                .clone()
                .err()
                .unwrap_or_else(|| owner.protocol_refusal())
        })?;
        cx.consume(actual);
        facts.query = id;
        Ok(CheckRefresh::Recomputed(path, facts))
    }

    /// Join the selected producer's observations to the completed demand scope.
    /// V refreshes validation guards only after semantic reads still agree.
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
        let Some(producer) = self.demand_producer() else {
            return Ok(reads);
        };
        let current = producer.current()?;
        let path = self
            .path(id)
            .ok_or_else(|| successor::refused("missing Lower observation owner"))?;
        if reads
            .first_changed(|key| current.observe(&path, key))
            .is_none()
        {
            return Ok(reads);
        }
        let successor = SuccessorCapture::new(&self.namespace, current)?;
        let outcome = self
            .outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.kind, id.address.0))
            .map(|(_, outcome, _)| outcome.clone())
            .ok_or_else(|| successor::refused("missing Lower observation outcome"))?;
        let execution = outcome.execution.clone().or_else(|| {
            let Ok(Value::Lower(lower)) = &outcome.result else {
                return None;
            };
            let proof = lower.acceptance.as_ref()?;
            let mut contexts: Vec<_> = reads
                .iter()
                .filter_map(|(key, value)| match key {
                    ReadKey::ValidationUniverse(package) => Some((package.id(), *value)),
                    _ => None,
                })
                .collect();
            contexts.sort_unstable();
            hirsel_validate::ExecutionReceipt::compose(&contexts, std::slice::from_ref(proof))
        });
        let (reads, _) = successor
            .revalidate(&path, &reads, execution.as_ref())
            .map_err(|error| successor::refused(&format!("{path}: {error:?}")))?;
        Ok(Arc::new(reads))
    }
}

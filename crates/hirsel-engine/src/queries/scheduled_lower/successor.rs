use super::*;

/// An admitted producer and its immutable capture, explicitly descended from
/// the consumer's capture. Keeping the producer retains its input reservation.
#[derive(Clone)]
pub(crate) struct SuccessorCapture {
    previous: ContentHash,
    owner: Arc<FrontLowerQueries>,
}
#[derive(Clone)]
pub(crate) struct WeakSuccessorCapture {
    previous: ContentHash,
    owner: std::sync::Weak<FrontLowerQueries>,
}
impl WeakSuccessorCapture {
    pub(crate) fn upgrade(&self) -> Option<SuccessorCapture> {
        Some(SuccessorCapture {
            previous: self.previous,
            owner: self.owner.upgrade()?,
        })
    }
}
impl SuccessorCapture {
    pub(crate) fn downgrade(&self) -> WeakSuccessorCapture {
        WeakSuccessorCapture {
            previous: self.previous,
            owner: Arc::downgrade(&self.owner),
        }
    }
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
    pub(in crate::queries) fn owner(&self) -> &Arc<FrontLowerQueries> {
        &self.owner
    }
    pub(crate) fn follows(&self, previous: &NamespaceCapture) -> bool {
        self.previous == previous.identity() && previous.admits_successor(self.namespace())
    }
    pub(super) fn changed_input(
        &self,
        owner: &DefPath,
        reads: &Reads,
        executed: bool,
    ) -> Option<ReadKey> {
        reads.first_changed(|key| {
            if matches!(key, ReadKey::ValidationUniverse(_)) && executed {
                reads.get(key)
            } else {
                self.owner.observe(owner, key)
            }
        })
    }
    pub(crate) fn revalidate(
        &self,
        owner: &DefPath,
        reads: &Reads,
        execution: Option<&hirsel_validate::ExecutionReceipt>,
    ) -> Result<(Reads, Option<hirsel_validate::ExecutionReceipt>), QueryError> {
        self.namespace()
            .admit_reuse(owner, reads, execution, |path, key| {
                self.owner.observe(path, key)
            })
    }
}
pub(super) fn refused(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "namespace capture join".into(),
        message: message.into(),
    })
}

impl FrontLowerQueries {
    pub(super) async fn forward_capture(
        &self,
        id: QueryId,
        actual: QueryId,
        owner: Arc<FrontLowerQueries>,
        reads: &QueryReads,
    ) -> Result<ProductRef, Reference> {
        if actual == id {
            return Err(self.errors.protocol);
        }
        let selected = reads.read_consuming(actual).await;
        let alias = selected.clone().map(|mut product| {
            product.address = id.address;
            product
        });
        self.forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (id.kind, id.address.0),
                ForwardedLower {
                    id: actual,
                    owner,
                    selected,
                    alias: alias.clone(),
                },
            );
        alias
    }
}

impl FrontLowerQueries {
    pub(super) fn discovery_completed_owner(&self, path: &DefPath, kind: u32) -> Option<Arc<Self>> {
        if self.namespace.consumers_admitted() {
            return None;
        }
        let mut pending = self
            .forwarded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|row| row.owner.clone())
            .collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            if !seen.insert(owner.namespace.identity()) {
                continue;
            }
            if owner.id(path, kind).is_some_and(|id| {
                owner
                    .outcomes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&(id.kind, id.address.0))
            }) {
                return Some(owner);
            }
            let next = owner
                .forwarded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .map(|row| row.owner.clone())
                .collect::<Vec<_>>();
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.tree::<ContentHash, ()>(seen.len());
            weight.add(3 * (pending.len() + next.len()) * size_of::<Arc<Self>>());
            if weight.bytes() > self.weight.scratch {
                return None;
            }
            pending.extend(next);
        }
        None
    }
}

//! Per-definition semantic products shared by Run closures and precomputation.
//! These producers demand P/Lower/Evaluate directly and never construct launch
//! metadata or require the program Check root to complete.
use super::*;

const OUTPUT: Domain = Domain {
    kind: 0x4600_0034,
    schema: 3,
};

pub(super) struct SemanticOwnership {
    context: crate::NativeRecordContext,
    remaining: Arc<Mutex<u64>>,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    bytes: u64,
}
impl Drop for SemanticOwnership {
    fn drop(&mut self) {
        *self
            .remaining
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += self.bytes;
    }
}
pub(super) struct LiveSemantic {
    entry: Arc<GraphEntry>,
    path: DefId,
    records: crate::NativeRecords,
    pub(super) ownership: Arc<SemanticOwnership>,
    charge: u64,
    store: Arc<dyn ResultStore>,
}
impl LiveProductValue for LiveSemantic {
    fn domain(&self) -> Domain {
        OUTPUT
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> std::io::Result<ProductArchive> {
        ProductArchive::materialize(OUTPUT, admission, |grant| {
            if grant.bytes() < self.charge {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            let source = self
                .entry
                .source
                .as_ref()
                .map(|source| {
                    source
                        .export(&self.records, &self.ownership.context, &*self.store, grant)
                        .ok_or_else(|| std::io::Error::other("semantic source encoding"))
                })
                .transpose()?;
            let content::CanonicalNode {
                identity,
                content,
                dependencies,
            } = self
                .entry
                .node
                .canonical(self.path.path(), &*self.store, grant, grant.bytes())
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            let value = (
                identity,
                content,
                dependencies,
                self.entry.content.references(),
                source,
                &self.entry.node.observations,
            );
            let length = bincode::serialized_size(&value).map_err(std::io::Error::other)?;
            if length > grant.bytes() {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            bincode::serialize(&value).map_err(std::io::Error::other)
        })
    }
}

impl FrontRunQueries {
    /// The source-owned semantic node, independent of a launch or Check root.
    pub fn semantic_query(&self, path: &DefPath) -> Option<QueryId> {
        self.semantic_ids.get(path).copied()
    }
    /// Read one exact completed semantic node without demanding any query.
    pub fn semantic_node(
        &self,
        product: ProductRef,
    ) -> Result<hirsel_run_schema::ProgramNode, QueryError> {
        let entry = self
            .semantic_entry(product.clone())
            .ok_or_else(|| operational("semantic product is unavailable"))?;
        let path = self
            .semantic_paths
            .get(&product.address.0)
            .ok_or_else(|| operational("semantic product has another owner"))?
            .path();
        entry.node.program(self, path)
    }
    pub(super) fn semantic_entry(&self, product: ProductRef) -> Option<Arc<GraphEntry>> {
        self.semantic_completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&product.address.0)
            .and_then(|(selected, entry)| (*selected == product).then(|| entry.clone()))
    }
}

pub(super) fn create(queries: &Arc<FrontRunQueries>, query: QueryId) -> Option<Box<dyn QueryTask>> {
    if query.kind != FRONT_RUN_KIND {
        return None;
    }
    let path = queries.semantic_paths.get(&query.address.0)?.path().clone();
    let terminal = queries
        .semantic_completed
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&query.address.0)
        .map(|(product, _)| Ok(product.clone()));
    Some(Box::new(SemanticTask {
        node: Task {
            queries: queries.clone(),
            query,
            root: path,
            started: false,
            pending: Vec::new(),
            entries: BTreeMap::new(),
            input: None,
            waiting: None,
            terminal,
        },
    }))
}
struct SemanticTask {
    node: Task,
}
impl QueryTask for SemanticTask {
    fn id(&self) -> QueryId {
        self.node.query
    }
    fn weight(&self) -> MemoryWeight {
        let weight = self.node.queries.weight;
        MemoryWeight {
            retained: weight.retained / 2,
            ..weight
        }
    }
    fn continuation_bytes(&self) -> u64 {
        // Completed nodes live in semantic_completed under the independent
        // source-output reservation, just like settled Lower products.
        self.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        let result = if let Some(result) = self.node.terminal.clone() {
            result
        } else {
            let path = self.node.root.clone();
            match self.node.node(cx, &path) {
                Poll::Pending => {
                    return QueryPoll::Waiting(self.node.waiting.into_iter().collect());
                }
                Poll::Ready(Err(error)) => Err(error),
                Poll::Ready(Ok((node, content, dependencies, source))) => self
                    .complete(node, content, dependencies, source)
                    .map_err(|error| self.node.queries.failure_record(error)),
            }
        };
        // The completed immutable values own the semantic result. Neither its
        // decoded input nor its raw Front/Lower dependencies remain consumers.
        self.node.input = None;
        if result.is_ok() {
            let path = &self.node.root;
            for query in [
                self.node.queries.front.query(path),
                self.node.queries.front.body_query(path),
                self.node.queries.lower.query(path),
                self.node.queries.lower.evaluate_query(path),
            ]
            .into_iter()
            .flatten()
            {
                cx.consume(query);
            }
        }
        self.node.terminal = Some(result.clone());
        match result {
            Ok(product) => QueryPoll::Complete(product),
            Err(error) => QueryPoll::Failed(error),
        }
    }
}
impl SemanticTask {
    fn complete(
        &self,
        node: content::Node,
        content: DefinitionContent,
        dependencies: BTreeSet<DefPath>,
        source: Option<source_products::Body>,
    ) -> Result<ProductRef, QueryError> {
        let queries = &self.node.queries;
        if queries.precompute.is_some() {
            queries.publish_installed_input(
                &self.node.root,
                self.node.input.as_ref().ok_or(QueryError::NotImplemented)?,
            )?;
        }
        let mut entry = GraphEntry {
            source: source.map(|body| source_products::Source::Live(Arc::new(body))),
            content: content::Content::new(queries, content, &node)?,
            node,
            dependencies: dependencies
                .iter()
                .map(DefId::intern)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        };
        let charge_entry = |entry: &GraphEntry| {
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.tree::<ContentHash, (ProductRef, Arc<GraphEntry>)>(1);
            weight.add(
                std::mem::size_of::<GraphEntry>()
                    + std::mem::size_of::<LiveSemantic>()
                    + std::mem::size_of::<SemanticOwnership>()
                    + 6 * std::mem::size_of::<usize>(),
            );
            weight.add(std::mem::size_of_val(&*entry.dependencies));
            entry.node.weight(&mut weight);
            entry.content.weight(&mut weight);
            if let Some(source) = &entry.source {
                weight.add(source.weight() as usize);
            }
            weight.bytes()
        };
        let mut charge = charge_entry(&entry);
        let remaining = *queries
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if charge > queries.weight.retained / 2
            || charge > remaining.saturating_sub(queries.weight.scratch)
        {
            entry.node.spill(queries)?;
            entry.content.spilled_node(&entry.node);
            if let Some(source) = &mut entry.source {
                source.spill(queries)?;
            }
            charge = charge_entry(&entry);
        }
        if charge > queries.weight.retained / 2 {
            return Err(operational("semantic product exceeds retained admission"));
        }
        let entry = Arc::new(entry);
        // Hash the canonical semantic preimage directly. Identity encoding does
        // not create a portable archive or consult the result provider.
        let content::CanonicalNode {
            identity,
            content,
            dependencies,
        } = entry.node.canonical(
            &self.node.root,
            &*queries.store,
            &queries.ownership._allowance,
            queries.weight.scratch,
        )?;
        let canonical = (
            identity,
            content,
            dependencies,
            entry.content.references(),
            &entry.node.observations,
        );
        let length =
            bincode::serialized_size(&canonical).map_err(|error| operational(error.to_string()))?;
        if length > queries.weight.scratch {
            return Err(operational("semantic identity exceeds scratch admission"));
        }
        let identity =
            ContentHash::of_stream("hirsel.front.semantic.identity.v1", length, |writer| {
                bincode::serialize_into(writer, &canonical).map_err(std::io::Error::other)
            })
            .map_err(|error| operational(error.to_string()))?;
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.semantic.action.v2",
            [
                self.node.query.address.0.0.as_slice(),
                identity.0.as_slice(),
            ],
        ));
        let mut remaining = queries
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *remaining = remaining
            .checked_sub(charge)
            .ok_or_else(|| operational("semantic products exceed admission"))?;
        drop(remaining);
        let ownership = Arc::new(SemanticOwnership {
            context: queries.anchors.clone(),
            remaining: queries.retention.clone(),
            _allowance: queries.ownership._allowance.clone(),
            bytes: charge,
        });
        let product = ProductRef::live(
            self.node.query.address,
            action,
            Arc::new(LiveSemantic {
                entry: entry.clone(),
                path: DefId::intern(&self.node.root),
                records: queries.records.clone(),
                ownership,
                charge,
                store: queries.store.clone(),
            }),
        );
        queries
            .semantic_completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(product.address.0, (product.clone(), entry));
        Ok(product)
    }
}

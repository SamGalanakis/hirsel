//! Source GEN-5 over every required checked root. Native reachability never
//! selects this root set. Pending reads retain the frontier and compact facts;
//! factories and output inspection execute no compiler phase.
use super::*;
use crate::runtime::*;
use crate::source_components::{SourceGrowthComponents, SourceGrowthQueryRecord, SourceMembership};
use crate::store::{Domain, Reference, ResultStore};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    task::Poll,
};
pub const FRONT_GROWTH_KIND: u32 = 0x4600_0008;
const OBSERVATIONS: Domain = Domain {
    kind: 0x4600_0023,
    schema: 1,
};
const FAILURE: Domain = Domain {
    kind: 0x4600_000c,
    schema: 1,
};
pub struct FrontGrowthQueries {
    query: QueryId,
    roots: Vec<QueryId>,
    front: Arc<FrontBodyQueries>,
    items: Arc<crate::NativeRecordContext>,
    records: crate::NativeRecords,
    sources: SourceMap,
    snapshot: Arc<SignatureSnapshot>,
    package: hirsel_syntax::PackageId,
    store: Arc<dyn ResultStore>,
    completed: Mutex<Option<ProductRef>>,
    membership: Mutex<Option<ProductRef>>,
    components: Mutex<SourceGrowthComponents>,
    publication_source: Mutex<Option<Arc<GrowthPublicationSource>>>,
    solves: AtomicU64,
    memberships: AtomicU64,
    failure: Mutex<Option<(Reference, QueryError)>>,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    output_pool: MemoryReservationPool,
    _allowance: MemoryReservation,
}
#[derive(Clone)]
pub struct FrontGrowthOutput {
    pub facets: Arc<SourceGrowthFacets>,
    pub diagnostics: Arc<Vec<Diagnostic>>,
    pub reads: Arc<Reads>,
    _product: ProductRef,
}
impl FrontGrowthQueries {
    /// Root handles and the owner itself. The exact front keeps source paths,
    /// anchors and body outputs; the admitted task keeps its execution frame.
    pub fn input_bytes(definitions: &[DefPath], _weight: MemoryWeight) -> Option<u64> {
        let package = definitions
            .iter()
            .map(|path| path.0.package.name.len() + path.0.package.version.len())
            .max()
            .unwrap_or(0);
        (std::mem::size_of::<Self>() as u64)
            .checked_add((2 * std::mem::size_of::<usize>()) as u64)?
            .checked_add(MemoryReservationPool::metadata_bytes())?
            .checked_add(package as u64)?
            .checked_add(
                (definitions.len() as u64).checked_mul(std::mem::size_of::<QueryId>() as u64)?,
            )
    }
    fn producer(&self) -> SourceGrowthProducer<'_> {
        SourceGrowthProducer {
            source: &self.items,
            output_pool: &self.output_pool,
            namespace: self.query.address,
            scratch: self.weight.scratch,
        }
    }
    pub fn failure(&self, reference: Reference) -> Option<QueryError> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|(selected, error)| (*selected == reference).then(|| error.clone()))
    }
    pub fn component_solves(&self) -> u64 {
        self.solves.load(Relaxed)
    }
    pub fn membership_solves(&self) -> u64 {
        self.memberships.load(Relaxed)
    }
    fn membership_query(&self) -> QueryId {
        QueryId {
            kind: FRONT_GROWTH_KIND,
            address: self.producer().membership_address(),
        }
    }
    pub fn query(&self) -> QueryId {
        self.query
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontGrowthOutput> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let selected = completed.as_ref()?;
        let data = product.growth_value()?.value();
        (*selected == product).then(|| FrontGrowthOutput {
            facets: data.facets.clone(),
            diagnostics: data.diagnostics.clone(),
            reads: data.reads.clone(),
            _product: product.clone(),
        })
    }
}
struct Factory(Arc<FrontGrowthQueries>);
impl QueryFactory for Factory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        let queries = self.0.clone();
        let cached = if id == queries.query {
            queries
                .completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else if id == queries.membership_query() {
            queries
                .membership
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else {
            let value = queries
                .components
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|value| value.product.address == id.address)
                .cloned()
                .or_else(|| {
                    queries
                        .completed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()?
                        .growth_value()?
                        .value()
                        .facets
                        .components
                        .iter()
                        .find(|value| value.product.address == id.address)
                        .cloned()
                });
            value
                .and_then(LiveSourceGrowthProduct::reopen_component)
                .map(ProductRef::source_growth)
        };
        if let Some(product) = cached {
            return Ok(Box::new(Reopened { product }));
        }
        if id == queries.query {
            return Ok(Box::new(Task {
                queries,
                membership: None,
                component_cursor: 0,
                cursor: 0,
                table_keys: BTreeMap::new(),
                required: BTreeMap::new(),
                reads: ReadsBuilder::new(),
                waiting: None,
                terminal: None,
            }));
        }
        let component = if id == queries.membership_query() {
            None
        } else {
            let selected = queries.membership.lock().unwrap_or_else(|e| e.into_inner());
            let SourceGrowthQueryData::Membership { value, .. } = selected
                .as_ref()
                .and_then(ProductRef::source_growth_value)
                .map(LiveSourceGrowthProduct::value)
                .ok_or(queries.errors.protocol)?
            else {
                return Err(queries.errors.protocol);
            };
            let index = value
                .components
                .iter()
                .position(|c| c.address == id.address.0)
                .ok_or(queries.errors.protocol)?;
            Some((value.clone(), index))
        };
        if id.kind != FRONT_GROWTH_KIND {
            return Err(queries.errors.protocol);
        }
        if queries.weight.retained
            < (std::mem::size_of::<FactTask>() + 2 * std::mem::size_of::<usize>()) as u64
        {
            return Err(queries.errors.resource);
        }
        let count = component
            .as_ref()
            .map_or(queries.roots.len(), |(m, i)| m.components[*i].inputs.len());
        let bytes = (count as u64)
            .checked_mul(
                (std::mem::size_of::<SourceGrowthFact>() + std::mem::size_of::<Arc<Reads>>())
                    as u64,
            )
            .ok_or(queries.errors.resource)?;
        let admission = queries
            .output_pool
            .take(bytes)
            .ok_or(queries.errors.resource)?;
        Ok(Box::new(FactTask {
            queries,
            id,
            component,
            count,
            waiting: None,
            cursor: 0,
            facts: Vec::with_capacity(count),
            reads: Vec::with_capacity(count),
            terminal: None,
            admission,
        }))
    }
}
struct Reopened {
    product: ProductRef,
}
impl QueryTask for Reopened {
    fn id(&self) -> QueryId {
        QueryId {
            kind: FRONT_GROWTH_KIND,
            address: self.product.address,
        }
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: (size_of::<Self>() + 2 * size_of::<usize>()) as u64,
            ..MemoryWeight::default()
        }
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, _: &mut dyn QueryContext) -> QueryPoll {
        QueryPoll::Complete(self.product.clone())
    }
}
fn finish(result: Result<ProductRef, Reference>) -> QueryPoll {
    match result {
        Ok(product) => QueryPoll::Complete(product),
        Err(error) => QueryPoll::Failed(error),
    }
}
struct FactTask {
    queries: Arc<FrontGrowthQueries>,
    id: QueryId,
    component: Option<(Arc<SourceMembership>, usize)>,
    count: usize,
    waiting: Option<QueryId>,
    cursor: usize,
    facts: Vec<SourceGrowthFact>,
    reads: Vec<Arc<Reads>>,
    terminal: Option<Result<ProductRef, Reference>>,
    admission: MemoryReservation,
}
impl FactTask {
    fn input(&self, i: usize) -> QueryId {
        self.component
            .as_ref()
            .map_or(self.queries.roots.get(i).copied(), |(m, index)| {
                Some(QueryId {
                    kind: FRONT_BODY_KIND,
                    address: hirsel_artifact_store::Address(m.components[*index].inputs[i]),
                })
            })
            .unwrap()
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Poll<Result<ProductRef, Reference>> {
        while self.cursor < self.count {
            let input = self.input(self.cursor);
            self.waiting = Some(input);
            let selected = match cx.read(input) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(selected) => selected,
            };
            if let Some(checked) = self.queries.front.checked(selected.clone()) {
                let Some(reads) = self.queries.publication_source().ok().and_then(|source| {
                    self.queries
                        .front
                        .retained_reads_in(cx, &checked.reads, &source.reads)
                }) else {
                    return Poll::Ready(Err(self.queries.errors.resource));
                };
                self.reads.push(reads);
                let observed = selected
                    .as_ref()
                    .map(|p| p.action.0)
                    .unwrap_or_else(|r| r.output.0);
                let Some(fact) = self.queries.producer().fact_observed(
                    self.input(self.cursor).address,
                    checked.summary,
                    selected.into(),
                    observed,
                ) else {
                    return Poll::Ready(Err(self.queries.errors.resource));
                };
                let bytes = fact.input_bytes() - std::mem::size_of::<SourceGrowthFact>() as u64;
                let Some(charge) = self.queries.output_pool.take(bytes) else {
                    return Poll::Ready(Err(self.queries.errors.resource));
                };
                if self.admission.join(charge).is_err() {
                    return Poll::Ready(Err(self.queries.errors.protocol));
                }
                self.facts.push(fact);
            } else if let Some(facts) = self.queries.front.check_facts(selected.clone())
                && matches!(facts.result, Err(QueryError::Diagnostics(_)))
            {
                let Some(reads) = self.queries.publication_source().ok().and_then(|source| {
                    self.queries
                        .front
                        .retained_reads_in(cx, &facts.reads, &source.reads)
                }) else {
                    return Poll::Ready(Err(self.queries.errors.resource));
                };
                self.reads.push(reads);
            } else {
                return Poll::Ready(Err(selected.err().unwrap_or(self.queries.errors.protocol)));
            }
            cx.consume(input);
            self.cursor += 1;
        }
        self.waiting = None;
        Poll::Ready(self.complete(cx))
    }
    fn complete(&mut self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        self.facts
            .sort_by_key(|fact| (fact.owner, fact.definition.0.0));
        // Input observations own their growth-pool grants across suspension.
        // The flat union exists only during this admitted scratch frame.
        let mut union = ReadsBuilder::new();
        for reads in &self.reads {
            union.merge(reads);
        }
        let union = union.finish().map_err(|_| queries.errors.protocol)?;
        let reads = self
            .reads
            .iter()
            .find(|reads| reads.as_ref() == &union && reads.observed().eq(union.observed()))
            .cloned()
            .unwrap_or_else(|| Arc::new(union));
        let reads = queries
            .front
            .retained_reads_in(cx, &reads, &queries.publication_source()?.reads)
            .ok_or(queries.errors.resource)?;
        let producer = queries.producer();
        let (identity, data) = if let Some((membership, index)) = &self.component {
            let members = &membership.components[*index].members;
            let action = producer
                .component_action(members, &self.facts)
                .ok_or(queries.errors.resource)?;
            let cached = queries
                .store
                .lookup_product(self.id.address)
                .filter(|a| a.action == action && a.reference.domain == LIVE_SOURCE_COMPONENT)
                .and_then(|a| queries.load_record(a.reference));
            let value = if let Some(SourceGrowthQueryRecord::Component {
                product,
                members: stored,
                growing,
                ..
            }) = cached
            {
                if stored.as_ref() != members.as_ref()
                    || product.address != self.id.address
                    || product.action != action
                {
                    return Err(queries.errors.protocol);
                }
                let value = producer
                    .component_value(stored, growing, action)
                    .ok_or(queries.errors.resource)?;
                if value.product != product {
                    return Err(queries.errors.protocol);
                }
                value
            } else {
                queries.solves.fetch_add(1, Relaxed);
                producer
                    .component(members, &self.facts)
                    .ok_or(queries.errors.resource)?
            };
            (
                value.product,
                SourceGrowthQueryData::Component { value, reads },
            )
        } else {
            let inputs = producer
                .membership_action(&self.facts)
                .ok_or(queries.errors.resource)?;
            // Rejected roots without a checked summary still contribute every
            // failed lookup and signature observation to this computation.
            let action = hirsel_artifact_store::Action(ContentHash::of_parts(
                "hirsel.source-growth.membership.query-action.v1",
                [
                    inputs.0.0.as_slice(),
                    crate::store::payload_reads_hash(&reads).0.as_slice(),
                ],
            ));
            let cached = queries
                .store
                .lookup_product(self.id.address)
                .filter(|a| a.action == action && a.reference.domain == LIVE_SOURCE_MEMBERSHIP)
                .and_then(|a| queries.load_record(a.reference));
            let value = if let Some(SourceGrowthQueryRecord::Membership {
                product,
                components,
                ..
            }) = cached
            {
                if product.address != self.id.address || product.action != action {
                    return Err(queries.errors.protocol);
                }
                producer
                    .membership_value(product, components)
                    .ok_or(queries.errors.resource)?
            } else {
                queries.memberships.fetch_add(1, Relaxed);
                let mut value = producer
                    .membership(&self.facts)
                    .ok_or(queries.errors.resource)?;
                value.product.action = action;
                value
            };
            (
                value.product,
                SourceGrowthQueryData::Membership {
                    value: Arc::new(value),
                    reads,
                },
            )
        };
        // Drop accepted input facts before charging the settled publication.
        self.facts.clear();
        self.facts.shrink_to_fit();
        self.reads.clear();
        self.reads.shrink_to_fit();
        let old = std::mem::replace(
            &mut self.admission,
            queries.output_pool.take(0).ok_or(queries.errors.resource)?,
        );
        drop(old);
        let value = LiveSourceGrowthProduct::new(
            (identity.address, identity.action),
            data,
            queries.publication_source()?,
        )
        .ok_or(queries.errors.resource)?;
        let product = ProductRef::source_growth(value);
        queries.persist(&product);
        if self.component.is_none() {
            *queries.membership.lock().unwrap_or_else(|e| e.into_inner()) = Some(product.clone());
        }
        Ok(product)
    }
}
impl QueryTask for FactTask {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        // The producer pool already owns both retained input vectors. A new
        // component does not reserve another whole-program root continuation.
        MemoryWeight {
            retained: (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64,
            ..self.queries.weight
        }
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = &self.terminal {
            return finish(result.clone());
        }
        match self.advance(cx) {
            Poll::Pending => QueryPoll::Waiting(self.waiting.into_iter().collect()),
            Poll::Ready(result) => {
                self.terminal = Some(result.clone());
                finish(result)
            }
        }
    }
}
impl FrontGrowthQueries {
    fn publication_source(&self) -> Result<Arc<GrowthPublicationSource>, Reference> {
        let mut source = self
            .publication_source
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(source) = &*source {
            return Ok(source.clone());
        }
        let input = ProductSource::prepare(
            (*self.items).clone(),
            self.records.clone(),
            self.front.clone(),
        )
        .ok_or(self.errors.protocol)?;
        let mut allowance = self
            .output_pool
            .take(LiveSourceGrowthProduct::source_bytes())
            .ok_or(self.errors.resource)?;
        let admitted = LiveSourceGrowthProduct::admit_source(input, &mut allowance)
            .ok_or(self.errors.resource)?;
        *source = Some(admitted.clone());
        Ok(admitted)
    }
    fn load_record(&self, reference: Reference) -> Option<SourceGrowthQueryRecord> {
        if reference.len.checked_mul(8)? > self.weight.scratch {
            return None;
        }
        self.records
            .decode(&*self.items, &self.store.get_content(reference)?)
    }
    fn persist(&self, product: &ProductRef) {
        if !self.store.persistence().retains() {
            return;
        }
        let Some(value) = product.source_growth_value() else {
            return;
        };
        let Some(bytes) = self.records.encode(&*self.items, &value.value()) else {
            return;
        };
        if (bytes.len() as u64)
            .checked_mul(8)
            .is_none_or(|bytes| bytes > self.weight.scratch)
        {
            return;
        }
        self.store
            .cache_product(product.address, product.action, product.domain(), &bytes);
    }
}
struct Task {
    queries: Arc<FrontGrowthQueries>,
    membership: Option<Arc<SourceMembership>>,
    component_cursor: usize,
    cursor: usize,
    table_keys: BTreeMap<DefPath, Arc<crate::table_keys::TableKeyUse>>,
    required: BTreeMap<DefPath, BTreeSet<usize>>,
    reads: ReadsBuilder,
    waiting: Option<QueryId>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.queries.query
    }
    fn weight(&self) -> MemoryWeight {
        self.queries.weight
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = &self.terminal {
            return finish(result.clone());
        }
        match self.advance(cx) {
            Poll::Pending => QueryPoll::Waiting(self.waiting.into_iter().collect()),
            Poll::Ready(result) => {
                self.terminal = Some(result.clone());
                finish(result)
            }
        }
    }
}
impl Task {
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Poll<Result<ProductRef, Reference>> {
        let queries = self.queries.clone();
        if self.membership.is_none() {
            let id = queries.membership_query();
            self.waiting = Some(id);
            let product = match cx.read(id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(p)) => p,
            };
            let Some(SourceGrowthQueryData::Membership { value, reads }) = product
                .source_growth_value()
                .map(LiveSourceGrowthProduct::value)
            else {
                return Poll::Ready(Err(queries.errors.protocol));
            };
            self.reads.merge(&reads);
            self.membership = Some(value.clone());
            cx.consume(id);
        }
        let membership = self.membership.as_ref().unwrap();
        while self.component_cursor < membership.components.len() {
            let id = QueryId {
                kind: FRONT_GROWTH_KIND,
                address: hirsel_artifact_store::Address(
                    membership.components[self.component_cursor].address,
                ),
            };
            self.waiting = Some(id);
            let product = match cx.read(id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(p)) => p,
            };
            let Some(SourceGrowthQueryData::Component { value, reads }) = product
                .source_growth_value()
                .map(LiveSourceGrowthProduct::value)
            else {
                return Poll::Ready(Err(queries.errors.protocol));
            };
            self.reads.merge(&reads);
            let mut components = queries.components.lock().unwrap_or_else(|e| e.into_inner());
            if self.component_cursor == components.len() {
                if components
                    .push(
                        value.clone(),
                        &queries.output_pool,
                        membership.components.len() - self.component_cursor,
                    )
                    .is_none()
                {
                    return Poll::Ready(Err(queries.errors.resource));
                }
            } else if components[self.component_cursor].product != value.product {
                return Poll::Ready(Err(queries.errors.protocol));
            }
            // A restarted reduction reads the retained prefix through the
            // scheduler again, so this generation records every dependency.
            drop(components);
            cx.consume(id);
            self.component_cursor += 1;
        }
        // Table-key acceptance reduces the same complete checked roots. It
        // neither builds membership nor solves a source component.
        while self.cursor < queries.roots.len() {
            let id = queries.roots[self.cursor];
            self.waiting = Some(id);
            let selected = match cx.read(id) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(p) => p,
            };
            if let Some(checked) = queries.front.checked(selected.clone()) {
                for call in &checked.summary.summary.calls {
                    let slots = hirsel_types::core_roles::table_key_parameters(call.callee.path());
                    if !slots.is_empty() {
                        self.required
                            .entry(call.callee.path().clone())
                            .or_default()
                            .extend(slots);
                    }
                }
                self.table_keys.insert(
                    checked.summary.summary.owner.path().clone(),
                    checked.table_keys,
                );
                self.reads.merge(&checked.reads);
            } else if let Some(facts) = queries.front.check_facts(selected.clone())
                && matches!(facts.result, Err(QueryError::Diagnostics(_)))
            {
                self.reads.merge(&facts.reads);
            } else {
                return Poll::Ready(Err(selected.err().unwrap_or(queries.errors.protocol)));
            }
            cx.consume(id);
            self.cursor += 1;
        }
        self.waiting = None;
        Poll::Ready(self.complete(cx))
    }
    fn complete(&mut self, _cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        let mut table_reads = ReadsBuilder::new();
        let mut provider =
            FrozenProvider::new(&queries.snapshot, queries.package.clone(), &mut table_reads);
        let table_result = crate::table_keys::check_uses(
            queries.snapshot.types(),
            &self.table_keys,
            std::mem::take(&mut self.required),
            &mut |definition| provider.signature(definition).map(Some),
        );
        self.reads
            .merge(&table_reads.finish().map_err(|_| queries.errors.protocol)?);
        let reads = std::mem::take(&mut self.reads)
            .finish()
            .map_err(|_| queries.errors.protocol)?;
        let table_diagnostics = match table_result {
            Ok(diagnostics) => diagnostics,
            Err(error) => {
                let bytes = queries
                    .records
                    .encode(&*queries.items, &(&error, &reads))
                    .ok_or(queries.errors.resource)?;
                if bytes.len() as u64 > queries.weight.scratch {
                    return Err(queries.errors.resource);
                }
                let reference = queries
                    .store
                    .put_content(FAILURE, &bytes)
                    .ok_or(queries.errors.resource)?;
                *queries.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((reference, error));
                return Err(reference);
            }
        };
        let membership = self.membership.as_ref().ok_or(queries.errors.protocol)?;
        let facets = queries
            .producer()
            .accept(
                membership.product,
                std::mem::take(&mut *queries.components.lock().unwrap_or_else(|e| e.into_inner())),
            )
            .ok_or(queries.errors.resource)?;
        let diagnostics = crate::select_diagnostics(
            table_diagnostics
                .into_iter()
                .chain(
                    facets
                        .components
                        .iter()
                        .flat_map(|component| component.growing.iter().copied())
                        .map(|site| {
                            Diagnostic::error(
                                const { RuleId::of("GEN-5") },
                                site,
                                "a recursive instantiation cycle grows its type arguments",
                            )
                        }),
                )
                .collect(),
            Some(&queries.sources),
        );
        // Equal settled components still stop propagation even when their
        // checked inputs/actions changed. Complete current reads remain bound.
        let outputs: Vec<_> = facets
            .components
            .iter()
            .map(|component| component.product.output)
            .collect();
        let observed = crate::source_components::hash_source_value(
            &queries.items,
            "hirsel.front.source-growth.value.v1",
            &(
                facets.membership.output,
                &outputs,
                &diagnostics,
                crate::store::payload_reads_hash(&reads),
            ),
        )
        .ok_or(queries.errors.resource)?;
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.source-growth.action.v3",
            [queries.query.address.0.0.as_slice(), observed.0.as_slice()],
        ));
        let membership_reads = &membership
            .publication
            .as_ref()
            .ok_or(queries.errors.protocol)?
            .reads;
        let shared_reads =
            reads == **membership_reads && reads.observed().eq(membership_reads.observed());
        let reads_owner = shared_reads.then(|| membership.clone());
        let reads = if shared_reads {
            membership_reads.clone()
        } else {
            Arc::new(reads)
        };
        let data = Arc::new(crate::source_components::SourceGrowthData {
            facets: Arc::new(facets),
            diagnostics: Arc::new(diagnostics),
            reads,
            _reads_owner: reads_owner,
        });
        let required = LiveGrowthProduct::value_bytes(&data).ok_or(queries.errors.resource)?;
        let mut allowance = queries
            .output_pool
            .take(required)
            .ok_or(queries.errors.resource)?;
        let source = ProductSource::prepare(
            (*queries.items).clone(),
            queries.records.clone(),
            queries.front.clone(),
        )
        .ok_or(queries.errors.protocol)?;
        let value = LiveGrowthProduct::new(
            (queries.query.address, action),
            data,
            source,
            &mut allowance,
        )
        .ok_or(queries.errors.resource)?;
        let product = ProductRef::growth(value);
        *queries.completed.lock().unwrap_or_else(|e| e.into_inner()) = Some(product.clone());
        Ok(product)
    }
}
impl QueryEngine {
    /// Enumerate required source declarations from the index, including
    /// unexecuted declarations and fact helpers, without demanding headers.
    pub fn front_source_roots(&mut self) -> Vec<DefPath> {
        // The source-only index restores every declaration but no legacy
        // growth controller. Its pending set therefore contains only edited
        // declarations, never the complete source Check obligation set.
        let mut roots: BTreeSet<_> = self.source_paths().into_iter().collect();
        // A warm source catalog carries only the written declaration. Its fact
        // helper is still a required body of the source check transaction.
        for path in roots.clone() {
            if self.source_facts(&path).is_some_and(|facts| facts.fact) {
                let helper = hirsel_check_protocol::fact::fact_path(&path);
                roots.insert(helper);
            }
        }
        roots.into_iter().collect()
    }
    /// Capture the complete selected source-root set, including declarations
    /// never executed by the launch entry. A partial front view is refused.
    pub fn front_growth_queries(
        &mut self,
        root: &DefPath,
        front: Arc<FrontBodyQueries>,
        weight: MemoryWeight,
        mut allowance: MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontGrowthQueries>, QueryError> {
        let refusal = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "source growth admission".into(),
                message: "required source roots are missing or exceed admission".into(),
            })
        };
        let definitions = front
            .namespace()
            .roots
            .iter()
            .map(|id| id.path().clone())
            .collect::<Vec<_>>();
        if FrontGrowthQueries::input_bytes(&definitions, weight)
            .is_none_or(|n| n > allowance.bytes())
        {
            return Err(refusal());
        }
        // Keep this backing exact so the forecast and retained-input debit
        // describe the same allocation even for non-power-of-two root counts.
        let mut roots = Vec::with_capacity(definitions.len());
        for path in &definitions {
            roots.push(front.query(path).ok_or_else(|| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "source growth admission".into(),
                    message: format!("missing checked-root producer for {path:?}"),
                })
            })?);
        }
        // The settled owner retains only root IDs and handles to the exact
        // front source. Charge its real backing before assigning the remainder
        // of this same reservation to its completed output.
        let source = front.stage_source();
        let package = root.0.package.clone();
        let mut retained = crate::NativeMetadataWeight::default();
        retained.add(std::mem::size_of::<FrontGrowthQueries>() + 2 * std::mem::size_of::<usize>());
        retained.flat_vec(&roots);
        retained.add(package.name.capacity() + package.version.capacity());
        let output_budget = allowance
            .bytes()
            .checked_sub(retained.bytes())
            .ok_or_else(refusal)?;
        let output = allowance.split(output_budget).ok_or_else(refusal)?;
        let output_pool = MemoryReservationPool::new(output).map_err(|_| refusal())?;
        let query = QueryId {
            kind: FRONT_GROWTH_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.source-growth.address.v1",
                [
                    self.environment_value().0.as_slice(),
                    root.fingerprint().0.as_slice(),
                ],
            )),
        };
        let store = self.result_store.clone().ok_or_else(refusal)?;
        Ok(Arc::new(FrontGrowthQueries {
            publication_source: Mutex::new(None),
            query,
            roots,
            front: front.clone(),
            // The front owner remains held here, including its original
            // source/context reservation. No copied anchor/path tables.
            items: source.anchors,
            records: source.records,
            sources: front.namespace().sources.clone(),
            snapshot: source.snapshot,
            package,
            store,
            completed: Mutex::new(None),
            membership: Mutex::new(None),
            components: Mutex::default(),
            solves: AtomicU64::new(0),
            memberships: AtomicU64::new(0),
            failure: Mutex::new(None),
            weight,
            errors,
            output_pool,
            _allowance: allowance,
        }))
    }
}

pub(super) fn publish_observations(
    records: &crate::NativeRecords,
    source: &crate::NativeRecordContext,
    store: &dyn ResultStore,
    reads: &Reads,
    scratch: u64,
) -> Option<Reference> {
    let bytes = records.encode(source, reads)?;
    (bytes.len() as u64 <= scratch)
        .then(|| store.put_content(OBSERVATIONS, &bytes))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_record_preserves_every_row_and_refuses_short_working_space() {
        let engine = QueryEngine::new(crate::default_environment(), Default::default());
        let source = crate::NativeRecordContext::capture(&engine);
        let store = Arc::new(crate::store::MemStore::default());
        let records = crate::NativeRecords::new(engine.types().clone(), store.clone());
        let reads = Reads::from_vec(
            (0u32..1024)
                .map(|i| {
                    (
                        ReadKey::ExecutedBody(DefId::intern(&DefPath::core(&format!(
                            "observed_{i}"
                        )))),
                        ContentHash::of_bytes(&i.to_le_bytes()),
                    )
                })
                .collect(),
        )
        .unwrap();
        let bytes = records.encode(&source, &reads).unwrap();
        let reference = OBSERVATIONS.reference(&bytes).unwrap();
        assert!(
            publish_observations(&records, &source, &*store, &reads, bytes.len() as u64 - 1)
                .is_none()
        );
        assert!(store.get_content(reference).is_none());
        assert_eq!(
            publish_observations(&records, &source, &*store, &reads, bytes.len() as u64),
            Some(reference)
        );
        let restored: Reads = records
            .decode(&source, &store.get_content(reference).unwrap())
            .unwrap();
        assert_eq!(restored, reads);
        assert_eq!(restored.len(), 1024);
    }
}

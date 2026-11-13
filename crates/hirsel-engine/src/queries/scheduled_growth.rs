//! Source GEN-5 over every required checked root. Native reachability never
//! selects this root set. Pending reads retain the frontier and compact facts;
//! factories and output inspection execute no compiler phase.
use super::*;
use crate::products::ProductSource;
use crate::products::{
    GrowthPublicationSource, LIVE_SOURCE_COMPONENT, LIVE_SOURCE_MEMBERSHIP, LiveGrowthProduct,
    LiveSourceGrowthProduct, SourceGrowthQueryData,
};
use crate::source_components::{SourceGrowthComponents, SourceGrowthQueryRecord, SourceMembership};
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering::Relaxed},
};
pub const FRONT_GROWTH_KIND: u32 = 0x4600_0008;
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
    output_pool: Arc<hirsel_engine_admission::ProcessBudget>,
    _allowance: hirsel_engine_admission::MemoryReservation,
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
        let data = product
            .live_value::<crate::products::LiveGrowthProduct>()?
            .value();
        if *selected != product {
            return None;
        }
        Some(FrontGrowthOutput {
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
        if id.kind != FRONT_GROWTH_KIND {
            return Err(queries.errors.protocol);
        }
        let membership_query = queries.membership_query();
        let component = if id == queries.query || id == membership_query {
            None
        } else {
            let selected = queries.membership.lock().unwrap_or_else(|e| e.into_inner());
            let value = selected
                .as_ref()
                .and_then(ProductRef::live_value::<LiveSourceGrowthProduct>)
                .and_then(LiveSourceGrowthProduct::membership)
                .ok_or(queries.errors.protocol)?;
            let index = value
                .component_index(id.address.0)
                .ok_or(queries.errors.protocol)?;
            Some((value, index))
        };
        let cached = if id == queries.query {
            queries
                .completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else if id == membership_query {
            queries
                .membership
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else {
            let (_, index) = component.as_ref().ok_or(queries.errors.protocol)?;
            let value = queries
                .components
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(*index)
                .cloned()
                .or_else(|| {
                    queries
                        .completed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()?
                        .live_value::<crate::products::LiveGrowthProduct>()?
                        .value()
                        .facets
                        .components
                        .get(*index)
                        .cloned()
                });
            value
                .and_then(LiveSourceGrowthProduct::reopen_component)
                .map(|value| {
                    value
                        .publish(queries.store.as_ref())
                        .map_err(|_| queries.errors.resource)
                })
                .transpose()?
        };
        if let Some(product) = cached {
            return Ok(Box::new(Reopened { product }));
        }
        let front = queries.front.clone();
        if id == queries.query {
            return Ok(Box::new(Task {
                front,
                queries,
                membership: None,
                component_cursor: 0,
                cursor: 0,
                reads: Vec::new(),
                terminal: None,
            }));
        }
        if queries.weight.retained < (size_of::<FactTask>() + 2 * size_of::<usize>()) as u64 {
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
            .reserve(bytes)
            .ok()
            .ok_or(queries.errors.resource)?;
        Ok(Box::new(FactTask {
            front,
            queries,
            id,
            component,
            count,
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
    fn run(&mut self, _: &mut dyn QueryContext) -> QueryOutcome {
        Ok(self.product.clone())
    }
}
struct FactTask {
    front: Arc<FrontBodyQueries>,
    queries: Arc<FrontGrowthQueries>,
    id: QueryId,
    component: Option<(Arc<SourceMembership>, usize)>,
    count: usize,
    cursor: usize,
    facts: Vec<SourceGrowthFact>,
    reads: Vec<Arc<Reads>>,
    terminal: Option<Result<ProductRef, Reference>>,
    admission: hirsel_engine_admission::MemoryReservation,
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
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        while self.cursor < self.count {
            let input = self
                .queries
                .selected_input(&self.front, self.input(self.cursor))?;
            let selected = cx.read(input);
            if let Some(checked) = self.front.checked(selected.clone()) {
                self.reads.push(checked.reads.clone());
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
                    return Err(self.queries.errors.resource);
                };
                let bytes = fact.input_bytes() - std::mem::size_of::<SourceGrowthFact>() as u64;
                let Some(charge) = self.queries.output_pool.reserve(bytes).ok() else {
                    return Err(self.queries.errors.resource);
                };
                if self.admission.join(charge).is_err() {
                    return Err(self.queries.errors.protocol);
                }
                self.facts.push(fact);
            } else if let Some(facts) = self.front.check_facts(selected.clone())
                && matches!(facts.result, Err(QueryError::Diagnostics(_)))
            {
                self.reads.push(facts.reads.clone());
            } else {
                return Err(selected.err().unwrap_or(self.queries.errors.protocol));
            }
            cx.consume(input);
            self.cursor += 1;
        }
        self.complete(cx)
    }
    fn release_inputs(&mut self) -> Result<(), Reference> {
        drop(std::mem::take(&mut self.facts));
        drop(std::mem::take(&mut self.reads));
        let old = std::mem::replace(
            &mut self.admission,
            self.queries
                .output_pool
                .reserve(0)
                .ok()
                .ok_or(self.queries.errors.resource)?,
        );
        drop(old);
        Ok(())
    }
    fn complete(&mut self, _cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let queries = self.queries.clone();
        self.facts
            .sort_by_key(|fact| (fact.owner, fact.definition.0.0));
        let reads = Arc::new(
            union_of(self.reads.iter().map(|reads| &**reads))
                .finish()
                .map_err(|_| queries.errors.protocol)?,
        );
        let producer = queries.producer();
        let (identity, data) = if let Some((membership, index)) = &self.component {
            let members = &membership.components[*index].members;
            let action = producer
                .component_action(members, self.facts.iter())
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
            self.release_inputs()?;
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
                "hirsel.source-growth.membership.query-action.v2",
                [inputs.0.0.as_slice()],
            ));
            let cached = queries
                .store
                .lookup_product(self.id.address)
                .filter(|a| a.action == action && a.reference.domain == LIVE_SOURCE_MEMBERSHIP)
                .and_then(|a| queries.load_record(a.reference));
            let (product, components) = if let Some(SourceGrowthQueryRecord::Membership {
                product,
                components,
                ..
            }) = cached
            {
                if product.address != self.id.address || product.action != action {
                    return Err(queries.errors.protocol);
                }
                (product, components)
            } else {
                queries.memberships.fetch_add(1, Relaxed);
                let (mut product, components) = producer
                    .membership_parts(&self.facts)
                    .ok_or(queries.errors.resource)?;
                product.action = action;
                (product, components)
            };
            // The active scratch frame owns construction until publication.
            // Retire the input vectors before admitting the retained membership.
            self.release_inputs()?;
            let value = producer
                .membership_value(product, components)
                .ok_or(queries.errors.resource)?;
            (
                value.product,
                SourceGrowthQueryData::Membership {
                    value: Arc::new(value),
                    reads,
                },
            )
        };
        let value = LiveSourceGrowthProduct::new(
            (identity.address, identity.action),
            data,
            queries.publication_source()?,
        )
        .ok_or(queries.errors.resource)?;
        let product = value
            .publish(queries.store.as_ref())
            .map_err(|_| queries.errors.resource)?;
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
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = &self.terminal {
            return result.clone();
        }
        let result = self.advance(cx);
        self.terminal = Some(result.clone());
        result
    }
}
impl FrontGrowthQueries {
    fn selected_input(
        &self,
        front: &FrontBodyQueries,
        original: QueryId,
    ) -> Result<QueryId, Reference> {
        self.front
            .definition(original)
            .and_then(|definition| front.query(definition.path()))
            .ok_or(self.errors.protocol)
    }
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
            .reserve(LiveSourceGrowthProduct::source_bytes())
            .ok()
            .ok_or(self.errors.resource)?;
        let admitted = LiveSourceGrowthProduct::admit_source(
            input,
            self.store.clone(),
            self.weight.scratch,
            &mut allowance,
        )
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
        if !self.store.persistence().publishes() {
            return;
        }
        let Some(value) = product.live_value::<crate::products::LiveSourceGrowthProduct>() else {
            return;
        };
        let Some(value) = value.value() else {
            return;
        };
        let Some(bytes) = self.records.encode(&*self.items, &value) else {
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
    front: Arc<FrontBodyQueries>,
    queries: Arc<FrontGrowthQueries>,
    membership: Option<Arc<SourceMembership>>,
    component_cursor: usize,
    cursor: usize,
    reads: Vec<Arc<Reads>>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.queries.query
    }
    fn weight(&self) -> MemoryWeight {
        self.queries.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = &self.terminal {
            return result.clone();
        }
        let result = self.advance(cx);
        self.terminal = Some(result.clone());
        result
    }
}
impl Task {
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let queries = self.queries.clone();
        if self.membership.is_none() {
            let id = queries.membership_query();
            let product = cx.read(id)?;
            let Some(publication) =
                product.live_value::<crate::products::LiveSourceGrowthProduct>()
            else {
                return Err(queries.errors.protocol);
            };
            let Some(value) = publication.membership() else {
                return Err(queries.errors.protocol);
            };
            self.reads.push(publication.reader());
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
            let product = cx.read(id)?;
            let Some(publication) =
                product.live_value::<crate::products::LiveSourceGrowthProduct>()
            else {
                return Err(queries.errors.protocol);
            };
            let Some(value) = publication.component() else {
                return Err(queries.errors.protocol);
            };
            self.reads.push(publication.reader());
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
                    return Err(queries.errors.resource);
                }
            } else if components[self.component_cursor].product != value.product {
                return Err(queries.errors.protocol);
            }
            // A restarted reduction reads the retained prefix through the
            // scheduler again, so this generation records every dependency.
            drop(components);
            cx.consume(id);
            self.component_cursor += 1;
        }
        while self.cursor < queries.roots.len() {
            let id = queries.selected_input(&self.front, queries.roots[self.cursor])?;
            let selected = cx.read(id);
            if let Some(checked) = self.front.checked(selected.clone()) {
                self.reads.push(checked.reads.clone());
            } else if let Some(facts) = self.front.check_facts(selected.clone())
                && matches!(facts.result, Err(QueryError::Diagnostics(_)))
            {
                self.reads.push(facts.reads.clone());
            } else {
                return Err(selected.err().unwrap_or(queries.errors.protocol));
            }
            cx.consume(id);
            self.cursor += 1;
        }
        self.complete(cx)
    }
    fn complete(&mut self, _cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let queries = &self.queries;
        let reads = union_of(self.reads.iter().map(|reads| &**reads))
            .finish()
            .map_err(|_| queries.errors.protocol)?;
        self.reads.clear();
        self.reads.shrink_to_fit();
        let membership = self.membership.as_ref().ok_or(queries.errors.protocol)?;
        let facets = queries
            .producer()
            .accept(
                membership.product,
                std::mem::take(&mut *queries.components.lock().unwrap_or_else(|e| e.into_inner())),
            )
            .ok_or(queries.errors.resource)?;
        let diagnostics = crate::select_diagnostics(
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
                })
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
            "hirsel.front.source-growth.value.v2",
            &(facets.membership.output, &outputs, &diagnostics),
        )
        .ok_or(queries.errors.resource)?;
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.source-growth.action.v3",
            [queries.query.address.0.0.as_slice(), observed.0.as_slice()],
        ));
        let membership_reader = &membership
            .publication
            .as_ref()
            .ok_or(queries.errors.protocol)?
            .reads;
        let reads = if reads == **membership_reader {
            membership_reader.clone()
        } else {
            Arc::new(reads)
        };
        let data = Arc::new(crate::source_components::SourceGrowthData {
            facets: Arc::new(facets),
            diagnostics: Arc::new(diagnostics),
            reads,
        });
        let required = LiveGrowthProduct::value_bytes(&data).ok_or(queries.errors.resource)?;
        let mut allowance = queries
            .output_pool
            .reserve(required)
            .ok()
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
        let product = value
            .publish(queries.store.as_ref())
            .map_err(|_| queries.errors.resource)?;
        *queries.completed.lock().unwrap_or_else(|e| e.into_inner()) = Some(product.clone());
        Ok(product)
    }
}
impl QueryEngine {
    /// Enumerate required source declarations from the index, including
    /// unexecuted declarations and fact helpers, without demanding headers.
    pub fn front_source_roots(&mut self) -> Vec<DefPath> {
        self.source_root_ids()
            .into_iter()
            .map(|id| id.path().clone())
            .collect()
    }
    pub(super) fn source_root_ids(&self) -> BTreeSet<DefId> {
        let mut roots: BTreeSet<_> = self
            .source_paths_iter()
            .filter(|path| !self.installed_definition(path))
            .map(DefId::intern)
            .collect();
        let helpers: Vec<_> = roots
            .iter()
            .filter(|id| self.source_facts(id.path()).is_some_and(|facts| facts.fact))
            .map(|id| DefId::intern(&hirsel_check_protocol::fact::fact_path(id.path())))
            .collect();
        roots.extend(helpers);
        roots
    }
    /// Capture the complete selected source-root set, including declarations
    /// never executed by the launch entry. A partial front view is refused.
    pub fn front_growth_queries(
        &mut self,
        root: &DefPath,
        front: Arc<FrontBodyQueries>,
        weight: MemoryWeight,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
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
        // front source. Charge its real backing; outputs charge the request.
        let source = front.stage_source();
        let package = root.0.package.clone();
        let mut retained = hirsel_query_products::retained::MetadataWeight::default();
        retained.add(std::mem::size_of::<FrontGrowthQueries>() + 2 * std::mem::size_of::<usize>());
        retained.flat_vec(&roots);
        retained.add(package.name.capacity() + package.version.capacity());
        let allowance = request.reserve(retained.bytes()).map_err(|_| refusal())?;
        let output_pool = request.clone();
        let query = QueryId {
            kind: FRONT_GROWTH_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.source-growth.address.v1",
                [
                    front.namespace().lease().identity().0.as_slice(),
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

fn union_of<'a>(readers: impl IntoIterator<Item = &'a Reads>) -> ReadsBuilder {
    let mut union = ReadsBuilder::new();
    for reads in readers {
        union.merge(reads);
    }
    union
}

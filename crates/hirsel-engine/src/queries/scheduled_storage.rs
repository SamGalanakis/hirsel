//! Source storage closure over actual settled checked and recipe products.
//! Port of storage_graph::derive: backward callers select the frontier, then
//! actual callee/constant references extend it. The solver is shared with the
//! scalar adapter and is never used as a substitute for V's independent proof.
use super::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};
use hirsel_types::storage::{CallableStorageProfile, graph::*};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering::Relaxed},
};
pub const FRONT_STORAGE_GRAPH_KIND: u32 = 0x4600_0014;
const OUTPUT: Domain = Domain {
    kind: 0x4600_0015,
    schema: 1,
};
const FAILURE: Domain = Domain {
    kind: 0x4600_0016,
    schema: 1,
};
struct Data {
    profiles: Arc<Vec<CallableStorageProfile>>,
    diagnostics: Arc<Vec<Diagnostic>>,
    reads: Arc<Reads>,
    _reservation: Arc<hirsel_engine_admission::MemoryReservation>,
    _source: Arc<FrontBodyQueries>,
}
pub struct FrontStorageGraphOutput {
    pub profiles: RetainedView<Vec<CallableStorageProfile>>,
    pub diagnostics: RetainedView<Vec<Diagnostic>>,
    pub reads: Arc<Reads>,
}
pub struct FrontStorageGraphQueries {
    query: QueryId,
    definitions: Vec<DefId>,
    instances: Box<[DefId]>,
    front: Arc<FrontBodyQueries>,
    growth: Arc<FrontGrowthQueries>,
    recipes: Arc<FrontStorageQueries>,
    lower: Arc<FrontLowerQueries>,
    records: crate::NativeRecords,
    anchors: Arc<crate::NativeRecordContext>,
    store: Arc<dyn ResultStore>,
    completed: Mutex<Option<(ProductRef, Arc<Data>)>>,
    failure: Mutex<
        Option<(
            Reference,
            QueryError,
            hirsel_engine_admission::MemoryReservation,
        )>,
    >,
    executions: AtomicU64,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    outputs: Arc<hirsel_engine_admission::ProcessBudget>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
impl FrontStorageGraphQueries {
    pub(crate) fn matches(
        &self,
        front: &Arc<FrontBodyQueries>,
        lower: &Arc<FrontLowerQueries>,
        growth: &Arc<FrontGrowthQueries>,
    ) -> bool {
        Arc::ptr_eq(&self.front, front)
            && Arc::ptr_eq(&self.lower, lower)
            && Arc::ptr_eq(&self.growth, growth)
    }
    pub fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Self>() + 2 * size_of::<usize>());
        weight.add(
            definitions
                .len()
                .checked_mul(std::mem::size_of::<DefId>())?,
        );
        // Every potential instance can occupy one set entry. Interned source
        // paths and the immutable record context stay with the front owner.
        weight.tree::<DefId, ()>(definitions.len());
        Some(weight.bytes())
    }

    pub fn query(&self) -> QueryId {
        self.query
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontStorageGraphOutput> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = completed.as_ref()?;
        (*selected == product).then(|| FrontStorageGraphOutput {
            profiles: RetainedView::new(data.profiles.clone(), data.clone()),
            diagnostics: RetainedView::new(data.diagnostics.clone(), data.clone()),
            reads: data.reads.clone(),
        })
    }
    pub fn failure(&self, reference: Reference) -> Option<QueryError> {
        let own = self
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|(selected, error, _)| (*selected == reference).then(|| error.clone()));
        own.or_else(|| self.recipes.failure(reference))
            .or_else(|| self.lower.failure(reference))
            .or_else(|| self.growth.failure(reference))
            .or_else(|| {
                self.front
                    .outcome(Err(reference))
                    .and_then(|o| o.outcome.result.err())
            })
    }
    pub fn executions(&self) -> u64 {
        self.executions.load(Relaxed)
    }
    fn fail(&self, error: QueryError) -> Reference {
        let Some(bytes) = self.records.encode(&*self.anchors, &error) else {
            return self.errors.resource;
        };
        let mut weight = hirsel_query_products::retained::Weight::default();
        weight.error(&error);
        let Some(reservation) = self.outputs.reserve(weight.bytes).ok() else {
            return self.errors.resource;
        };
        let Some(reference) = self.store.put_content(FAILURE, &bytes) else {
            return self.errors.resource;
        };
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((reference, error, reservation));
        reference
    }
}
struct Factory(Arc<FrontStorageGraphQueries>);
impl QueryFactory for Factory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if id != self.0.query {
            return Err(self.0.errors.protocol);
        }
        let terminal = self
            .0
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(product, _)| Ok(product.clone()));
        Ok(Box::new(Task {
            owner: self.0.clone(),
            initialized: false,
            pending: BTreeSet::new(),
            seen: BTreeSet::new(),
            bodies: BTreeMap::new(),
            current: None,
            reads: ReadsBuilder::new(),
            digest: ContentHash::of_bytes(b"source storage"),
            terminal,
        }))
    }
}
struct Current {
    storage: StorageDefinition,
    body: usize,
    node: usize,
}
struct Task {
    owner: Arc<FrontStorageGraphQueries>,
    initialized: bool,
    pending: BTreeSet<DefPath>,
    seen: BTreeSet<DefPath>,
    bodies: BTreeMap<hirsel_types::storage::StorageCallable, StorageBody>,
    current: Option<Current>,
    reads: ReadsBuilder,
    terminal: Option<Result<ProductRef, Reference>>,
    digest: ContentHash,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.owner.query
    }
    fn weight(&self) -> MemoryWeight {
        self.owner.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        let result = if let Some(result) = self.terminal.clone() {
            result
        } else {
            let result = self.advance(cx);
            self.terminal = Some(result.clone());
            result
        };
        result
    }
}
impl Task {
    fn read(&mut self, cx: &mut dyn QueryContext, query: QueryId) -> Result<ProductRef, Reference> {
        let result = cx.read(query);
        if let Ok(product) = &result {
            self.fold(product);
        }
        result
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let owner = self.owner.clone();
        if !self.initialized {
            match self.read(cx, owner.growth.query()) {
                Err(error) => return Err(error),
                Ok(_) => (),
            };
            let mut checked = Vec::new();
            for definition in &owner.definitions {
                let path = definition.path();
                let Some(output) = owner.front.checked_definition(path) else {
                    return Err(owner.fail(invalid(format!(
                        "missing checked source storage root {path}"
                    ))));
                };
                self.reads.merge(&output.reads);
                self.digest = ContentHash::of_parts(
                    "hirsel.front.storage-source.v1",
                    [
                        self.digest.0.as_slice(),
                        path.fingerprint().0.as_slice(),
                        owner.front.namespace().identity().0.as_slice(),
                    ],
                );
                if output
                    .summary
                    .summary
                    .calls
                    .iter()
                    .any(|call| call.callee.path().is_core("region.with"))
                {
                    self.pending.insert(path.clone());
                }
                checked.push((path.clone(), output.summary));
            }
            // Actual callers, including unexecuted source roots, preserve the
            // original provenance of callback and owner arguments.
            loop {
                let mut changed = false;
                for (path, summary) in &checked {
                    if summary
                        .summary
                        .calls
                        .iter()
                        .any(|call| self.pending.contains(call.callee.path()))
                    {
                        changed |= self.pending.insert(path.clone());
                    }
                }
                if !changed {
                    break;
                }
            }
            self.initialized = true;
        }
        loop {
            if self.current.is_none() {
                let Some(path) = self.pending.pop_first() else {
                    return self.complete().map_err(|error| owner.fail(error));
                };
                if !self.seen.insert(path.clone()) {
                    continue;
                }
                // Reviewed core recipes are independent. An instance body is
                // selected through the immutable instance table at each use.
                if path.0.package.name == "core" || owner.instances.contains(&DefId::intern(&path))
                {
                    continue;
                }
                let Some(query) = owner.recipes.query(&path) else {
                    return Err(owner.fail(invalid(format!(
                        "missing actual storage producer for {path}"
                    ))));
                };
                let mut recipe = owner.recipes.factory().create(query)?;
                if recipe.id() != query {
                    return Err(owner.errors.protocol);
                }
                // The root runs each recipe under its own frame; the recipe's
                // reads are still actual runtime reads.
                let product = recipe.run(cx)?;
                self.fold(&product);
                let Some(output) = owner.recipes.outcome(product) else {
                    return Err(owner.errors.protocol);
                };
                self.reads.merge(&output.reads);
                self.current = Some(Current {
                    storage: output.output.value.clone(),
                    body: 0,
                    node: 0,
                });
            }
            let mut current = self.current.take().unwrap();
            while current.body < current.storage.bodies.len() {
                let body = &mut current.storage.bodies[current.body];
                while current.node < body.nodes.len() {
                    let node = &mut body.nodes[current.node];
                    if let StorageOp::Assign(assignments) = &mut node.operation {
                        for (_, value) in assignments {
                            if let StorageValue::ConstantReference {
                                definition,
                                arguments,
                            } = value
                            {
                                if !arguments.is_empty() {
                                    *value = StorageValue::Unknown;
                                    continue;
                                }
                                // Reviewed builtin dictionaries have no source Evaluate
                                // producer. settled_constant materializes their actual
                                // fields and still refuses unavailable source constants.
                                if let Some(query) = owner.lower.evaluate_query(definition.path()) {
                                    match self.read(cx, query) {
                                        Err(error) => return Err(error),
                                        Ok(_) => (),
                                    }
                                }
                                let materialized =
                                    match owner.lower.settled_constant(definition.path(), &[]) {
                                        Ok(value) => value,
                                        Err(error) => return Err(owner.fail(error)),
                                    };
                                if let Some(reads) = owner
                                    .lower
                                    .settled_reads(definition.path(), QueryKind::Evaluate)
                                {
                                    self.reads.merge(&reads);
                                }
                                let snapshot = owner.lower.signatures();
                                let mut provider = crate::FrozenProvider::new(
                                    &snapshot,
                                    definition.path().0.package.clone(),
                                    &mut self.reads,
                                );
                                if let Err(error) = hirsel_types::SignatureProvider::signature(
                                    &mut provider,
                                    *definition,
                                ) {
                                    return Err(owner.fail(error));
                                }
                                *value = match crate::storage_graph::constant_value(
                                    snapshot.types(),
                                    &materialized,
                                    &mut |id| {
                                        hirsel_types::SignatureProvider::signature(
                                            &mut provider,
                                            id,
                                        )
                                    },
                                ) {
                                    Ok(value) => value,
                                    Err(error) => return Err(owner.fail(error)),
                                };
                            }
                        }
                    }
                    match &node.operation {
                        StorageOp::Call {
                            callee:
                                StorageCallee::Apply {
                                    definition,
                                    arguments,
                                },
                            ..
                        } => {
                            if let Err(error) = self.reference(*definition, arguments) {
                                return Err(owner.fail(error));
                            }
                        }
                        StorageOp::Call {
                            callee: StorageCallee::Definition(definition),
                            ..
                        } => {
                            self.pending.insert(definition.path().clone());
                        }
                        StorageOp::Assign(assignments) => {
                            for (_, value) in assignments {
                                match value {
                                    StorageValue::Reference {
                                        definition,
                                        arguments,
                                    } => {
                                        if let Err(error) = self.reference(*definition, arguments) {
                                            return Err(owner.fail(error));
                                        }
                                    }
                                    StorageValue::Constant { code, .. } => {
                                        for reference in code {
                                            if let Err(error) = self.reference(
                                                reference.definition,
                                                &reference.arguments,
                                            ) {
                                                return Err(owner.fail(error));
                                            }
                                        }
                                    }
                                    StorageValue::Callable { target, .. } => {
                                        self.pending.insert(target.definition.path().clone());
                                    }
                                    _ => (),
                                }
                            }
                        }
                        _ => (),
                    }
                    current.node += 1;
                }
                if self.bodies.insert(body.callable, body.clone()).is_some() {
                    return Err(owner.fail(invalid("duplicate actual callable association")));
                }
                current.body += 1;
                current.node = 0;
            }
            let bytes = self.bodies.values().try_fold(0u64, |bytes, body| {
                let mut weight = hirsel_query_products::retained::Weight::default();
                weight.storage_body(body);
                bytes.checked_add(weight.bytes)
            });
            if bytes.is_none_or(|bytes| bytes > owner.weight.retained) {
                return Err(owner.errors.resource);
            }
        }
    }
    fn fold(&mut self, product: &ProductRef) {
        self.digest = ContentHash::of_parts(
            "hirsel.front.storage-input.v1",
            [
                self.digest.0.as_slice(),
                product.address.0.0.as_slice(),
                product.action.0.0.as_slice(),
            ],
        );
    }
    fn reference(
        &mut self,
        definition: DefId,
        arguments: &[hirsel_types::GenericArgument],
    ) -> Result<(), QueryError> {
        self.pending.insert(definition.path().clone());
        if definition.path().0.package.name == "core" || !self.owner.instances.contains(&definition)
        {
            return Ok(());
        }
        let signature = self.owner.lower.signature(definition.path())?;
        let hirsel_types::SignatureKind::Function(function) = &signature.kind else {
            return Ok(());
        };
        if function.generics.len() != arguments.len()
            || !function.generics.iter().zip(arguments).all(|(p, a)| {
                matches!(
                    (p, a),
                    (
                        hirsel_types::GenericParameter::Type { .. },
                        hirsel_types::GenericArgument::Type(_)
                    ) | (
                        hirsel_types::GenericParameter::Size { .. },
                        hirsel_types::GenericArgument::Size(_)
                    )
                )
            })
        {
            return Ok(());
        }
        if hirsel_tir::InstanceBodies::key(self.owner.lower.signatures().types(), arguments)
            .is_none()
        {
            return Ok(());
        }
        let selected = self
            .owner
            .lower
            .settled_instance(definition.path(), arguments)?
            .ok_or_else(|| {
                invalid(format!(
                    "missing selected generated storage instance for {}",
                    definition.path()
                ))
            })?;
        self.pending.insert(selected);
        Ok(())
    }
    fn complete(&mut self) -> Result<ProductRef, QueryError> {
        let owner = &self.owner;
        let bodies = std::mem::take(&mut self.bodies)
            .into_values()
            .collect::<Vec<_>>();
        let lower = owner.lower.clone();
        let select = |definition: DefId, arguments: &[hirsel_types::GenericArgument]| {
            lower
                .settled_instance(definition.path(), arguments)
                .ok()
                .map(|selected| selected.map_or(definition, |p| DefId::intern(&p)))
        };
        owner.executions.fetch_add(1, Relaxed);
        let (profiles, diagnostics) =
            crate::storage_graph::finish(owner.lower.signatures().types(), &select, bodies)?;
        let reads = std::mem::take(&mut self.reads)
            .finish()
            .map_err(|_| invalid("storage source reads disagree"))?;
        let profiles = Arc::new(profiles);
        let diagnostics = Arc::new(diagnostics);
        let reads = Arc::new(reads);
        let mut weight = hirsel_query_products::retained::Weight::default();
        weight.add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<Data>::uninit(),
        ) as usize);
        weight.arc(&profiles, |w, profiles| w.storage_profiles(profiles));
        weight.arc(&diagnostics, |w, diagnostics| {
            w.vec(diagnostics, |w, diagnostic| w.diagnostic(diagnostic))
        });
        weight.reads(&reads);
        type Record = (
            Arc<Vec<CallableStorageProfile>>,
            Arc<Vec<Diagnostic>>,
            Arc<Reads>,
        );
        weight.add(
            hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                super::live_record::LiveRecord<Record>,
            >::uninit()) as usize,
        );
        weight.add(
            hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                hirsel_engine_admission::MemoryReservation,
            >::uninit()) as usize,
        );
        let reservation = Arc::new(
            owner
                .outputs
                .reserve(weight.bytes)
                .ok()
                .ok_or_else(|| invalid("storage graph output exceeds admission"))?,
        );
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.storage-graph.action.v3",
            [self.digest.0.as_slice()],
        ));
        let product = ProductRef::live(
            owner.query.address,
            action,
            Arc::new(super::live_record::LiveRecord {
                value: (profiles.clone(), diagnostics.clone(), reads.clone()),
                domain: OUTPUT,
                records: owner.records.clone(),
                source: (*owner.anchors).clone(),
                serialization_working: reads
                    .encoding_working_bytes()
                    .ok_or_else(|| invalid("storage read encoding size overflow"))?,
                _admission: reservation.clone(),
            }),
        );
        *owner.completed.lock().unwrap_or_else(|e| e.into_inner()) = Some((
            product.clone(),
            Arc::new(Data {
                profiles,
                diagnostics,
                reads,
                _reservation: reservation,
                _source: owner.front.clone(),
            }),
        ));
        Ok(product)
    }
}
fn invalid(message: impl Into<String>) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "admitted source storage proof".into(),
        message: message.into(),
    })
}
impl QueryEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn front_storage_graph_queries(
        &mut self,
        root: &DefPath,
        front: Arc<FrontBodyQueries>,
        growth: Arc<FrontGrowthQueries>,
        recipes: Arc<FrontStorageQueries>,
        lower: Arc<FrontLowerQueries>,
        weight: MemoryWeight,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontStorageGraphQueries>, QueryError> {
        if !recipes.fits_poll(weight) {
            return Err(invalid(
                "storage recipe exceeds its parent execution admission",
            ));
        }
        front.namespace().require_consistent(lower.namespace())?;
        let definitions = front
            .namespace()
            .roots
            .iter()
            .map(|id| id.path().clone())
            .collect::<Vec<_>>();
        let inventory = FrontStorageGraphQueries::input_bytes(&definitions)
            .ok_or(QueryError::NotImplemented)?;
        let allowance = request
            .reserve(inventory)
            .map_err(|_| invalid("source storage inventory exceeds the request footprint"))?;
        let outputs = request.clone();
        let instances = definitions
            .iter()
            .filter(|p| {
                front
                    .namespace()
                    .instances
                    .declarations
                    .contains(&DefId::intern(p))
            })
            .map(DefId::intern)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let store = self
            .result_store
            .clone()
            .ok_or(QueryError::NotImplemented)?;
        let query = QueryId {
            kind: FRONT_STORAGE_GRAPH_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.storage-graph.address.v1",
                [
                    self.environment_value().0.as_slice(),
                    root.fingerprint().0.as_slice(),
                ],
            )),
        };
        let source = front.stage_source();
        Ok(Arc::new(FrontStorageGraphQueries {
            query,
            definitions: definitions.iter().map(DefId::intern).collect(),
            instances,
            front,
            growth,
            recipes,
            lower,
            records: source.records,
            anchors: source.anchors,
            store,
            completed: Mutex::new(None),
            failure: Mutex::new(None),
            executions: AtomicU64::new(0),
            weight,
            errors,
            outputs,
            _allowance: allowance,
        }))
    }
}

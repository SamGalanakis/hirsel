//! Demand-driven source storage equations from immutable checked P products.
//! The worklist consumer requests these bodies; signature lookup never invokes
//! storage derivation or a synchronous compiler/controller fallback.
use super::*;
use hirsel_types::storage::graph::StorageDefinition;
use std::task::Poll;
pub const FRONT_STORAGE_KIND: u32 = 0x4600_000d;
const OUTPUT: Domain = Domain {
    kind: 0x4600_000e,
    schema: 1,
};
const FAILURE: Domain = Domain {
    kind: 0x4600_000f,
    schema: 1,
};
struct Outcome {
    result: Result<(ProductRef, Arc<QueryOutput<StorageDefinition>>), (Reference, QueryError)>,
    reads: Arc<Reads>,
    _reservation: Arc<hirsel_engine_admission::MemoryReservation>,
    _source: Arc<FrontBodyQueries>,
}
#[derive(Default)]
struct Outcomes {
    values: BTreeMap<Key, Arc<Outcome>>,
    directory: Option<hirsel_engine_admission::MemoryReservation>,
}
impl Outcomes {
    fn get(&self, key: &Key) -> Option<&Arc<Outcome>> {
        self.values.get(key)
    }
    fn values(&self) -> impl Iterator<Item = &Arc<Outcome>> {
        self.values.values()
    }
    fn insert(
        &mut self,
        key: Key,
        outcome: Arc<Outcome>,
        outputs: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> Option<()> {
        let len = self
            .values
            .len()
            .checked_add(usize::from(!self.values.contains_key(&key)))?;
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.tree::<Key, Arc<Outcome>>(len);
        let current = self.directory.as_ref().map_or(0, |grant| grant.bytes());
        let extra = outputs.reserve(weight.bytes().checked_sub(current)?).ok()?;
        if let Some(directory) = &mut self.directory {
            directory.join(extra).ok()?;
        } else {
            self.directory = Some(extra);
        }
        self.values.insert(key, outcome);
        Some(())
    }
}
pub struct FrontStorageQueries {
    front: Arc<FrontBodyQueries>,
    completed: Mutex<Outcomes>,
    anchors: Arc<crate::NativeRecordContext>,
    records: crate::NativeRecords,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    outputs: Arc<hirsel_engine_admission::ProcessBudget>,
    executions: std::sync::atomic::AtomicU64,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
#[derive(Clone)]
pub struct FrontStorageOutput {
    pub output: RetainedView<QueryOutput<StorageDefinition>>,
    pub reads: RetainedView<Reads>,
}
impl FrontStorageQueries {
    /// The immutable front already owns the address/definition index. Only
    /// demanded recipes need a task or a completed output in this owner.
    pub fn input_bytes(_front: &FrontBodyQueries) -> Option<u64> {
        Self::input_bytes_for_definitions(std::iter::empty())
    }
    /// The same shared-index owner before front construction.
    pub fn input_bytes_for_definitions<'a>(
        _definitions: impl IntoIterator<Item = &'a DefPath>,
    ) -> Option<u64> {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<Self>() + 2 * size_of::<usize>());
        Some(weight.bytes())
    }
    fn address(input: Key) -> Key {
        ContentHash::of_parts("hirsel.front.storage.address.v1", [input.0.as_slice()])
    }
    pub fn query(&self, path: &DefPath) -> Option<QueryId> {
        let input = self.front.query(path)?;
        Some(QueryId {
            kind: FRONT_STORAGE_KIND,
            address: hirsel_artifact_store::Address(Self::address(input.address.0)),
        })
    }
    fn definition(&self, id: QueryId) -> Option<DefId> {
        if id.kind != FRONT_STORAGE_KIND {
            return None;
        }
        self.front.inputs.iter().find_map(|(address, input)| {
            (Self::address(address) == id.address.0).then_some(input.definition)
        })
    }
    // A composed recipe uses its parent's execution grant. Its retained
    // products and the bounded task frame remain charged to this owner.
    pub(crate) fn fits_poll(&self, parent: MemoryWeight) -> bool {
        self.weight.scratch <= parent.scratch && self.weight.stack <= parent.stack
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(StorageFactory(self.clone()))
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontStorageOutput> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let selected = completed.get(&product.address.0)?;
        let output = match &selected.result {
            Ok((selected, output)) if selected == &product => output,
            Ok((_, _)) | Err((_, _)) => return None,
        };
        Some(FrontStorageOutput {
            output: RetainedView::new(output.clone(), selected.clone()),
            reads: RetainedView::new(selected.reads.clone(), selected.clone()),
        })
    }
    pub fn failure(&self, reference: Reference) -> Option<QueryError> {
        self.completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find_map(|outcome| match &outcome.result {
                Ok((_, _)) => None,
                Err((record, error)) if *record == reference => Some(error.clone()),
                Err((_, _)) => None,
            })
    }
    pub fn executions(&self) -> u64 {
        self.executions.load(std::sync::atomic::Ordering::Relaxed)
    }
}
struct StorageFactory(Arc<FrontStorageQueries>);
impl QueryFactory for StorageFactory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        let definition = self.0.definition(id).ok_or(self.0.errors.protocol)?;
        let terminal = self
            .0
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)
            .map(|outcome| match &outcome.result {
                Ok((product, _)) => Ok(product.clone()),
                Err((reference, _)) => Err(*reference),
            });
        Ok(Box::new(StorageTask {
            owner: self.0.clone(),
            id,
            definition,
            body: None,
            terminal,
        }))
    }
}
struct StorageTask {
    owner: Arc<FrontStorageQueries>,
    id: QueryId,
    definition: DefId,
    body: Option<Box<dyn QueryTask>>,
    terminal: Option<Result<ProductRef, Reference>>,
}
const _: () = assert!(
    std::mem::size_of::<StorageTask>()
        + std::mem::size_of::<BodyTask>()
        + 4 * std::mem::size_of::<usize>()
        <= 512
);
impl QueryTask for StorageTask {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        self.owner.weight
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(terminal) = self.terminal.clone() {
            return finish(terminal);
        }
        let path = self.definition.path();
        let Some(front) = self.owner.front.query(path) else {
            return finish(Err(self.owner.errors.protocol));
        };
        let selected = match cx.read(front) {
            Poll::Pending => return QueryPoll::Waiting(vec![front]),
            Poll::Ready(Err(reference)) => Err(reference),
            Poll::Ready(Ok(product)) => {
                if self.owner.front.outcome(Ok(product.clone())).is_some() {
                    self.run(product)
                } else {
                    let Some(body) = self.owner.front.body_query(path) else {
                        return finish(Err(self.owner.errors.protocol));
                    };
                    if self.body.is_none() {
                        let task = match self.owner.front.factory().create(body) {
                            Ok(task) => task,
                            Err(error) => return finish(Err(error)),
                        };
                        let weight = task.weight();
                        if weight.scratch > self.owner.weight.scratch
                            || weight.stack > self.owner.weight.stack
                        {
                            drop(task);
                            return match cx.read(body) {
                                Poll::Pending => QueryPoll::Waiting(vec![body]),
                                Poll::Ready(Err(error)) => finish(Err(error)),
                                Poll::Ready(Ok(product)) => {
                                    let selected = self.run(product);
                                    self.terminal = Some(selected.clone());
                                    finish(selected)
                                }
                            };
                        }
                        self.body = Some(task);
                    }
                    // Storage owns this one continuation under its existing
                    // frame and working grant, just as the storage graph owns
                    // one recipe. Its actual P read remains the dependency.
                    match self.body.as_mut().unwrap().poll(cx) {
                        waiting @ (QueryPoll::Waiting(_) | QueryPoll::WaitingAny(_)) => {
                            return waiting;
                        }
                        QueryPoll::Failed(error) => Err(error),
                        QueryPoll::Complete(product) => self.run(product),
                    }
                }
            }
        };
        self.body = None;
        self.terminal = Some(selected.clone());
        finish(selected)
    }
}
impl StorageTask {
    fn run(&self, product: ProductRef) -> Result<ProductRef, Reference> {
        let owner = &self.owner;
        let path = self.definition.path();
        let body = owner
            .front
            .outcome(Ok(product.clone()))
            .ok_or(owner.errors.protocol)?;
        let analyzed = body
            .outcome
            .result
            .as_ref()
            .map_err(|_| owner.errors.protocol)?;
        let mut reads = ReadsBuilder::new();
        reads.merge(&body.outcome.reads);
        let mut provider = FrozenProvider::for_definition(
            &owner.front.namespace.snapshot,
            DefId::intern(path),
            &owner.front.citations,
            &mut reads,
        );
        let result = (|| {
            let signature = provider.signature(DefId::intern(path))?;
            let modes = owner
                .front
                .modes
                .as_ref()
                .ok_or(QueryError::NotImplemented)?;
            owner
                .executions
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let output = modes.storage(
                &owner.front.types,
                &owner.front.memo,
                &analyzed.value,
                &signature,
                &mut provider,
            )?;
            if has_errors(&output.diagnostics) {
                return Err(QueryError::Diagnostics(output.diagnostics));
            }
            Ok(Arc::new(output))
        })();
        if matches!(&result, Err(QueryError::Operational(_))) {
            return Err(owner.errors.resource);
        }
        if let Ok(output) = &result {
            for dependency in &output.dependencies {
                reads.record(
                    ReadKey::Signature(dependency.definition),
                    dependency.fingerprint,
                );
            }
        }
        let reads = Arc::new(reads.finish().map_err(|_| owner.errors.protocol)?);
        let mut weight = hirsel_query_products::retained::Weight::default();
        weight.add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<Outcome>::uninit(),
        ) as usize);
        weight.reads(&reads);
        match &result {
            Ok(output) => weight.arc(output, |w, output| {
                w.storage_definition(&output.value);
                w.vec(&output.diagnostics, |w, diagnostic| {
                    w.diagnostic(diagnostic)
                });
                w.flat_vec(&output.dependencies);
            }),
            Err(error) => weight.error(error),
        }
        type Record = (
            DefPath,
            Result<Arc<QueryOutput<StorageDefinition>>, QueryError>,
            Arc<Reads>,
        );
        weight.add(
            hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                super::super::super::live_record::LiveRecord<Record>,
            >::uninit()) as usize,
        );
        weight.def_path(path);
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
                .ok_or(owner.errors.resource)?,
        );
        let result = match result {
            Err(error) => {
                let bytes = owner
                    .records
                    .encode(&*owner.anchors, &(path, Err::<(), _>(&error), &*reads))
                    .ok_or(owner.errors.resource)?;
                let record = owner
                    .front
                    .store
                    .put_content(FAILURE, &bytes)
                    .ok_or(owner.errors.resource)?;
                Err((record, error))
            }
            Ok(output) => {
                let action = hirsel_artifact_store::Action(ContentHash::of_parts(
                    "hirsel.front.storage.action.v3",
                    [product.action.0.0.as_slice()],
                ));
                let product = ProductRef::live(
                    self.id.address,
                    action,
                    Arc::new(super::super::super::live_record::LiveRecord {
                        value: (
                            path.clone(),
                            Ok::<_, QueryError>(output.clone()),
                            reads.clone(),
                        ),
                        domain: OUTPUT,
                        records: owner.records.clone(),
                        source: (*owner.anchors).clone(),
                        serialization_working: reads
                            .encoding_working_bytes()
                            .ok_or(owner.errors.resource)?,
                        _admission: reservation.clone(),
                    }),
                );
                Ok((product, output))
            }
        };
        let terminal = match &result {
            Ok((product, _)) => Ok(product.clone()),
            Err((reference, _)) => Err(*reference),
        };
        owner
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                self.id.address.0,
                Arc::new(Outcome {
                    result,
                    reads,
                    _reservation: reservation,
                    _source: owner.front.clone(),
                }),
                &owner.outputs,
            )
            .ok_or(owner.errors.resource)?;
        terminal
    }
}
impl QueryEngine {
    pub fn front_storage_queries(
        &mut self,
        front: Arc<FrontBodyQueries>,
        weight: MemoryWeight,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontStorageQueries>, QueryError> {
        let inventory =
            FrontStorageQueries::input_bytes(&front).ok_or(QueryError::NotImplemented)?;
        let allowance = request
            .reserve(inventory)
            .map_err(|_| QueryError::NotImplemented)?;
        let outputs = request.clone();
        let source = front.stage_source();
        Ok(Arc::new(FrontStorageQueries {
            front,
            completed: Mutex::new(Outcomes::default()),
            anchors: source.anchors,
            records: source.records,
            weight,
            errors,
            outputs,
            executions: std::sync::atomic::AtomicU64::new(0),
            _allowance: allowance,
        }))
    }
}

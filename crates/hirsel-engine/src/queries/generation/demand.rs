//! One ordered publication owner for instance demand. Execution suspends through
//! the existing query graph; no publication lock crosses a dependency wait.
use super::*;
use hirsel_engine_runtime::MemoryWeight;
use hirsel_engine_runtime::QueryReads;
use hirsel_engine_runtime::RuntimeErrors;

use std::sync::{Mutex, Weak};

pub(crate) struct DemandAdmission {
    pub carry: Option<Arc<FrontCarry>>,
    pub front: hirsel_engine_admission::MemoryReservation,
    pub lower: hirsel_engine_admission::MemoryReservation,
    pub text: hirsel_engine_admission::MemoryReservation,
    pub syntax: hirsel_engine_admission::MemoryReservation,
}

pub(crate) struct DemandProducer {
    carry: Option<Arc<FrontCarry>>,
    adapters: SourcePhases,
    state: Mutex<State>,
    front: Arc<hirsel_engine_admission::MemoryReservationPool>,
    lower: scheduled_lower::LowerPartition,
    text: Arc<hirsel_engine_admission::MemoryReservationPool>,
    syntax: hirsel_engine_admission::MemoryReservationPool,
    weight: MemoryWeight,
    max_reads: usize,
    errors: RuntimeErrors,
    requests: Mutex<BTreeMap<ContentHash, Arc<DemandRequest>>>,
    scopes: Mutex<BTreeMap<ContentHash, Arc<ScopeRequest>>>,
    failure: Mutex<Option<FrontRegistrationBootstrapError>>,
    rounds: std::sync::atomic::AtomicUsize,
    staged: Mutex<BTreeSet<(DefId, ContentHash)>>,
    _metadata: hirsel_engine_admission::MemoryReservation,
}
struct ScopeRequest {
    previous: Arc<NamespaceCapture>,
    roots: Vec<DefPath>,
    policy: LoweringPolicy,
    stop_on_error: bool,
    result: Mutex<Option<Result<scheduled_lower::SuccessorCapture, QueryError>>>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
struct DemandRequest {
    previous: Arc<NamespaceCapture>,
    declaration: DefPath,
    site: Span,
    result: Mutex<Option<Result<scheduled_lower::SuccessorCapture, QueryError>>>,
    failure: Mutex<Option<DemandFailure>>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
#[derive(Clone)]
pub(crate) struct DemandFailure {
    pub(crate) error: QueryError,
    pub(crate) reads: Arc<Reads>,
    pub(crate) sources: Arc<namespace::DiagnosticSources>,
}
struct DemandView {
    owner: Arc<FrontLowerQueries>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
struct State {
    current: Weak<FrontLowerQueries>,
    views: Vec<DemandView>,
    trace: Vec<(DefId, Vec<hirsel_types::GenericArgument>)>,
    charges: Vec<hirsel_engine_admission::MemoryReservation>,
    selected: Vec<(DefId, hirsel_types::GeneratedActualKey)>,
    producing: BTreeMap<DefId, (usize, hirsel_engine_admission::MemoryReservation)>,
    staged_log: Vec<(DefId, ContentHash)>,
}
struct Production {
    producer: Arc<DemandProducer>,
    definition: DefId,
}
impl Drop for Production {
    fn drop(&mut self) {
        let mut state = self
            .producer
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((count, _)) = state.producing.get_mut(&self.definition) {
            *count -= 1;
            if *count == 0 {
                state.producing.remove(&self.definition);
            }
        }
    }
}
struct DemandTransaction {
    producer: Arc<DemandProducer>,
    current: Weak<FrontLowerQueries>,
    views: usize,
    trace: usize,
    charges: usize,
    selected: usize,
    staged: usize,
    completed: bool,
}
impl DemandTransaction {
    fn new(producer: &Arc<DemandProducer>) -> Self {
        let state = producer.state.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            producer: producer.clone(),
            current: state.current.clone(),
            views: state.views.len(),
            trace: state.trace.len(),
            charges: state.charges.len(),
            selected: state.selected.len(),
            staged: state.staged_log.len(),
            completed: false,
        }
    }
}
impl Drop for DemandTransaction {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let (views, charges, staged) = {
            let mut state = self
                .producer
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if state.views.len() < self.views
                || state.trace.len() < self.trace
                || state.charges.len() < self.charges
                || state.selected.len() < self.selected
                || state.staged_log.len() < self.staged
            {
                return;
            }
            state.current = self.current.clone();
            state.trace.truncate(self.trace);
            state.selected.truncate(self.selected);
            let discarded = (
                state.views.split_off(self.views),
                state.charges.split_off(self.charges),
                state.staged_log.split_off(self.staged),
            );
            state.views.shrink_to_fit();
            state.charges.shrink_to_fit();
            state.trace.shrink_to_fit();
            state.selected.shrink_to_fit();
            state.staged_log.shrink_to_fit();
            discarded
        };
        {
            let mut selected = self
                .producer
                .staged
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for key in staged {
                selected.remove(&key);
            }
        }
        drop((views, charges));
    }
}
fn refused(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "generated instance demand".into(),
        message: message.into(),
    })
}
impl DemandProducer {
    pub(crate) fn new(
        engine: &QueryEngine,
        initial: Arc<FrontLowerQueries>,
        admission: DemandAdmission,
        weight: MemoryWeight,
        max_reads: usize,
        errors: RuntimeErrors,
    ) -> Result<Arc<Self>, QueryError> {
        let pool = |grant| {
            hirsel_engine_admission::MemoryReservationPool::new(grant)
                .map_err(|_| refused("pool metadata exceeds admission"))
        };
        let mut syntax = admission.syntax;
        let metadata = syntax
            .split((size_of::<Self>() + 2 * size_of::<usize>()) as u64)
            .ok_or_else(|| refused("producer owner metadata exceeds admission"))?;
        let producer = Arc::new(Self {
            carry: admission.carry,
            adapters: engine.adapters.source_phases(),
            state: Mutex::new(State {
                current: Arc::downgrade(&initial),
                views: vec![],
                trace: vec![],
                charges: vec![],
                selected: vec![],
                producing: BTreeMap::new(),
                staged_log: vec![],
            }),
            front: Arc::new(pool(admission.front)?),
            lower: scheduled_lower::LowerPartition::new(admission.lower),
            text: Arc::new(pool(admission.text)?),
            syntax: pool(syntax)?,
            weight,
            max_reads,
            errors,
            requests: Mutex::new(BTreeMap::new()),
            scopes: Mutex::new(BTreeMap::new()),
            failure: Mutex::new(None),
            rounds: std::sync::atomic::AtomicUsize::new(0),
            staged: Mutex::new(BTreeSet::new()),
            _metadata: metadata,
        });
        initial.bind_demand(&producer);
        initial.keep_demand(producer.clone());
        Ok(producer)
    }
    pub(crate) fn assembly(&self, capture: &NamespaceCapture) -> Result<QueryEngine, QueryError> {
        capture.assembly(&self.adapters)
    }
    fn capture_owner(&self, capture: &NamespaceCapture) -> Option<Arc<FrontLowerQueries>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .current
            .upgrade()
            .into_iter()
            .chain(state.views.iter().rev().map(|view| view.owner.clone()))
            .find(|owner| owner.capture().identity() == capture.identity())
    }
    pub(crate) fn take_failure(&self) -> Option<FrontRegistrationBootstrapError> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
    pub(crate) fn rounds(&self) -> usize {
        self.rounds.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn charge_instance(
        &self,
        capture: &NamespaceCapture,
        declaration: &DefPath,
        arguments: &[hirsel_types::GenericArgument],
        site: Span,
    ) -> Result<(), QueryError> {
        let id = DefId::intern(declaration);
        if !capture.instances.declarations.contains(&id) {
            return Ok(());
        }
        let key = hirsel_tir::InstanceBodies::key(capture.snapshot.types(), arguments)
            .ok_or_else(|| refused("selected instance has no canonical key"))?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.selected.contains(&(id, key)) {
            return Ok(());
        }
        if state
            .selected
            .iter()
            .filter(|(declaration, _)| *declaration == id)
            .count()
            >= MAX_GENERATED_INSTANCES
        {
            let syntax = capture.syntax(declaration)?;
            let body = generator(&syntax).ok_or(QueryError::NotImplemented)?;
            return Err(instance_limit(
                declaration,
                key,
                syntax.tree.expressions[body.0 as usize].origin.span,
                site,
            ));
        }
        let charge = Self::take(
            &self.syntax,
            (3 * (size_of::<(DefId, hirsel_types::GeneratedActualKey)>()
                + size_of::<hirsel_engine_admission::MemoryReservation>())) as u64,
        )?;
        state.selected.push((id, key));
        state.charges.push(charge);
        Ok(())
    }
    pub(crate) async fn charge_manifest(
        &self,
        capture: &NamespaceCapture,
        manifest: hirsel_engine_store::Reference,
        site: Span,
        reads: &QueryReads,
    ) -> Result<(), QueryError> {
        let demands = capture
            .manifest_demands(manifest, self.weight.scratch)
            .ok_or_else(|| refused("missing selected generation manifest"))?;
        if !demands.is_empty() {
            reads
                .publication()
                .await
                .map_err(|_| refused("cached instance selection cancelled before admission"))?;
        }
        for demand in demands {
            self.charge_instance(capture, &demand.declaration, &demand.arguments, site)?;
        }
        Ok(())
    }
    pub(crate) fn front_failure(&self, roots: &[DefPath]) -> Option<scope::FrontDemandError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let initial = state.current.upgrade()?;
        let fronts = state
            .views
            .iter()
            .rev()
            .map(|view| view.owner.body_front())
            .chain(std::iter::once(initial.body_front()));
        let mut checked = BTreeSet::new();
        for front in fronts {
            for path in roots {
                if checked.contains(path) {
                    continue;
                }
                if let Some(result) = front.completed_front(path) {
                    checked.insert(path.clone());
                    if let Err((reference, error)) = result {
                        return Some(scope::FrontDemandError::Front {
                            reference,
                            error: Some(error),
                            sources: front.namespace().sources.clone(),
                            owner: front.clone(),
                        });
                    }
                }
            }
        }
        None
    }
    pub(crate) fn lower_for(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<Arc<FrontLowerQueries>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .views
            .iter()
            .rev()
            .find(|view| view.owner.owns_query(id))
            .map(|view| view.owner.clone())
    }
    pub(crate) fn front_for(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<Arc<FrontBodyQueries>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .views
            .iter()
            .rev()
            .map(|view| view.owner.body_front())
            .find(|front| front.owns_query(id))
            .cloned()
    }
    pub(crate) fn query(
        &self,
        previous: Arc<NamespaceCapture>,
        declaration: &DefPath,
        site: Span,
    ) -> Result<hirsel_engine_runtime::QueryId, QueryError> {
        let key = ContentHash::of_parts(
            "hirsel.front.generation-demand.v2",
            [
                previous.identity().0.as_slice(),
                declaration.fingerprint().0.as_slice(),
            ],
        );
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        if !requests.contains_key(&key) {
            let allowance = Self::take(
                &self.syntax,
                (size_of::<DemandRequest>()
                    + 12 * size_of::<usize>()
                    + declaration.0.name.len()
                    + declaration.0.modules.iter().map(String::len).sum::<usize>())
                    as u64,
            )?;
            requests.insert(
                key,
                Arc::new(DemandRequest {
                    previous,
                    declaration: declaration.clone(),
                    site,
                    result: Mutex::new(None),
                    failure: Mutex::new(None),
                    _allowance: allowance,
                }),
            );
        }
        Ok(hirsel_engine_runtime::QueryId {
            kind: scheduled_lower::FRONT_LOWER_KIND,
            address: hirsel_artifact_store::Address(key),
        })
    }
    pub(crate) fn scope_query(
        &self,
        previous: Arc<NamespaceCapture>,
        roots: &[DefPath],
        policy: LoweringPolicy,
        stop_on_error: bool,
    ) -> Result<hirsel_engine_runtime::QueryId, QueryError> {
        let seed = ContentHash::of_parts(
            "hirsel.front.generation-scope.v2",
            [
                previous.identity().0.as_slice(),
                &[
                    u8::from(policy == LoweringPolicy::Reachable),
                    u8::from(stop_on_error),
                ],
                (roots.len() as u64).to_le_bytes().as_slice(),
            ],
        );
        let key = roots.iter().fold(seed, |key, root| {
            ContentHash::of_parts(
                "hirsel.front.generation-scope-root.v2",
                [key.0.as_slice(), root.fingerprint().0.as_slice()],
            )
        });
        let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
        if !scopes.contains_key(&key) {
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.add(size_of::<ScopeRequest>() + 12 * size_of::<usize>() + size_of_val(roots));
            for root in roots {
                weight.path(root);
            }
            let allowance = Self::take(&self.syntax, weight.bytes())?;
            scopes.insert(
                key,
                Arc::new(ScopeRequest {
                    previous,
                    roots: roots.to_vec(),
                    policy,
                    stop_on_error,
                    result: Mutex::new(None),
                    _allowance: allowance,
                }),
            );
        }
        Ok(hirsel_engine_runtime::QueryId {
            kind: scheduled_lower::FRONT_LOWER_KIND,
            address: hirsel_artifact_store::Address(key),
        })
    }
    pub(crate) fn scope_completed(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<Result<scheduled_lower::SuccessorCapture, QueryError>> {
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)?
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn owns_demand(&self, id: hirsel_engine_runtime::QueryId) -> bool {
        id.kind == scheduled_lower::FRONT_LOWER_KIND
            && (self
                .requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&id.address.0)
                || self
                    .scopes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&id.address.0))
    }
    pub(crate) fn failed(&self, id: hirsel_engine_runtime::QueryId) -> Option<DemandFailure> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)?
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn demand_failure(
        &self,
        declaration: &DefPath,
        previous: &Arc<NamespaceCapture>,
        error: QueryError,
        reads: Reads,
    ) -> Result<DemandFailure, QueryError> {
        let failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        let (sources, owner): (SourceMap, Arc<dyn Send + Sync>) = match failure.as_ref() {
            Some(FrontRegistrationBootstrapError::Runtime {
                reference,
                sources,
                owner,
                ..
            }) => (
                sources.clone(),
                owner
                    .failed(*reference)
                    .ok_or_else(|| refused("missing generator failure owner"))?
                    .into_data(),
            ),
            Some(FrontRegistrationBootstrapError::Acceptance(failure)) => {
                (failure.sources.clone(), failure._allowance.clone())
            }
            _ => (previous.sources.clone(), previous.clone()),
        };
        let allowance = Arc::new(Self::take(
            &self.syntax,
            (size_of::<namespace::DiagnosticSources>() + 8 * size_of::<usize>()) as u64
                + sources.retained_snapshot_metadata_bytes(),
        )?);
        Ok(DemandFailure {
            error,
            reads: Arc::new(reads),
            sources: Arc::new(namespace::DiagnosticSources {
                definition: DefId::intern(declaration),
                sources: sources.retain_owner(Arc::new((owner, allowance.clone()))),
                _allowance: allowance,
            }),
        })
    }
    pub(crate) fn completed(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<Result<scheduled_lower::SuccessorCapture, QueryError>> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)?
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub(crate) fn task(
        self: &Arc<Self>,
        id: hirsel_engine_runtime::QueryId,
    ) -> Result<Box<dyn hirsel_engine_runtime::LocalQueryTask>, hirsel_engine_store::Reference>
    {
        if let Some(scope) = self
            .scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)
            .cloned()
        {
            let producer = self.clone();
            return Ok(Box::new(hirsel_engine_runtime::LocalFutureTask::new(
                id,
                self.weight,
                self.max_reads,
                self.errors,
                move |reads| async move {
                    let result = producer
                        .complete_scope(
                            &scope.previous,
                            &scope.roots,
                            scope.policy,
                            scope.stop_on_error,
                            reads,
                        )
                        .await;
                    let namespace = result
                        .as_ref()
                        .map(|successor| successor.namespace().clone());
                    *scope.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
                    let namespace = namespace.map_err(|error| {
                        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                            eprintln!("OWNER SCOPE PROTOCOL {error:?}");
                        }
                        producer.errors.protocol
                    })?;
                    producer.scope_product(id, &namespace)
                },
            )?));
        }
        let request = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)
            .cloned()
            .ok_or(self.errors.protocol)?;
        let producer = self.clone();
        Ok(Box::new(hirsel_engine_runtime::LocalFutureTask::new(
            id,
            self.weight,
            self.max_reads,
            self.errors,
            move |reads| async move {
                let product = producer
                    .demand(
                        &request.previous,
                        &request.declaration,
                        &[],
                        request.site,
                        reads,
                    )
                    .await;
                let result = match product {
                    Ok(product) => match product.result {
                        Ok(_) => product
                            .successor
                            .ok_or_else(|| refused("demand completed without a successor")),
                        Err(error) => {
                            let failure = producer
                                .demand_failure(
                                    &request.declaration,
                                    &request.previous,
                                    error.clone(),
                                    product.reads,
                                )
                                .map_err(|_| producer.errors.resource)?;
                            *request.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(failure);
                            Err(error)
                        }
                    },
                    Err(error) => {
                        let failure = producer
                            .demand_failure(
                                &request.declaration,
                                &request.previous,
                                error.clone(),
                                Reads::empty(),
                            )
                            .map_err(|_| producer.errors.resource)?;
                        *request.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure);
                        Err(error)
                    }
                };
                let complete = result
                    .as_ref()
                    .map(|successor| successor.namespace().clone());
                *request.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
                let namespace = complete.map_err(|error| {
                    if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                        eprintln!("OWNER DEMAND PROTOCOL {error:?}");
                    }
                    producer.errors.protocol
                })?;
                producer.scope_product(id, &namespace)
            },
        )?))
    }
    fn scope_product(
        &self,
        id: hirsel_engine_runtime::QueryId,
        namespace: &NamespaceCapture,
    ) -> Result<hirsel_engine_runtime::ProductRef, hirsel_engine_store::Reference> {
        let bytes = size_of::<crate::queries::live_record::LiveRecord<ContentHash>>()
            + size_of::<hirsel_engine_admission::MemoryReservation>()
            + 4 * size_of::<usize>();
        let allowance = self.text.take(bytes as u64).ok_or(self.errors.resource)?;
        Ok(hirsel_engine_runtime::ProductRef::live(
            id.address,
            hirsel_artifact_store::Action(namespace.identity()),
            Arc::new(crate::queries::live_record::LiveRecord {
                value: namespace.manifest,
                domain: hirsel_engine_store::Domain {
                    kind: 0x4601_000b,
                    schema: 1,
                },
                records: namespace.records.clone(),
                source: (*namespace.anchors).clone(),
                serialization_working: self.weight.scratch,
                _admission: Arc::new(allowance),
            }),
        ))
    }

    pub(crate) async fn stage_declaration(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        declaration: &DefPath,
        reads: QueryReads,
    ) -> Result<Option<scheduled_lower::SuccessorCapture>, QueryError> {
        let Some((owner, ordinal)) = declaration.0.name.rsplit_once(':') else {
            return Ok(None);
        };
        let Some(name) = owner.strip_prefix("$comptime:") else {
            return Ok(None);
        };
        if ordinal.parse::<u32>().is_err() {
            return Ok(None);
        }
        let mut owner = declaration.clone();
        owner.0.name = name.into();
        if !previous.bodies.contains_key(&DefId::intern(&owner)) {
            return Ok(None);
        }
        let successor = self.stage_body(previous, &owner, reads).await?;
        let capture = successor.as_ref().map_or(previous, |next| next.namespace());
        if !capture
            .snapshot
            .header_products()
            .is_some_and(|headers| headers.staged_constants().contains_key(declaration))
        {
            return Err(refused(
                "stored staged declaration differs from its source producer",
            ));
        }
        Ok(successor)
    }
    pub(crate) async fn stage_body(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        path: &DefPath,
        reads: hirsel_engine_runtime::QueryReads,
    ) -> Result<Option<scheduled_lower::SuccessorCapture>, QueryError> {
        let id = DefId::intern(path);
        let Some(body) = previous.bodies.get(&id) else {
            return Ok(None);
        };
        let key = (id, body.implementation);
        if self
            .staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&key)
        {
            let current = self.current()?;
            return if current.capture().identity() == previous.identity() {
                Ok(None)
            } else {
                scheduled_lower::SuccessorCapture::new(previous, current).map(Some)
            };
        }
        if previous
            .snapshot
            .header_products()
            .and_then(|headers| headers.source_facts(id))
            .is_some_and(|facts| facts.comptime.is_empty())
        {
            return Ok(None);
        }
        if !scheduled_stage::has_staged_body(previous.syntax(path)?.as_ref()) {
            return Ok(None);
        }
        reads
            .publication()
            .await
            .map_err(|_| refused("staged demand cancelled before admission"))?;
        let owner = self.current()?;
        let stage = crate::FrontStageQueries::new(
            std::slice::from_ref(path),
            owner.body_front().clone(),
            self.weight,
            Self::take(&self.text, self.text.available_bytes())?,
            self.errors,
        )?;
        let query = stage
            .query(path)
            .ok_or_else(|| refused("staged owner is unavailable"))?;
        let task = stage
            .factory()
            .create(query)
            .map_err(|_| refused("staged producer admission"))?;
        let selected = reads.compose(task).await.map_err(|reference| {
            stage
                .failure(reference)
                .unwrap_or_else(|| refused("staged producer refused its product"))
        })?;
        let output = stage
            .outcome(selected)
            .ok_or_else(|| refused("staged output is unavailable"))?;
        reads
            .publication()
            .await
            .map_err(|_| refused("staged demand cancelled at acceptance"))?;
        let metadata = {
            let staged = self.staged.lock().unwrap_or_else(|e| e.into_inner());
            let mut next = hirsel_query_products::retained::MetadataWeight::default();
            next.tree::<(DefId, ContentHash), ()>(staged.len() + 1);
            let mut previous = hirsel_query_products::retained::MetadataWeight::default();
            if !staged.is_empty() {
                previous.tree::<(DefId, ContentHash), ()>(staged.len());
            }
            Self::take(
                &self.syntax,
                next.bytes().saturating_sub(previous.bytes())
                    + (3 * (size_of::<hirsel_engine_admission::MemoryReservation>()
                        + size_of::<(DefId, ContentHash)>())) as u64,
            )?
        };
        let mut engine = owner.capture().assembly(&self.adapters)?;
        let installed = output.installation_bytes()?;
        engine.accept_front_staged(&output, Self::take(&self.syntax, installed)?)?;
        let definitions = owner
            .capture()
            .bodies
            .keys()
            .map(|id| id.path().clone())
            .collect::<Vec<_>>();
        let registrations = owner
            .capture()
            .registrations
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let successor = self.view(&mut engine, &definitions, &registrations)?;
        let admitted = if successor.capture().identity() == previous.identity() {
            None
        } else {
            Some(scheduled_lower::SuccessorCapture::new(
                previous,
                successor.clone(),
            )?)
        };
        // A completed staged demand must expose its owner before the cache
        // marker: callers suspended in an older capture join that owner too.
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current = Arc::downgrade(&successor);
        if self
            .staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key)
        {
            state.staged_log.push(key);
            state.charges.push(metadata);
        }
        Ok(admitted)
    }
    fn current(&self) -> Result<Arc<FrontLowerQueries>, QueryError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .upgrade()
            .ok_or_else(|| refused("demand owner has ended"))
    }
    pub(crate) async fn reopen_manifest(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        manifest: hirsel_engine_store::Reference,
        reads: QueryReads,
    ) -> Option<scheduled_lower::SuccessorCapture> {
        let demands = previous.manifest_demands(manifest, self.weight.scratch)?;
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if demands.iter().any(|demand| {
                let selected = if demand.arguments.is_empty() {
                    demand.declaration.clone()
                } else {
                    let Some(key) = hirsel_tir::InstanceBodies::key(
                        previous.snapshot.types(),
                        &demand.arguments,
                    ) else {
                        return true;
                    };
                    instance_path(&demand.declaration, key)
                };
                state.producing.contains_key(&DefId::intern(&selected))
            }) {
                return None;
            }
        }
        let mut capture = previous.clone();
        let mut last = None;
        for demand in demands {
            let site = capture
                .bodies
                .get(&DefId::intern(&demand.declaration))?
                .queries
                .as_ref()
                .map(|keys| keys.site)
                .unwrap_or_default();
            let output = self
                .demand(
                    &capture,
                    &demand.declaration,
                    &demand.arguments,
                    site,
                    reads.clone(),
                )
                .await
                .ok()?;
            output.result.ok()?;
            if let Some(successor) = output.successor {
                capture = successor.namespace().clone();
                last = Some(successor);
            }
        }
        if !capture.manifest_inputs_hold(manifest, self.weight.scratch) {
            return None;
        }
        last
    }
    async fn complete_scope(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        roots: &[DefPath],
        _policy: LoweringPolicy,
        stop_on_error: bool,
        reads: QueryReads,
    ) -> Result<scheduled_lower::SuccessorCapture, QueryError> {
        let initial = self.current()?;
        // Nongeneric declarations already listed as roots are demanded in this
        // loop. Static traversal is needed only to close generic instances or
        // find a generator outside that explicit scope.
        let needs_discovery = !initial.capture().instances.declarations.is_empty()
            || initial
                .capture()
                .bodies
                .iter()
                .any(|(id, body)| body.generated && !roots.contains(id.path()));
        let mut checked = BTreeSet::new();
        for root in roots {
            let mut demanded = vec![root.clone()];
            loop {
                if self
                    .rounds
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    >= 65536
                {
                    return Err(refused("scope selection expansion limit"));
                }
                let owner = self.current()?;
                let mut pending = Vec::new();
                for path in &demanded {
                    if checked.contains(path) {
                        continue;
                    }
                    if owner
                        .capture()
                        .bodies
                        .get(&DefId::intern(path))
                        .is_some_and(|body| body.generated)
                        && !owner
                            .capture()
                            .registrations
                            .contains_key(&DefId::intern(path))
                    {
                        let syntax = owner.capture().syntax(path)?;
                        match self
                            .demand(
                                owner.capture(),
                                path,
                                &[],
                                syntax.item.name_span,
                                reads.clone(),
                            )
                            .await
                        {
                            Ok(product) => {
                                product.result?;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    let current = self.current()?;
                    let front = current.body_front();
                    let query = front
                        .query(path)
                        .ok_or_else(|| refused("scope body has no Front producer"))?;
                    let selected = reads.read_consuming(query).await;
                    if let Some(facts) = front.check_facts(selected.clone()) {
                        if let Err(error) = facts.result {
                            // A generated dependency can supply the private
                            // result proof this body needs. Discover it before
                            // treating a cacheable front diagnostic as final.
                            if !error.cacheable() {
                                return Err(error);
                            }
                        }
                    } else {
                        if let Err(reference) = selected
                            && let Some(error) = front.runtime_error(reference)
                        {
                            return Err(error);
                        }
                        return Err(refused(&format!(
                            "scope Front outcome is unavailable for {path}; query {query:?}; selected {selected:?}"
                        )));
                    }
                    checked.insert(path.clone());
                }
                if !needs_discovery {
                    break;
                }
                let owner = self.current()?;
                let mut engine = owner.capture().discovery_assembly(&self.adapters)?;
                let fronts = [owner.body_front().clone()];
                let discovery = engine.discover_front_instances(
                    &demanded,
                    &fronts,
                    Self::take(&self.syntax, self.syntax.available_bytes())?,
                )?;
                let requests = discovery.requests().to_vec();
                let missing = discovery.pending_bodies().to_vec();
                drop(discovery);
                drop(engine);
                let mut changed = false;
                for request in &requests {
                    let key = hirsel_tir::InstanceBodies::key(
                        owner.capture().snapshot.types(),
                        &request.actuals.arguments,
                    )
                    .ok_or_else(|| refused("open static instance"))?;
                    let instance = instance_path(&request.declaration, key);
                    if !self
                        .current()?
                        .capture()
                        .registrations
                        .contains_key(&DefId::intern(&instance))
                    {
                        let current = self.current()?;
                        match self
                            .demand(
                                current.capture(),
                                &request.declaration,
                                &request.actuals.arguments,
                                request.site,
                                reads.clone(),
                            )
                            .await
                        {
                            Ok(product) => {
                                product.result?;
                                changed = true;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    if !demanded.contains(&instance) && !checked.contains(&instance) {
                        pending.push(instance);
                    }
                }
                for path in &missing {
                    if !demanded.contains(path)
                        && !checked.contains(path)
                        && self
                            .current()?
                            .capture()
                            .bodies
                            .contains_key(&DefId::intern(path))
                    {
                        pending.push(path.clone());
                    }
                }
                for path in pending {
                    if !demanded.contains(&path) {
                        demanded.push(path);
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
                checked.clear();
            }
            let owner = self.current()?;
            for path in &demanded {
                let Some(query) = owner.body_front().query(path) else {
                    continue;
                };
                let Some(facts) = owner
                    .body_front()
                    .check_facts(reads.read_consuming(query).await)
                else {
                    continue;
                };
                if let Err(error) = facts.result {
                    if stop_on_error {
                        return Err(error);
                    }
                    continue;
                }
                let executable = owner
                    .capture()
                    .bodies
                    .get(&DefId::intern(path))
                    .is_some_and(|body| body.executable);
                // Ordinary lowering belongs to the consuming Check/Run graph.
                // Preparation only executes bodies that can select CT work.
                let needs_comptime = executable
                    && facts.lowering.as_ref().is_some_and(|use_| {
                        use_.needed_with(|id| {
                            owner
                                .capture()
                                .snapshot
                                .signature(id)
                                .is_some_and(|signature| {
                                    signature.is_ok_and(|signature| {
                                        matches!(
                                            signature.kind,
                                            hirsel_types::SignatureKind::Const { .. }
                                        )
                                    })
                                })
                        })
                    });
                if needs_comptime {
                    let query = owner
                        .query(path)
                        .ok_or_else(|| refused("scope body has no Lower producer"))?;
                    let selected = reads.read_consuming(query).await;
                    if let Err(reference) = selected {
                        let error = owner.failure(reference).ok_or_else(|| {
                            let reason = if reference == self.errors.resource {
                                "resource admission"
                            } else if reference == self.errors.protocol {
                                "runtime protocol"
                            } else if reference == self.errors.cancelled {
                                "cancellation"
                            } else if reference == self.errors.cycle {
                                "dependency cycle"
                            } else if reference == self.errors.panicked {
                                "producer panic"
                            } else {
                                "unavailable outcome"
                            };
                            refused(&format!("scope Lower {path} refused: {reason}"))
                        })?;
                        if stop_on_error || !error.cacheable() {
                            return Err(error);
                        }
                    }
                }
            }
        }
        scheduled_lower::SuccessorCapture::new(previous, self.current()?)
    }
    pub(crate) fn task_weight(&self) -> MemoryWeight {
        self.weight
    }
    fn take(
        pool: &hirsel_engine_admission::MemoryReservationPool,
        bytes: u64,
    ) -> Result<hirsel_engine_admission::MemoryReservation, QueryError> {
        pool.take(bytes).ok_or_else(|| refused(&format!(
            "producer inputs require {bytes} bytes; available {} bytes in their existing partition",
            pool.available_bytes()
        )))
    }
    fn view(
        self: &Arc<Self>,
        engine: &mut QueryEngine,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) -> Result<Arc<FrontLowerQueries>, QueryError> {
        engine.prepare_namespace_scope(registrations);
        let refreshed = registrations
            .iter()
            .map(|registration| registration.in_engine(engine, &self.syntax))
            .collect::<Result<Vec<_>, _>>()?;
        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            eprintln!(
                "OWNER VIEWS count={} registrations={} pool_available={}",
                state.views.len(),
                registrations.len(),
                self.front.available_bytes()
            );
        }
        let namespace =
            engine.capture_namespace(definitions, &refreshed, self.front.available_bytes())?;
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let existing = state
                .current
                .upgrade()
                .into_iter()
                .chain(state.views.iter().map(|view| view.owner.clone()))
                .find(|owner| owner.capture().identity() == namespace.identity());
            if let Some(owner) = existing {
                return Ok(owner);
            }
        }
        // Readiness belongs to the immutable capture, not to this assembler's
        // demand history. Ordinary shared syntax can still be cold here.
        let ready = definitions
            .iter()
            .filter(|path| {
                let id = DefId::intern(path);
                !namespace.bodies[&id].generated || namespace.registrations.contains_key(&id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let previous = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .upgrade();
        let front = engine.front_body_queries_in_pool(
            &ready,
            namespace,
            self.weight,
            self.front.clone(),
            self.errors,
            previous.as_ref().map(|previous| &**previous.body_front()),
        )?;
        if let Some(carry) = &self.carry {
            // An optional carry miss leaves ordinary production active.
            let _ = front.install_carry(carry.clone());
        }
        let lower = engine.front_lower_queries_in_partition(
            &ready,
            front,
            self.weight,
            self.max_reads,
            &self.lower,
            self.errors,
            None,
        )?;
        lower.bind_demand(self);
        let charge = Self::take(
            &self.syntax,
            (3 * (size_of::<Arc<FrontLowerQueries>>()
                + size_of::<hirsel_engine_admission::MemoryReservation>())) as u64,
        )?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.views.push(DemandView {
            owner: lower.clone(),
            _allowance: charge,
        });
        Ok(lower)
    }
    /// `previous` is the caller's consumption capture. The only successor
    /// returned here is an admitted descendant of that explicit capture.
    pub(crate) fn demand<'a>(
        self: &'a Arc<Self>,
        previous: &'a Arc<NamespaceCapture>,
        declaration: &'a DefPath,
        arguments: &'a [hirsel_types::GenericArgument],
        site: Span,
        reads: QueryReads,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<s1::DependencyProduct<Option<DefPath>>, QueryError>,
                > + 'a,
        >,
    > {
        Box::pin(async move {
            let declaration_id = DefId::intern(declaration);
            if !previous.instances.declarations.contains(&declaration_id)
                && !previous
                    .bodies
                    .get(&declaration_id)
                    .is_some_and(|body| body.generated)
            {
                return Ok(s1::DependencyProduct {
                    result: Ok(None),
                    reads: Reads::empty(),
                    successor: None,
                });
            }

            reads
                .publication()
                .await
                .map_err(|_| refused("demand cancelled before admission"))?;
            let mut transaction = DemandTransaction::new(self);
            let result = async {
                let current = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .current
                    .upgrade()
                    .ok_or_else(|| refused("demand owner has ended"))?;
                if previous.identity() != current.capture().identity()
                    && !previous.admits_successor(current.capture())
                {
                    return Err(refused(
                        "demand does not descend from its consuming capture",
                    ));
                }
                self.charge_instance(current.capture(), declaration, arguments, site)?;
                let original = current.capture().syntax(declaration)?;
                if generator(&original).is_none() {
                    return Ok(s1::DependencyProduct {
                        result: Ok(None),
                        reads: Reads::empty(),
                        successor: None,
                    });
                }
                let generic = instance_bodied(&original);
                if !generic {
                    if !arguments.is_empty() {
                        return Err(refused("nongeneric generator has actual arguments"));
                    }
                    if current
                        .capture()
                        .registrations
                        .contains_key(&declaration_id)
                    {
                        return Ok(s1::DependencyProduct {
                            result: Ok(Some(declaration.clone())),
                            reads: Reads::empty(),
                            successor: (previous.identity() != current.capture().identity())
                                .then(|| scheduled_lower::SuccessorCapture::new(previous, current))
                                .transpose()?,
                        });
                    }
                }
                let mut engine = if generic {
                    current.capture().assembly(&self.adapters)?
                } else {
                    current.capture().discovery_assembly(&self.adapters)?
                };
                let actual_key =
                    hirsel_tir::InstanceBodies::key(current.capture().snapshot.types(), arguments);
                let fresh = instance_bodied(&original)
                    && actual_key.is_some_and(|key| {
                        !current
                            .capture()
                            .instances
                            .instances
                            .get(&declaration_id)
                            .is_some_and(|rows| rows.contains_key(&key))
                    });
                let selected = if instance_bodied(&original) {
                    let request = FrontInstanceRequest {
                        declaration: declaration.clone(),
                        actuals: hirsel_types::Actuals {
                            arguments: arguments.to_vec(),
                        },
                        site,
                    };
                    let syntax = engine
                        .register_front_instance(
                            &request,
                            Self::take(&self.syntax, self.syntax.available_bytes())?,
                        )
                        .map_err(|failure| failure.error)?;
                    syntax.definition().clone()
                } else {
                    if !arguments.is_empty() {
                        return Err(refused("nongeneric generator has actual arguments"));
                    }
                    declaration.clone()
                };
                if current
                    .capture()
                    .registrations
                    .contains_key(&DefId::intern(&selected))
                {
                    return Ok(s1::DependencyProduct {
                        result: Ok(Some(selected)),
                        reads: Reads::empty(),
                        successor: (previous.identity() != current.capture().identity())
                            .then(|| scheduled_lower::SuccessorCapture::new(previous, current))
                            .transpose()?,
                    });
                }
                let mut definitions = current
                    .capture()
                    .bodies
                    .keys()
                    .map(|id| id.path().clone())
                    .collect::<Vec<_>>();
                if !definitions.contains(&selected) {
                    definitions.push(selected.clone());
                    definitions.sort();
                }
                let registrations = current
                    .capture()
                    .registrations
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let lower = if generic {
                    self.view(&mut engine, &definitions, &registrations)?
                } else {
                    current.clone()
                };
                // The selected head is an admitted prerequisite of private execution.
                // Nested demand descends from this explicit head capture.
                self.state.lock().unwrap_or_else(|e| e.into_inner()).current =
                    Arc::downgrade(&lower);

                let text = engine.front_registration_queries_in_pool(
                    std::slice::from_ref(&selected),
                    lower,
                    self.weight,
                    self.text.clone(),
                    self.errors,
                )?;
                let definition = DefId::intern(&selected);
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some((count, _)) = state.producing.get_mut(&definition) {
                        *count = count
                            .checked_add(1)
                            .ok_or_else(|| refused("producer depth overflow"))?;
                    } else {
                        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
                        weight
                            .tree::<DefId, (usize, hirsel_engine_admission::MemoryReservation)>(1);
                        state
                            .producing
                            .insert(definition, (1, Self::take(&self.syntax, weight.bytes())?));
                    }
                }
                let _production = Production {
                    producer: self.clone(),
                    definition,
                };
                let query = text
                    .query(&selected)
                    .ok_or_else(|| refused("missing generator execution input"))?;
                drop(engine);
                let product = text.clone().run_tentative(query, reads.clone()).await;
                let completed = product.as_ref().ok().cloned();
                let output = match product {
                    Ok(product) => text.outcome(product),
                    Err(reference) => {
                        let output = text.failed(reference);
                        if let Some(output) = &output
                            && let Err(mut error) = output.text()
                        {
                            if fresh && let QueryError::Diagnostics(diagnostics) = &mut error {
                                let key = actual_key.ok_or_else(|| {
                                    refused("selected instance has no canonical key")
                                })?;
                                for diagnostic in diagnostics {
                                    diagnostic.secondary.push(hirsel_diag::Label {
                                        span: site,
                                        message: format!(
                                            "selects instance {}",
                                            key.instance_name(declaration)
                                        ),
                                    });
                                }
                            }
                            *self.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(FrontRegistrationBootstrapError::Runtime {
                                    reference,
                                    error: Some(error.clone()),
                                    sources: output.sources().clone(),
                                    owner: text.clone(),
                                });
                            let owner =
                                self.capture_owner(output.namespace()).ok_or_else(|| {
                                    refused("failed generator lost its capture owner")
                                })?;
                            return Ok(s1::DependencyProduct {
                                result: Err(error),
                                reads: output.reads().clone(),
                                successor: Some(scheduled_lower::SuccessorCapture::new(
                                    previous, owner,
                                )?),
                            });
                        }
                        output
                    }
                }
                .ok_or_else(|| refused("generator execution refused its product"))?;
                let manifest = output
                    .namespace()
                    .manifest_for_reads(
                        DefId::intern(&selected),
                        output.reads(),
                        self.weight.scratch,
                    )
                    .ok_or_else(|| refused("generator demand manifest"))?;
                self.charge_manifest(output.namespace(), manifest, site, &reads)
                    .await?;
                // Cancellation is checked again after execution and before allocating
                // any accepted source. A cancelled future drops every tentative owner.
                reads
                    .publication()
                    .await
                    .map_err(|_| refused("demand cancelled at acceptance"))?;
                let metadata = Self::take(
                    &self.syntax,
                    (3 * (size_of::<(DefId, Vec<hirsel_types::GenericArgument>)>()
                        + size_of::<hirsel_engine_admission::MemoryReservation>())
                        + size_of_val(arguments)) as u64,
                )?;
                let observed = output.reads().clone();
                let output_capture = output.namespace().clone();
                let mut engine = output_capture.assembly(&self.adapters)?;
                let accepted = match engine.accept_front_registration(
                    output,
                    Self::take(&self.syntax, self.syntax.available_bytes())?,
                ) {
                    Ok(accepted) => accepted,
                    Err(failure) => {
                        let error = failure.error.clone();
                        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(FrontRegistrationBootstrapError::Acceptance(failure));
                        let owner = self
                            .capture_owner(&output_capture)
                            .ok_or_else(|| refused("rejected generator lost its capture owner"))?;
                        return Ok(s1::DependencyProduct {
                            result: Err(error),
                            reads: observed,
                            successor: Some(scheduled_lower::SuccessorCapture::new(
                                previous, owner,
                            )?),
                        });
                    }
                };
                reads
                    .publication()
                    .await
                    .map_err(|_| refused("demand cancelled during acceptance"))?;
                let mut registrations = output_capture
                    .registrations
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                registrations.push(accepted);
                let definitions = output_capture
                    .bodies
                    .keys()
                    .map(|id| id.path().clone())
                    .collect::<Vec<_>>();
                let successor = self.view(&mut engine, &definitions, &registrations)?;
                let result = scheduled_lower::SuccessorCapture::new(previous, successor.clone())?;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.current.upgrade().is_some_and(|published| {
                    published.capture().identity() != current.capture().identity()
                        && !published.capture().admits_successor(successor.capture())
                }) {
                    return Err(refused("publication conflicts with a committed dependency"));
                }
                state
                    .trace
                    .push((DefId::intern(declaration), arguments.to_vec()));
                state.charges.push(metadata);
                state.current = Arc::downgrade(&successor);
                drop(state);
                if let Some(completed) = completed {
                    text.commit(&completed);
                }
                Ok(s1::DependencyProduct {
                    result: Ok(Some(selected)),
                    reads: observed,
                    successor: Some(result),
                })
            }
            .await;
            transaction.completed = true;
            result
        })
    }
}

//! One ordered publication owner for instance demand. Execution suspends through
//! the existing query graph; no publication lock crosses a dependency wait.
use super::*;
use hirsel_engine_runtime::MemoryWeight;
use hirsel_engine_runtime::QueryReads;
use hirsel_engine_runtime::RuntimeErrors;

use std::sync::{Mutex, Weak};

pub(crate) struct DemandAdmission {
    pub carry: Option<Arc<FrontCarry>>,
    pub request: Arc<hirsel_engine_admission::ProcessBudget>,
}

pub(crate) struct DemandProducer {
    carry: Option<Arc<FrontCarry>>,
    adapters: SourcePhases,
    state: Mutex<State>,
    lower: scheduled_lower::LowerPartition,
    request: Arc<hirsel_engine_admission::ProcessBudget>,
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
    previous: Weak<NamespaceCapture>,
    roots: Vec<DefPath>,
    policy: LoweringPolicy,
    stop_on_error: bool,
    result: Mutex<Option<Result<scheduled_lower::WeakSuccessorCapture, QueryError>>>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
struct DemandRequest {
    previous: Weak<NamespaceCapture>,
    declaration: DefPath,
    site: Span,
    result: Mutex<Option<Result<scheduled_lower::WeakSuccessorCapture, QueryError>>>,
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
    owner: Weak<FrontLowerQueries>,
    front: Weak<FrontBodyQueries>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
struct InstanceSelection {
    declaration: DefId,
    key: hirsel_types::GeneratedActualKey,
    site: Span,
}
#[derive(Clone)]
enum CurrentView {
    Initial(Weak<FrontLowerQueries>),
    Committed(Arc<FrontLowerQueries>),
}
impl CurrentView {
    fn upgrade(&self) -> Option<Arc<FrontLowerQueries>> {
        match self {
            Self::Initial(owner) => owner.upgrade(),
            Self::Committed(owner) => Some(owner.clone()),
        }
    }
}
struct State {
    current: CurrentView,
    views: Vec<DemandView>,
    trace: Vec<(DefId, Vec<hirsel_types::GenericArgument>)>,
    charges: Vec<hirsel_engine_admission::MemoryReservation>,
    selected: Vec<InstanceSelection>,
    producing: BTreeMap<DefId, (usize, hirsel_engine_admission::MemoryReservation)>,
    staged_log: Vec<(DefId, ContentHash)>,
}
impl State {
    fn commit(&mut self, owner: &Arc<FrontLowerQueries>) {
        self.current = CurrentView::Committed(owner.clone());
    }
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
    current: CurrentView,
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
        let metadata = admission
            .request
            .reserve((size_of::<Self>() + 2 * size_of::<usize>()) as u64)
            .map_err(|_| refused("producer owner metadata exceeds the request footprint"))?;
        let producer = Arc::new(Self {
            carry: admission.carry,
            adapters: engine.adapters.source_phases(),
            state: Mutex::new(State {
                current: CurrentView::Initial(Arc::downgrade(&initial)),
                views: vec![],
                trace: vec![],
                charges: vec![],
                selected: vec![],
                producing: BTreeMap::new(),
                staged_log: vec![],
            }),
            lower: scheduled_lower::LowerPartition::new(&admission.request),
            request: admission.request,
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
        initial.bind_demand(&producer)?;
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
            .chain(
                state
                    .views
                    .iter()
                    .rev()
                    .filter_map(|view| view.owner.upgrade()),
            )
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
        if state
            .selected
            .iter()
            .any(|selection| selection.declaration == id && selection.key == key)
        {
            return Ok(());
        }
        if state
            .selected
            .iter()
            .filter(|selection| selection.declaration == id)
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
            &self.request,
            (3 * (size_of::<InstanceSelection>()
                + size_of::<hirsel_engine_admission::MemoryReservation>())) as u64,
        )?;
        state.selected.push(InstanceSelection {
            declaration: id,
            key,
            site,
        });
        state.charges.push(charge);
        Ok(())
    }
    fn label_selection(
        &self,
        declaration: &DefPath,
        key: Option<hirsel_types::GeneratedActualKey>,
        mut error: QueryError,
        sources: &SourceMap,
    ) -> QueryError {
        let site = key.and_then(|key| {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .selected
                .iter()
                .find(|selection| {
                    selection.declaration.path() == declaration && selection.key == key
                })
                .map(|selection| (key, selection.site))
        });
        if let Some((_, site)) = site {
            error = locate_failure(error, sources, site);
        }
        if let Some((key, site)) = site
            && let QueryError::Diagnostics(diagnostics) = &mut error
        {
            let label = hirsel_diag::Label {
                span: site,
                message: format!("selects instance {}", key.instance_name(declaration)),
            };
            for diagnostic in diagnostics {
                if diagnostic.secondary.contains(&label) {
                    continue;
                }
                let at = sources.resolve(diagnostic.primary);
                diagnostic
                    .notes
                    .push(format!("generated text span {}..{}", at.start, at.end,));
                diagnostic.primary = site;
                diagnostic.secondary.push(label.clone());
            }
        }
        error
    }
    pub(crate) fn charge_manifest<'r>(
        &self,
        capture: &NamespaceCapture,
        manifest: hirsel_engine_store::Reference,
        site: Span,
        reads: &'r QueryReads<'r>,
    ) -> Result<(), QueryError> {
        let demands = capture
            .manifest_demands(manifest, self.weight.scratch)
            .ok_or_else(|| refused("missing selected generation manifest"))?;
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
            .filter_map(|view| view.front.upgrade())
            .chain(std::iter::once(initial.body_front().clone()));
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
            .filter_map(|view| view.owner.upgrade())
            .find(|owner| owner.owns_query(id))
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
            .filter_map(|view| view.front.upgrade())
            .find(|front| front.owns_query(id))
    }
    pub(crate) fn front_for_eviction(
        &self,
        id: hirsel_engine_runtime::QueryId,
    ) -> Option<Arc<FrontBodyQueries>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .views
            .iter()
            .rev()
            .filter_map(|view| view.front.upgrade())
            .find(|front| front.has_cached_query(id))
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
        if requests
            .get(&key)
            .is_none_or(|request| request.previous.strong_count() == 0)
        {
            let allowance = Self::take(
                &self.request,
                (size_of::<DemandRequest>()
                    + 12 * size_of::<usize>()
                    + declaration.0.name.len()
                    + declaration.0.modules.iter().map(String::len).sum::<usize>())
                    as u64,
            )?;
            requests.insert(
                key,
                Arc::new(DemandRequest {
                    previous: Arc::downgrade(&previous),
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
        preparation_roots: Option<&[DefPath]>,
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
        // The address binds the full requested scope, even when discovery
        // can omit ordinary bodies that cannot publish namespace inputs.
        let roots = preparation_roots.unwrap_or(roots);
        let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
        if scopes
            .get(&key)
            .is_none_or(|scope| scope.previous.strong_count() == 0)
        {
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.add(size_of::<ScopeRequest>() + 12 * size_of::<usize>() + size_of_val(roots));
            for root in roots {
                weight.path(root);
            }
            let allowance = Self::take(&self.request, weight.bytes())?;
            scopes.insert(
                key,
                Arc::new(ScopeRequest {
                    previous: Arc::downgrade(&previous),
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
            .map(|result| {
                result.and_then(|successor| {
                    successor
                        .upgrade()
                        .ok_or_else(|| refused("settled demand lost its memo lease"))
                })
            })
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
            &self.request,
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
            .map(|result| {
                result.and_then(|successor| {
                    successor
                        .upgrade()
                        .ok_or_else(|| refused("settled demand lost its memo lease"))
                })
            })
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
            let previous = scope.previous.upgrade().ok_or(self.errors.protocol)?;
            let producer = self.clone();
            return Ok(Box::new(hirsel_engine_runtime::LocalFnTask::new(
                id,
                self.weight,
                self.errors,
                move |reads| {
                    let result = producer.complete_scope(
                        &previous,
                        &scope.roots,
                        scope.policy,
                        scope.stop_on_error,
                        reads,
                    );
                    *scope.result.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(result.clone().map(|successor| successor.downgrade()));
                    let successor = result.map_err(|error| {
                        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                            crate::queries::operation_owner_trace(format_args!(
                                "OWNER SCOPE PROTOCOL {error:?}"
                            ));
                        }
                        producer.errors.protocol
                    })?;
                    producer.scope_product(id, &previous, &successor)
                },
            )));
        }
        let request = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)
            .cloned()
            .ok_or(self.errors.protocol)?;
        let previous = request.previous.upgrade().ok_or(self.errors.protocol)?;
        let producer = self.clone();
        Ok(Box::new(hirsel_engine_runtime::LocalFnTask::new(
            id,
            self.weight,
            self.errors,
            move |reads| {
                let product =
                    producer.demand(&previous, &request.declaration, &[], request.site, reads);
                let result = match product {
                    Ok(product) => match product.result {
                        Ok(_) => product
                            .successor
                            .ok_or_else(|| refused("demand completed without a successor")),
                        Err(error) => {
                            let failure = producer
                                .demand_failure(
                                    &request.declaration,
                                    &previous,
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
                                &previous,
                                error.clone(),
                                Reads::empty(),
                            )
                            .map_err(|_| producer.errors.resource)?;
                        *request.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure);
                        Err(error)
                    }
                };
                *request.result.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(result.clone().map(|successor| successor.downgrade()));
                let successor = result.map_err(|error| {
                    if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                        crate::queries::operation_owner_trace(format_args!(
                            "OWNER DEMAND PROTOCOL {error:?}"
                        ));
                    }
                    producer.errors.protocol
                })?;
                producer.scope_product(id, &previous, &successor)
            },
        )))
    }
    fn scope_product(
        &self,
        id: hirsel_engine_runtime::QueryId,
        previous: &Arc<NamespaceCapture>,
        successor: &scheduled_lower::SuccessorCapture,
    ) -> Result<hirsel_engine_runtime::ProductRef, hirsel_engine_store::Reference> {
        let namespace = successor.namespace();
        let bytes = size_of::<crate::queries::live_record::LiveRecord<ContentHash>>()
            + size_of::<hirsel_engine_admission::MemoryReservation>()
            + size_of::<namespace::OperationLease>()
            + size_of::<scheduled_lower::SuccessorCapture>()
            + 4 * size_of::<usize>();
        let allowance = self
            .request
            .reserve(bytes as u64)
            .map_err(|_| self.errors.resource)?;
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
                _admission: Arc::new((allowance, previous.lease(), successor.clone())),
            }),
        ))
    }

    pub(crate) fn stage_declaration<'r>(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        declaration: &DefPath,
        reads: &'r QueryReads<'r>,
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
        let successor = self.stage_body(previous, &owner, reads)?;
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
    pub(crate) fn stage_body<'r>(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        path: &DefPath,
        reads: &'r QueryReads<'r>,
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
        let owner = self.current()?;
        let stage = crate::FrontStageQueries::new(
            std::slice::from_ref(path),
            owner.body_front().clone(),
            self.weight,
            Self::take(
                &self.request,
                crate::FrontStageQueries::input_bytes(std::slice::from_ref(path)),
            )?,
            self.errors,
        )?;
        let query = stage
            .query(path)
            .ok_or_else(|| refused("staged owner is unavailable"))?;
        let task = stage
            .factory()
            .create(query)
            .map_err(|_| refused("staged producer admission"))?;
        let selected = reads.compose(task).map_err(|reference| {
            stage
                .failure(reference)
                .unwrap_or_else(|| refused("staged producer refused its product"))
        })?;
        let output = stage
            .outcome(selected)
            .ok_or_else(|| refused("staged output is unavailable"))?;
        let metadata = {
            let staged = self.staged.lock().unwrap_or_else(|e| e.into_inner());
            let mut next = hirsel_query_products::retained::MetadataWeight::default();
            next.tree::<(DefId, ContentHash), ()>(staged.len() + 1);
            let mut previous = hirsel_query_products::retained::MetadataWeight::default();
            if !staged.is_empty() {
                previous.tree::<(DefId, ContentHash), ()>(staged.len());
            }
            Self::take(
                &self.request,
                next.bytes().saturating_sub(previous.bytes())
                    + (3 * (size_of::<hirsel_engine_admission::MemoryReservation>()
                        + size_of::<(DefId, ContentHash)>())) as u64,
            )?
        };
        let mut engine = owner.capture().assembly(&self.adapters)?;
        let installed = output.installation_bytes()?;
        engine.accept_front_staged(&output, Self::take(&self.request, installed)?)?;
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
        state.commit(&successor);
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
    pub(crate) fn current(&self) -> Result<Arc<FrontLowerQueries>, QueryError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .upgrade()
            .ok_or_else(|| refused("demand owner has ended"))
    }
    pub(crate) fn reopen_manifest<'r>(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        manifest: hirsel_engine_store::Reference,
        reads: &'r QueryReads<'r>,
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
    fn complete_scope<'r>(
        self: &Arc<Self>,
        previous: &Arc<NamespaceCapture>,
        roots: &[DefPath],
        _policy: LoweringPolicy,
        stop_on_error: bool,
        reads: &'r QueryReads<'r>,
    ) -> Result<scheduled_lower::SuccessorCapture, QueryError> {
        let initial = self.current()?;
        // Nongeneric declarations already listed as roots are demanded in this
        // loop. Static traversal also reaches imported staged declarations:
        // their helpers must be published before Run captures its inventory.
        let needs_discovery = !initial.capture().instances.declarations.is_empty()
            || initial.capture().bodies.iter().any(|(id, body)| {
                (body.generated
                    || initial
                        .capture()
                        .snapshot
                        .header_products()
                        .and_then(|headers| headers.source_facts(*id))
                        .is_some_and(|facts| facts.staged))
                    && !roots.contains(id.path())
            });
        drop(initial);
        let mut checked = BTreeSet::new();
        for root in roots {
            loop {
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
                            match self.demand(
                                owner.capture(),
                                path,
                                &[],
                                syntax.item.name_span,
                                reads.clone(),
                            ) {
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
                        let selected = reads.read_consuming(query);
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
                    drop(owner);
                    if !needs_discovery {
                        break;
                    }
                    let owner = self.current()?;
                    let mut engine = owner.capture().discovery_assembly(&self.adapters)?;
                    let fronts = [owner.body_front().clone()];
                    let discovery =
                        engine.discover_front_instances(&demanded, &fronts, &self.request)?;
                    let requests = discovery.requests().to_vec();
                    let missing = discovery.pending_bodies().to_vec();
                    drop(discovery);
                    drop(engine);
                    let types = owner.capture().snapshot.types().clone();
                    drop(owner);
                    let mut changed = false;
                    for request in &requests {
                        let key =
                            hirsel_tir::InstanceBodies::key(&types, &request.actuals.arguments)
                                .ok_or_else(|| refused("open static instance"))?;
                        let instance = instance_path(&request.declaration, key);
                        if !self
                            .current()?
                            .capture()
                            .registrations
                            .contains_key(&DefId::intern(&instance))
                        {
                            let current = self.current()?;
                            match self.demand(
                                current.capture(),
                                &request.declaration,
                                &request.actuals.arguments,
                                request.site,
                                reads.clone(),
                            ) {
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
                    let Some(facts) = owner.body_front().check_facts(reads.read_consuming(query))
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
                    let needs_comptime =
                        executable
                            && facts.lowering.as_ref().is_some_and(|use_| {
                                use_.needed_with(|id| {
                                    owner.capture().snapshot.signature(id).is_some_and(
                                        |signature| {
                                            signature.is_ok_and(|signature| {
                                                matches!(
                                                    signature.kind,
                                                    hirsel_types::SignatureKind::Const { .. }
                                                )
                                            })
                                        },
                                    )
                                })
                            });
                    if needs_comptime {
                        let query = owner
                            .query(path)
                            .ok_or_else(|| refused("scope body has no Lower producer"))?;
                        let selected = reads.read_consuming(query);
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
                if owner.capture().identity() == self.current()?.capture().identity() {
                    break;
                }
                // Lower may execute a staged body after static discovery. Settle
                // that root again in its new capture before admitting consumers.
                checked.clear();
            }
        }
        scheduled_lower::SuccessorCapture::new(previous, self.current()?)
    }
    pub(crate) fn task_weight(&self) -> MemoryWeight {
        self.weight
    }
    fn take(
        pool: &Arc<hirsel_engine_admission::ProcessBudget>,
        bytes: u64,
    ) -> Result<hirsel_engine_admission::MemoryReservation, QueryError> {
        pool.reserve(bytes).ok().ok_or_else(|| refused(&format!(
            "producer inputs require {bytes} bytes; available {} bytes in their existing partition",
            pool.free()
        )))
    }
    fn view(
        self: &Arc<Self>,
        engine: &mut QueryEngine,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) -> Result<Arc<FrontLowerQueries>, QueryError> {
        engine.prepare_namespace_scope();
        let refreshed = registrations
            .iter()
            .map(|registration| registration.in_engine(engine, &self.request))
            .collect::<Result<Vec<_>, _>>()?;
        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            crate::queries::operation_owner_trace(format_args!(
                "OWNER VIEWS count={} registrations={} pool_available={}",
                state.views.len(),
                registrations.len(),
                self.request.free()
            ));
            for (generation, view) in state.views.iter().enumerate() {
                if let Some(owner) = view.owner.upgrade() {
                    crate::queries::operation_owner_trace(format_args!(
                        "OWNER LIVE CAPTURE generation={} registrations={} lower_refs={} namespace_refs={} view_bytes={} front_retained={}",
                        generation + 1,
                        owner.capture().registrations.len(),
                        Arc::strong_count(&owner) - 1,
                        Arc::strong_count(owner.capture()),
                        owner.capture().retained_bytes(),
                        owner.body_front().retained_bytes(),
                    ));
                }
            }
        }
        let namespace = engine.capture_namespace(definitions, &refreshed, self.request.free())?;
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let existing = state
                .current
                .upgrade()
                .into_iter()
                .chain(state.views.iter().filter_map(|view| view.owner.upgrade()))
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
        let front = engine.front_body_queries_in_request(
            &ready,
            namespace,
            self.weight,
            self.request.clone(),
            self.errors,
            previous.as_ref().map(|previous| &**previous.body_front()),
        )?;
        if let Some(carry) = &self.carry {
            // An optional carry miss leaves ordinary production active.
            let _ = front.install_carry(carry.clone());
        }
        let previous = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .upgrade();
        let lower = engine.front_lower_queries_in_partition(
            &ready,
            front,
            self.weight,
            self.max_reads,
            &self.lower,
            self.errors,
            None,
            previous.as_deref(),
        )?;
        lower.bind_demand(self)?;
        let charge = Self::take(&self.request, (3 * size_of::<DemandView>()) as u64)?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.views.push(DemandView {
            owner: Arc::downgrade(&lower),
            front: Arc::downgrade(lower.body_front()),
            _allowance: charge,
        });
        Ok(lower)
    }
    /// `previous` is the caller's consumption capture. The only successor
    /// returned here is an admitted descendant of that explicit capture.
    pub(crate) fn demand<'a, 'r>(
        self: &'a Arc<Self>,
        previous: &'a Arc<NamespaceCapture>,
        declaration: &'a DefPath,
        arguments: &'a [hirsel_types::GenericArgument],
        site: Span,
        reads: &'r QueryReads<'r>,
    ) -> Result<s1::DependencyProduct<Option<DefPath>>, QueryError> {
        {
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

            let mut transaction = DemandTransaction::new(self);
            let mut diagnostic_sources = previous.sources.clone();
            let selected_key =
                hirsel_tir::InstanceBodies::key(previous.snapshot.types(), arguments);
            let result = (|| {
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
                    if let Some(registration) = current.capture().registrations.get(&declaration_id)
                    {
                        return Ok(s1::DependencyProduct {
                            result: Ok(Some(declaration.clone())),
                            reads: registration.reads().clone(),
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
                let selected = if instance_bodied(&original) {
                    let request = FrontInstanceRequest {
                        declaration: declaration.clone(),
                        actuals: hirsel_types::Actuals {
                            arguments: arguments.to_vec(),
                        },
                        site,
                    };
                    let syntax = engine
                        .register_front_instance(&request, &self.request)
                        .map_err(|mut failure| {
                            failure.error = self.label_selection(
                                declaration,
                                actual_key,
                                failure.error,
                                &failure.sources,
                            );
                            let error = failure.error.clone();
                            *self.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(FrontRegistrationBootstrapError::Acceptance(failure));
                            error
                        })?;
                    syntax.definition().clone()
                } else {
                    if !arguments.is_empty() {
                        return Err(refused("nongeneric generator has actual arguments"));
                    }
                    declaration.clone()
                };
                if let Some(registration) = current
                    .capture()
                    .registrations
                    .get(&DefId::intern(&selected))
                {
                    return Ok(s1::DependencyProduct {
                        result: Ok(Some(selected)),
                        reads: registration.reads().clone(),
                        successor: (previous.identity() != current.capture().identity())
                            .then(|| scheduled_lower::SuccessorCapture::new(previous, current))
                            .transpose()?,
                    });
                }
                if self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .producing
                    .contains_key(&DefId::intern(&selected))
                {
                    let body = generator(&original).ok_or(QueryError::NotImplemented)?;
                    return Err(QueryError::Diagnostics(vec![Diagnostic::error(
                        const { hirsel_diag::RuleId::of("CT-4") },
                        original.tree.expressions[body.0 as usize].origin.span,
                        "generation needs its own result",
                    )]));
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
                diagnostic_sources = engine.sources.clone();
                let lower = if generic {
                    self.view(&mut engine, &definitions, &registrations)?
                } else {
                    current.clone()
                };
                // The selected head is an admitted prerequisite of private execution.
                // Nested demand descends from this explicit head capture.
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .commit(&lower);

                let text = engine
                    .front_registration_queries(
                        std::slice::from_ref(&selected),
                        lower,
                        self.weight,
                        &self.request,
                        self.errors,
                    )
                    .map_err(|error| {
                        self.label_selection(declaration, actual_key, error, &engine.sources)
                    })?;
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
                            .insert(definition, (1, Self::take(&self.request, weight.bytes())?));
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
                let product = text.clone().run_tentative(query, reads.clone());
                let completed = product.as_ref().ok().cloned();
                let output = match product {
                    Ok(product) => text.outcome(product),
                    Err(reference) => {
                        let output = text.failed(reference);
                        if let Some(output) = &output
                            && let Err(error) = output.text()
                        {
                            let error = self.label_selection(
                                declaration,
                                actual_key,
                                error,
                                output.sources(),
                            );
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
                self.charge_manifest(output.namespace(), manifest, site, &reads)?;
                let metadata = Self::take(
                    &self.request,
                    (3 * (size_of::<(DefId, Vec<hirsel_types::GenericArgument>)>()
                        + size_of::<hirsel_engine_admission::MemoryReservation>())
                        + size_of_val(arguments)) as u64,
                )?;
                let observed = output.reads().clone();
                let output_capture = output.namespace().clone();
                let mut engine = output_capture.assembly(&self.adapters)?;
                let mut accepted = match engine.accept_front_registration(output, &self.request) {
                    Ok(accepted) => accepted,
                    Err(mut failure) => {
                        failure.error = self.label_selection(
                            declaration,
                            actual_key,
                            failure.error,
                            &failure.sources,
                        );
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
                if generic {
                    Arc::get_mut(&mut accepted)
                        .ok_or_else(|| refused("new accepted registration is already shared"))?
                        .instantiation = Some(site);
                }
                let observed = accepted.reads().clone();
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
                diagnostic_sources = engine.sources.clone();
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
                state.commit(&successor);
                drop(state);
                if let Some(completed) = completed {
                    text.commit(&completed);
                }
                Ok(s1::DependencyProduct {
                    result: Ok(Some(selected)),
                    reads: observed,
                    successor: Some(result),
                })
            })();
            transaction.completed = true;
            result.map_err(|error| {
                if self
                    .failure
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some()
                {
                    return error;
                }
                let error =
                    self.label_selection(declaration, selected_key, error, &diagnostic_sources);
                if matches!(error, QueryError::Diagnostics(_)) {
                    let required = diagnostic_sources
                        .retained_snapshot_metadata_bytes()
                        .checked_add(hirsel_query_products::retained::retained_query_error_bytes(
                            &error,
                        ))
                        .and_then(|bytes| {
                            bytes.checked_add(
                                (size_of::<FrontRegistrationFailure>() + 4 * size_of::<usize>())
                                    as u64,
                            )
                        })
                        .ok_or_else(|| refused("early generation diagnostic size overflow"));
                    let allowance =
                        match required.and_then(|bytes| Self::take(&self.request, bytes)) {
                            Ok(allowance) => Arc::new(allowance),
                            Err(error) => return error,
                        };
                    let failure = FrontRegistrationFailure {
                        definition: declaration_id,
                        span: Some(site),
                        error: error.clone(),
                        sources: diagnostic_sources.clone().retain_owner(allowance.clone()),
                        _allowance: allowance,
                    };
                    *self.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(FrontRegistrationBootstrapError::Acceptance(failure));
                }
                error
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_engine_admission::ProcessBudget;
    use hirsel_engine_store::MemStore;

    #[test]
    fn committed_successor_releases_predecessor_after_its_last_reader() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let mut engine =
                QueryEngine::new(default_environment(), crate::phases::engine_only_adapters())
                    .with_result_store(Arc::new(MemStore::default()));
            let file = engine
                .sources
                .add("main.hir", "pub fn main()->u8{1u8}\n".into())
                .unwrap();
            engine.index_source(file, &default_context()).unwrap();
            let file = engine
                .sources
                .add("helper.hir", "pub fn helper()->u8{2u8}\n".into())
                .unwrap();
            engine.index_source(file, &default_context()).unwrap();
            let all_definitions = engine.source_paths();
            let definitions = vec![all_definitions[0].clone()];
            let budget = ProcessBudget::new(64 << 20);
            let reference = |bytes: &[u8]| {
                hirsel_engine_store::Domain {
                    kind: 0x4600_00ff,
                    schema: 1,
                }
                .reference(bytes)
                .unwrap()
            };
            let errors = RuntimeErrors {
                resource: reference(b"resource"),
                cycle: reference(b"cycle"),
                cancelled: reference(b"cancelled"),
                protocol: reference(b"protocol"),
                panicked: reference(b"panic"),
            };
            let weight = MemoryWeight {
                retained: 1 << 20,
                scratch: 1 << 20,
                stack: 1 << 20,
            };
            let front = engine
                .front_body_queries(&definitions, weight, &budget, errors)
                .unwrap();
            let initial = engine
                .front_lower_queries(&definitions, front, weight, 256, &budget, errors)
                .unwrap();
            let producer = DemandProducer::new(
                &engine,
                initial,
                DemandAdmission {
                    carry: None,
                    request: budget.clone(),
                },
                weight,
                256,
                errors,
            )
            .unwrap();
            let first = producer.view(&mut engine, &definitions, &[]).unwrap();
            producer.state.lock().unwrap().commit(&first);
            let weak = Arc::downgrade(&first);
            let lease = first.capture().lease();
            let query = producer
                .query(first.capture().clone(), &definitions[0], Span::default())
                .unwrap();
            let successor =
                scheduled_lower::SuccessorCapture::new(first.capture(), first.clone()).unwrap();
            let marker = producer
                .scope_product(query, first.capture(), &successor)
                .unwrap();
            *producer.requests.lock().unwrap()[&query.address.0]
                .result
                .lock()
                .unwrap() = Some(Ok(successor.downgrade()));
            drop(successor);
            let second = producer.view(&mut engine, &all_definitions, &[]).unwrap();
            assert_ne!(first.capture().identity(), second.capture().identity());
            let transaction = DemandTransaction::new(&producer);
            producer.state.lock().unwrap().commit(&second);
            drop(transaction);
            assert!(
                Arc::ptr_eq(&producer.current().unwrap(), &first),
                "cancelled publication restores its committed predecessor"
            );
            producer.state.lock().unwrap().commit(&second);
            drop(first);
            assert!(
                weak.upgrade().is_some(),
                "an unconsumed memo pins its exact successor"
            );
            assert!(producer.completed(query).unwrap().is_ok());
            let held = producer.completed(query).unwrap().unwrap();
            drop(marker);
            assert!(
                weak.upgrade().is_some(),
                "the consuming reader takes over the memo lease"
            );
            let front_reader = held.owner().body_front().clone();
            let front_query = front_reader.query(&definitions[0]).unwrap();
            drop(held);
            assert!(
                weak.upgrade().is_none(),
                "a Front reader need not retain its Lower producer"
            );
            assert!(Arc::ptr_eq(
                &producer.front_for(front_query).unwrap(),
                &front_reader
            ));
            drop(front_reader);
            assert!(
                weak.upgrade().is_none(),
                "the publication cache must release the predecessor producer"
            );
            let retained = producer.request.free();
            let expected = lease.capture().retained_bytes();
            let read = lease.clone();
            drop(lease);
            assert_eq!(
                producer.request.free(),
                retained,
                "the held reader still pins its inputs"
            );
            assert_eq!(
                read.capture().bodies[&DefId::intern(&definitions[0])].implementation,
                second.capture().bodies[&DefId::intern(&definitions[0])].implementation
            );
            drop(read);
            assert_eq!(
                producer.request.free() - retained,
                expected,
                "the final reader returns the predecessor capture charge exactly once"
            );
            drop((producer, second, engine));
            assert_eq!(budget.reserved(), 0);
        });
    }
}

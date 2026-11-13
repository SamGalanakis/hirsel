//! One ordered publication owner for instance demand. Execution suspends through
//! the existing query graph; no publication lock crosses a dependency wait.
use super::*;
use hirsel_engine_runtime::MemoryWeight;
use hirsel_engine_runtime::QueryReads;
use hirsel_engine_runtime::RuntimeErrors;

use std::sync::{Mutex, Weak};

pub(crate) struct DemandAdmission {
    pub carry: Option<Arc<FrontCarry>>,
    pub lower_carry: Option<Arc<scheduled_lower::LowerCarry>>,
    pub request: Arc<hirsel_engine_admission::ProcessBudget>,
}

pub(crate) struct DemandProducer {
    carry: Option<Arc<FrontCarry>>,
    /// Every view's Lower graph adopts these rows where their reads still
    /// hold, so a definition whose inputs are unchanged lowers once.
    lower_carry: Option<Arc<scheduled_lower::LowerCarry>>,
    adapters: SourcePhases,
    state: Mutex<State>,
    lower: scheduled_lower::LowerPartition,
    request: Arc<hirsel_engine_admission::ProcessBudget>,
    weight: MemoryWeight,
    max_reads: usize,
    errors: RuntimeErrors,
    scopes: Mutex<BTreeMap<ContentHash, Arc<ScopeRequest>>>,
    rounds: std::sync::atomic::AtomicUsize,
    staged: Mutex<BTreeSet<(DefId, ContentHash)>>,
    base: Mutex<crate::queries::namespace::ViewBase>,
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
struct DemandView {
    owner: Weak<FrontLowerQueries>,
    front: Weak<FrontBodyQueries>,
    _allowance: hirsel_engine_admission::MemoryReservation,
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
    charges: Vec<hirsel_engine_admission::MemoryReservation>,
    staged_log: Vec<(DefId, ContentHash)>,
}
impl State {
    fn commit(&mut self, owner: &Arc<FrontLowerQueries>) {
        self.current = CurrentView::Committed(owner.clone());
    }
}
fn refused(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "staged evaluation".into(),
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
            lower_carry: admission.lower_carry,
            adapters: engine.adapters.source_phases(),
            state: Mutex::new(State {
                current: CurrentView::Initial(Arc::downgrade(&initial)),
                views: vec![],
                charges: vec![],
                staged_log: vec![],
            }),
            lower: scheduled_lower::LowerPartition::new(&admission.request),
            base: Mutex::new(crate::queries::namespace::ViewBase::new(
                admission.request.clone(),
            )),
            request: admission.request,
            weight,
            max_reads,
            errors,
            scopes: Mutex::new(BTreeMap::new()),
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
    pub(crate) fn rounds(&self) -> usize {
        self.rounds.load(std::sync::atomic::Ordering::Relaxed)
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
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id.address.0)
    }
    pub(crate) fn task(
        self: &Arc<Self>,
        id: hirsel_engine_runtime::QueryId,
    ) -> Result<Box<dyn hirsel_engine_runtime::LocalQueryTask>, hirsel_engine_store::Reference>
    {
        let scope = self
            .scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.address.0)
            .cloned()
            .ok_or(self.errors.protocol)?;
        let previous = scope.previous.upgrade().ok_or(self.errors.protocol)?;
        let producer = self.clone();
        Ok(Box::new(hirsel_engine_runtime::LocalFnTask::new(
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
                value: namespace.identity(),
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
        let successor = self.view(&mut engine, &definitions)?;
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
        let needs_discovery = initial.capture().bodies.iter().any(|(id, _)| {
            initial
                .capture()
                .snapshot
                .header_products()
                .and_then(|headers| headers.source_facts(*id))
                .is_some_and(|facts| facts.staged)
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
                    let missing = engine.discover_front_dependencies(&demanded, &fronts)?;
                    drop(engine);
                    drop(owner);
                    let mut changed = false;
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
    ) -> Result<Arc<FrontLowerQueries>, QueryError> {
        engine.prepare_namespace_scope();
        let namespace = {
            let mut base = self.base.lock().unwrap_or_else(|e| e.into_inner());
            engine.capture_namespace_after(definitions, self.request.free(), Some(&mut base))?
        };
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
        let previous = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .upgrade();
        let front = engine.front_body_queries_in_request(
            definitions,
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
            definitions,
            front,
            self.weight,
            self.max_reads,
            &self.lower,
            self.errors,
            previous.as_deref(),
        )?;
        if let Some(carry) = &self.lower_carry {
            // As for Front: a carry the graph refuses leaves ordinary Lower.
            let _ = lower.install_carry(carry.clone());
        }
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
                    lower_carry: None,
                    request: budget.clone(),
                },
                weight,
                256,
                errors,
            )
            .unwrap();
            let first = producer.view(&mut engine, &definitions).unwrap();
            producer.state.lock().unwrap().commit(&first);
            let weak = Arc::downgrade(&first);
            let lease = first.capture().lease();
            let query = producer
                .scope_query(
                    first.capture().clone(),
                    &definitions,
                    LoweringPolicy::Reachable,
                    true,
                    None,
                )
                .unwrap();
            let successor =
                scheduled_lower::SuccessorCapture::new(first.capture(), first.clone()).unwrap();
            let marker = producer
                .scope_product(query, first.capture(), &successor)
                .unwrap();
            *producer.scopes.lock().unwrap()[&query.address.0]
                .result
                .lock()
                .unwrap() = Some(Ok(successor.downgrade()));
            drop(successor);
            let second = producer.view(&mut engine, &all_definitions).unwrap();
            assert_ne!(first.capture().identity(), second.capture().identity());
            producer.state.lock().unwrap().commit(&second);
            drop(first);
            assert!(
                weak.upgrade().is_some(),
                "an unconsumed memo pins its exact successor"
            );
            assert!(producer.scope_completed(query).unwrap().is_ok());
            let held = producer.scope_completed(query).unwrap().unwrap();
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

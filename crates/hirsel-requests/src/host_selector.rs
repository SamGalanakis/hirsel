//! Host selection over admitted immutable producer graphs. This module owns
//! admission and output lifetimes, never a mutable program or a check cache.
use super::{
    CompileInputs, CompileOperation, CompileOutput, CompileRequest, CompileResult,
    CompilerReadiness, RequestLimits,
};
use hirsel_engine::{CheckCompletion, CheckOutput, ToolError, runtime::*};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub(super) use hirsel_engine::requests::source_graph::{
    GraphAdmission, LoadingAdmission, SnapshotPlan, retain_error, retain_sources,
};

/// Implemented only by installed compiler producer views. Public requests
/// carry no tasks, materializer callbacks or compiler/verifier capabilities.
pub(super) trait GraphProducts: Send + Sync {
    fn check_source_owner(&self) -> Option<Arc<dyn Send + Sync>> {
        None
    }
    fn metadata(&self) -> Option<Arc<InstalledMetadata>> {
        None
    }
    fn accepts(&self, _operation: &CompileOperation) -> bool {
        true
    }
    fn select(
        &self,
        operation: &CompileOperation,
    ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError>;
}
pub(super) struct InstalledGraph {
    pub path: PathBuf,
    pub inputs: CompileInputs,
    pub runtime: Arc<Mutex<AdmittedRuntime>>,
    pub products: Arc<dyn GraphProducts>,
}
pub(super) struct InstalledMetadata {
    pub path: PathBuf,
    pub inputs: CompileInputs,
    pub products: Arc<dyn GraphProducts>,
    pub errors: RuntimeErrors,
}
impl GraphProducts for InstalledMetadata {
    fn accepts(&self, operation: &CompileOperation) -> bool {
        self.products.accepts(operation)
    }
    fn select(
        &self,
        operation: &CompileOperation,
    ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError> {
        self.products.select(operation)
    }
}
struct OutputOwner {
    _products: Arc<dyn GraphProducts>,
}
type OutputLeases = Arc<Mutex<Vec<Weak<dyn Send + Sync>>>>;
struct OutputLease {
    _owner: Arc<dyn Send + Sync>,
}
fn lease_output(leases: &OutputLeases, owner: Arc<dyn Send + Sync>) -> Arc<dyn Send + Sync> {
    let owner: Arc<dyn Send + Sync> = Arc::new(OutputLease { _owner: owner });
    let mut leases = leases.lock().unwrap_or_else(|error| error.into_inner());
    leases.retain(|lease| lease.strong_count() != 0);
    leases.push(Arc::downgrade(&owner));
    owner
}
/// One hot-graph retention authority per process budget. Releasing an idle
/// graph drops its runtime/stacks outside the index lock. Live leases and
/// externally held source products keep their independent admitted owners.
pub(super) struct GraphBudget {
    budget: Arc<ProcessBudget>,
    reclaim: Arc<dyn Fn() + Send + Sync>,
    hot: Mutex<Vec<Arc<InstalledGraph>>>,
    metadata: Mutex<Vec<Arc<InstalledMetadata>>>,
}
type BudgetIndex = Vec<(Weak<ProcessBudget>, Weak<GraphBudget>)>;
static BUDGETS: OnceLock<Mutex<BudgetIndex>> = OnceLock::new();
impl GraphBudget {
    pub fn process_budget(&self) -> Arc<ProcessBudget> {
        self.budget.clone()
    }
    pub fn reserve_product(
        budget: &Arc<ProcessBudget>,
        bytes: u64,
    ) -> std::io::Result<MemoryReservation> {
        let owner = Self::shared(budget);
        match budget.reserve_once(bytes) {
            Ok(reservation) => Ok(reservation),
            Err(_) => {
                owner.release_idle();
                budget.reserve_once(bytes)
            }
        }
    }
    fn shared(budget: &Arc<ProcessBudget>) -> Arc<Self> {
        let mut budgets = BUDGETS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        budgets.retain(|(budget, owner)| budget.strong_count() != 0 && owner.strong_count() != 0);
        if let Some(owner) = budgets.iter().find_map(|(selected, owner)| {
            selected
                .upgrade()
                .filter(|selected| Arc::ptr_eq(selected, budget))
                .and_then(|_| owner.upgrade())
        }) {
            return owner;
        }
        let owner = Arc::new_cyclic(|owner: &Weak<Self>| {
            let idle = owner.clone();
            Self {
                budget: budget.clone(),
                reclaim: Arc::new(move || {
                    if let Some(owner) = idle.upgrade() {
                        owner.release_idle();
                    }
                }),
                hot: Mutex::new(vec![]),
                metadata: Mutex::new(vec![]),
            }
        });
        budget.set_idle_reclaimer(Arc::downgrade(&owner.reclaim));
        budgets.push((Arc::downgrade(budget), Arc::downgrade(&owner)));
        owner
    }
    /// A request whose selection holds no live graph waits, holding nothing,
    /// for other requests' releases instead of refusing; one that holds a graph
    /// could wait on itself, so it refuses as soon as idle graphs cannot make
    /// room. A request that cannot fit even alone, or a host whose charge stops
    /// moving for `ADMISSION_STALL`, is refused rather than left waiting.
    pub fn begin(
        &self,
        limits: RequestLimits,
        may_wait: bool,
        native_transport: u64,
    ) -> Result<LoadingAdmission, ToolError> {
        let fits_alone = limits
            .runtime_bytes
            .checked_add(limits.snapshot_bytes)
            .and_then(|bytes| bytes.checked_add(limits.transport_bytes))
            .and_then(|bytes| bytes.checked_add(native_transport))
            .is_some_and(|bytes| bytes <= self.budget.limit());
        let mut seen = self.budget.reserved();
        let mut moved = std::time::Instant::now();
        loop {
            let error = match LoadingAdmission::reserve_once(&self.budget, limits, native_transport)
            {
                Ok(admission) => return Ok(admission),
                Err(error) => error,
            };
            self.release_idle();
            if let Ok(admission) =
                LoadingAdmission::reserve_once(&self.budget, limits, native_transport)
            {
                return Ok(admission);
            }
            let reserved = self.budget.reserved();
            if reserved != seen {
                seen = reserved;
                moved = std::time::Instant::now();
            }
            if !may_wait || !fits_alone || reserved == 0 || moved.elapsed() >= ADMISSION_STALL {
                return Err(error);
            }
            self.budget
                .wait_for_release(std::time::Duration::from_secs(1));
        }
    }
    pub fn admit(
        &self,
        limits: RequestLimits,
        plan: SnapshotPlan,
    ) -> Result<GraphAdmission, ToolError> {
        let snapshot = plan
            .source
            .checked_add(plan.front)
            .and_then(|bytes| bytes.checked_add(plan.lower))
            .and_then(|bytes| bytes.checked_add(plan.check))
            .and_then(|bytes| bytes.checked_add(plan.growth))
            .and_then(|bytes| bytes.checked_add(plan.run));
        if snapshot.is_none_or(|bytes| bytes > limits.snapshot_bytes) {
            return GraphAdmission::reserve_once(&self.budget, limits, plan);
        }
        match GraphAdmission::reserve_once(&self.budget, limits, plan) {
            Ok(admission) => Ok(admission),
            Err(_) => {
                self.release_idle();
                GraphAdmission::reserve_once(&self.budget, limits, plan)
            }
        }
    }
    fn keep(&self, graph: Arc<InstalledGraph>) {
        let mut hot = self.hot.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = hot
            .iter()
            .position(|previous| Arc::ptr_eq(previous, &graph))
        {
            hot.remove(previous);
        }
        hot.push(graph);
    }
    /// Charge a loading admission again after lent work released its part.
    pub fn readmit(
        &self,
        loading: &mut LoadingAdmission,
        runtime: u64,
        snapshot: u64,
    ) -> Result<(), ToolError> {
        let reserve = || -> std::io::Result<_> {
            Ok((
                self.budget.reserve_once(runtime)?,
                self.budget.reserve_once(snapshot)?,
            ))
        };
        let (runtime, snapshot) = match reserve() {
            Ok(reserved) => reserved,
            Err(_) => {
                self.release_idle();
                reserve().map_err(ToolError::Io)?
            }
        };
        // Both come from this budget, as the loading charge does.
        loading.join(runtime, snapshot);
        Ok(())
    }
    fn release_idle(&self) {
        let discarded = {
            let mut hot = self.hot.lock().unwrap_or_else(|error| error.into_inner());
            let mut discarded = vec![];
            let mut index = 0;
            while index < hot.len() {
                if Arc::strong_count(&hot[index]) == 1
                    && Arc::strong_count(&hot[index].runtime) == 1
                {
                    discarded.push(hot.remove(index));
                } else {
                    index += 1;
                }
            }
            discarded
        };
        drop(discarded);
        let discarded = {
            let mut metadata = self
                .metadata
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut discarded = vec![];
            let mut index = 0;
            while index < metadata.len() {
                if Arc::strong_count(&metadata[index]) == 1 {
                    discarded.push(metadata.remove(index));
                } else {
                    index += 1;
                }
            }
            discarded
        };
        drop(discarded);
    }
}
/// How long a waiting admission tolerates a process charge that neither grows
/// nor shrinks before it refuses.
const ADMISSION_STALL: std::time::Duration = std::time::Duration::from_secs(600);
pub(super) struct GraphSelections {
    retention: Arc<GraphBudget>,
    retain_idle: bool,
    live: Vec<Weak<InstalledGraph>>,
    metadata: Vec<Weak<InstalledMetadata>>,
    outputs: OutputLeases,
}
impl Drop for GraphSelections {
    fn drop(&mut self) {
        // Its hot graphs may now be idle; a waiting admission can release them.
        self.retention.budget.notify_idle();
    }
}
impl GraphSelections {
    pub fn new(budget: &Arc<ProcessBudget>, retain_idle: bool) -> Self {
        Self {
            retention: GraphBudget::shared(budget),
            retain_idle,
            live: vec![],
            metadata: vec![],
            outputs: Arc::new(Mutex::new(vec![])),
        }
    }
    pub fn select(
        &mut self,
        request: &CompileRequest,
        install: impl FnOnce(&GraphBudget, bool) -> Result<Arc<InstalledGraph>, ToolError>,
    ) -> Result<
        hirsel_engine::requests::SelectedRequest<AdmittedRuntime, CompilerReadiness>,
        ToolError,
    > {
        self.live.retain(|graph| graph.strong_count() != 0);
        let selected = self
            .live
            .iter()
            .rev()
            .filter_map(Weak::upgrade)
            .find(|graph| {
                graph.path == request.path
                    && graph.inputs == request.inputs
                    && graph.products.accepts(&request.operation)
            });
        self.metadata
            .retain(|metadata| metadata.strong_count() != 0);
        let metadata = selected
            .is_none()
            .then(|| {
                self.metadata
                    .iter()
                    .rev()
                    .filter_map(Weak::upgrade)
                    .find(|metadata| {
                        metadata.path == request.path
                            && metadata.inputs == request.inputs
                            && metadata.products.accepts(&request.operation)
                    })
            })
            .flatten();
        let selected = match (selected, metadata) {
            (None, Some(metadata)) => {
                // Empty roots need only scheduler bookkeeping, with no producer
                // factories or retained worker stacks from the retired graph.
                let bytes = hirsel_intern::jobs::COMPILER_STACK_BYTES as u64
                    + AdmittedRuntime::query_capacity_bytes(16).unwrap();
                let reservation = GraphBudget::reserve_product(&self.retention.budget, bytes)
                    .map_err(ToolError::Io)?;
                let runtime = AdmittedRuntime::with_query_capacity(
                    hirsel_intern::jobs::Jobs::ONE,
                    reservation,
                    metadata.errors,
                    16,
                )
                .map_err(ToolError::Io)?;
                let graph = Arc::new(InstalledGraph {
                    path: metadata.path.clone(),
                    inputs: metadata.inputs.clone(),
                    runtime: Arc::new(Mutex::new(runtime)),
                    products: metadata,
                });
                self.live.push(Arc::downgrade(&graph));
                Some(graph)
            }
            (selected, _) => selected,
        };
        let graph = match selected {
            Some(graph) => graph,
            None => {
                // Graphs only the hot index holds are idle, not this
                // selection's own outputs.
                let no_active_graphs = self.live.iter().all(|graph| graph.strong_count() <= 1);
                let may_wait = no_active_graphs && {
                    let mut outputs = self
                        .outputs
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    outputs.retain(|output| output.strong_count() != 0);
                    outputs.is_empty()
                };
                let graph = install(&self.retention, may_wait)?;
                if graph.path != request.path || graph.inputs != request.inputs {
                    return Err(ToolError::Runtime(
                        "installed compiler graph differs from selected inputs".into(),
                    ));
                }
                self.live.push(Arc::downgrade(&graph));
                if !self.retain_idle
                    && let Some(metadata) = graph.products.metadata()
                {
                    self.metadata.push(Arc::downgrade(&metadata));
                    self.retention
                        .metadata
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(metadata);
                }
                graph
            }
        };
        let (roots, mut output) = graph.products.select(&request.operation).map_err(|error| {
            retain_error(
                error,
                lease_output(
                    &self.outputs,
                    Arc::new(OutputOwner {
                        _products: graph.products.clone(),
                    }),
                ),
            )
        })?;
        let read_scope = output.read_scope.take();
        let ready_graph = graph.clone();
        let failed_graph = graph.clone();
        let ready_outputs = self.outputs.clone();
        let failed_outputs = self.outputs.clone();
        if self.retain_idle {
            self.retention.keep(graph.clone());
        }
        Ok(hirsel_engine::requests::SelectedRequest {
            runtime: graph.runtime.clone(),
            roots,
            output: CompilerReadiness {
                read_scope,
                materialize: Arc::new(move |products| {
                    let completed = (output.materialize)(products)?;
                    // A checked-products receipt owns its own output and
                    // receipt charges, so like Check it keeps only sources.
                    let owner: Arc<dyn Send + Sync> = if matches!(
                        completed.result,
                        CompileResult::Check(_) | CompileResult::CheckedProducts { .. }
                    ) {
                        ready_graph.products.check_source_owner()
                    } else {
                        None
                    }
                    .unwrap_or_else(|| {
                        Arc::new(OutputOwner {
                            _products: ready_graph.products.clone(),
                        })
                    });
                    Ok(Arc::new(
                        CompileOutput::new(
                            completed.entry.clone(),
                            completed.inputs.clone(),
                            completed.work.clone(),
                            completed.result.clone(),
                        )
                        .retain_owner(lease_output(&ready_outputs, owner)),
                    ))
                }),
                failure: Arc::new(move |reference| {
                    let owner: Arc<dyn Send + Sync> = Arc::new(OutputOwner {
                        _products: failed_graph.products.clone(),
                    });
                    retain_error(
                        (output.failure)(reference),
                        lease_output(&failed_outputs, owner),
                    )
                }),
            },
        })
    }
}
/// Already completed export outputs. Native receipt/object bytes come from
/// their ordinary selected producers; formatting opens no compiler bodies.
pub(super) struct NativeArtifacts {
    pub receipts: Vec<u8>,
    pub objects: Vec<(hirsel_native::Backend, Result<Vec<Vec<u8>>, String>)>,
}
pub(super) fn completed_artifacts(
    checked: &CheckOutput,
    tir: Result<Vec<u8>, String>,
    native: Option<NativeArtifacts>,
) -> BTreeMap<String, Vec<u8>> {
    use std::fmt::Write;
    let mut artifacts = BTreeMap::new();
    let mut diagnostics = String::new();
    for diagnostic in &checked.diagnostics {
        writeln!(diagnostics, "{}", diagnostic.to_json(&checked.sources)).unwrap();
    }
    if let CheckCompletion::Incomplete { phase } = checked.completion {
        writeln!(diagnostics, "incomplete: {}", phase.phase_name()).unwrap();
    }
    artifacts.insert("diagnostics".into(), diagnostics.into_bytes());
    if checked.completion != CheckCompletion::Complete
        || checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == hirsel_diag::Severity::Error)
    {
        return artifacts;
    }
    artifacts.insert(
        "tir".into(),
        tir.unwrap_or_else(|error| format!("export: {error}").into_bytes()),
    );
    if let Some(native) = native {
        artifacts.insert("receipts".into(), native.receipts);
        for (backend, objects) in native.objects {
            match objects {
                Ok(objects) => {
                    for (index, bytes) in objects.into_iter().enumerate() {
                        artifacts.insert(format!("objects/{}/{index:04}.o", backend.name()), bytes);
                    }
                }
                Err(error) => {
                    artifacts.insert(
                        format!("objects/{}/error", backend.name()),
                        error.into_bytes(),
                    );
                }
            }
        }
    }
    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_hold_metadata_outputs_keep_their_index_owner_and_share_pending_runtime() {
        struct Metadata(MemoryReservation);
        impl GraphProducts for Metadata {
            fn accepts(&self, operation: &CompileOperation) -> bool {
                matches!(operation, CompileOperation::RecomputeCounts { .. })
            }
            fn select(
                &self,
                _: &CompileOperation,
            ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError> {
                assert_eq!(self.0.bytes(), 1);
                let (_, inputs) = hirsel_engine::requests::observe_inputs(
                    hirsel_engine::loader_inputs::InputLimits {
                        bytes: 0,
                        operations: 0,
                    },
                    1,
                    |_| Ok::<_, std::io::Error>(()),
                )
                .unwrap();
                Ok((
                    vec![],
                    CompilerReadiness {
                        read_scope: None,
                        materialize: Arc::new(move |products| {
                            assert!(products.is_empty());
                            Ok(Arc::new(CompileOutput::new(
                                hirsel_engine::entry_path(&hirsel_engine::default_context()),
                                inputs.clone().into(),
                                Default::default(),
                                CompileResult::RecomputeCounts(vec![]),
                            )))
                        }),
                        failure: Arc::new(|_| {
                            ToolError::Runtime("unexpected metadata failure".into())
                        }),
                    },
                ))
            }
        }
        let (_, inputs) = hirsel_engine::requests::observe_inputs(
            hirsel_engine::loader_inputs::InputLimits {
                bytes: 0,
                operations: 0,
            },
            1,
            |_| Ok::<_, std::io::Error>(()),
        )
        .unwrap();
        let request = CompileRequest {
            path: "metadata.hir".into(),
            inputs: inputs.clone().into(),
            operation: CompileOperation::RecomputeCounts { queries: vec![] },
        };
        let error = hirsel_engine::store::Domain {
            kind: 0x4600_00fe,
            schema: 1,
        }
        .reference(b"fixture error")
        .unwrap();
        let budget = ProcessBudget::new(2 * hirsel_intern::jobs::COMPILER_STACK_BYTES as u64);
        let mut selector = GraphSelections::new(&budget, false);
        let metadata = Arc::new(InstalledMetadata {
            path: request.path.clone(),
            inputs: inputs.into(),
            products: Arc::new(Metadata(budget.reserve(1).unwrap())),
            errors: RuntimeErrors {
                resource: error,
                cycle: error,
                cancelled: error,
                protocol: error,
                panicked: error,
            },
        });
        let indexed = Arc::downgrade(&metadata);
        selector.metadata.push(indexed.clone());
        selector.retention.metadata.lock().unwrap().push(metadata);
        let first = selector
            .select(&request, |_, _| {
                panic!("metadata must not reinstall source producers")
            })
            .unwrap();
        let second = selector
            .select(&request, |_, _| panic!("pending metadata must coalesce"))
            .unwrap();
        assert!(Arc::ptr_eq(&first.runtime, &second.runtime));
        let output = (first.output.materialize)(&[]).unwrap();
        drop(first);
        drop(second);
        assert_eq!(
            budget.reserved(),
            1,
            "completed metadata retains no scheduler grant"
        );
        selector.retention.release_idle();
        assert!(
            indexed.upgrade().is_some(),
            "a live metadata output keeps its discoverable owner"
        );
        drop(output);
        selector.retention.release_idle();
        assert!(indexed.upgrade().is_none());
        drop(selector);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn loading_transport_transfer_keeps_one_shared_charge_before_s0() {
        use hirsel_engine::store::{Domain, NoneStore, RequestStore, ResultStore};
        let budget = ProcessBudget::new(8320);
        let limits = RequestLimits {
            runtime_bytes: 64,
            snapshot_bytes: 64,
            transport_bytes: 8192,
        };
        let mut loading = LoadingAdmission::reserve(&budget, limits, 0).unwrap();
        let store = Arc::new(RequestStore::new(
            Arc::new(NoneStore),
            loading.take_transport(),
        ));
        assert_eq!(budget.reserved(), 8320);
        assert!(budget.reserve(1).is_err());
        let record = store
            .put_content(Domain { kind: 1, schema: 1 }, b"source input")
            .unwrap();
        let admission = loading
            .finish(SnapshotPlan {
                source: 16,
                front: 16,
                lower: 16,
                check: 16,
                growth: 0,
                run: 0,
            })
            .unwrap();
        assert_eq!(admission.transport.bytes(), 0);
        assert_eq!(budget.reserved(), 8320);
        let held = store.get_content(record).unwrap();
        drop(admission);
        drop(store);
        assert_eq!(
            budget.reserved(),
            8192,
            "a held transport view keeps its original admission"
        );
        assert_eq!(&*held, b"source input");
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn loading_admission_transfers_charge_and_holds_failed_diagnostic_sources() {
        let limits = RequestLimits {
            runtime_bytes: 40,
            snapshot_bytes: 40,
            transport_bytes: 20,
        };
        let budget = ProcessBudget::new(100);
        let loading = LoadingAdmission::reserve(&budget, limits, 0).unwrap();
        assert_eq!(budget.reserved(), 100);
        assert!(budget.reserve(1).is_err());
        let graph = loading
            .finish(SnapshotPlan {
                source: 5,
                front: 5,
                lower: 5,
                growth: 5,
                check: 5,
                run: 0,
            })
            .unwrap();
        assert_eq!(budget.reserved(), 85);
        drop(graph);
        assert_eq!(budget.reserved(), 0);
        for plan in [
            SnapshotPlan {
                source: u64::MAX,
                front: 1,
                lower: 0,
                growth: 0,
                check: 0,
                run: 0,
            },
            SnapshotPlan {
                source: 41,
                front: 0,
                lower: 0,
                growth: 0,
                check: 0,
                run: 0,
            },
        ] {
            assert!(
                LoadingAdmission::reserve(&budget, limits, 0)
                    .unwrap()
                    .finish(plan)
                    .is_err()
            );
            assert_eq!(budget.reserved(), 0);
        }
        let mut sources = hirsel_span::SourceMap::default();
        let file = sources.add("failed.hir", "bad source".into()).unwrap();
        let error = LoadingAdmission::reserve(&budget, limits, 0)
            .unwrap()
            .failed(ToolError::Query {
                error: hirsel_engine::QueryError::NotImplemented,
                phase: None,
                sources,
            });
        assert_eq!(budget.reserved(), 40);
        let ToolError::Query { sources, .. } = error else {
            unreachable!()
        };
        let held = sources.file(file).unwrap().clone();
        drop(sources);
        assert_eq!(budget.reserved(), 40);
        assert_eq!(held.text(), "bad source");
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn partial_graph_admission_rolls_back_and_never_multiplies_snapshot_cap() {
        let limits = RequestLimits {
            runtime_bytes: 40,
            snapshot_bytes: 40,
            transport_bytes: 20,
        };
        let plan = SnapshotPlan {
            source: 10,
            front: 10,
            lower: 10,
            check: 10,
            growth: 0,
            run: 0,
        };
        let budget = ProcessBudget::new(99);
        assert!(GraphAdmission::reserve(&budget, limits, plan).is_err());
        assert_eq!(budget.reserved(), 0);
        let budget = ProcessBudget::new(100);
        let admitted = GraphAdmission::reserve(&budget, limits, plan).unwrap();
        assert_eq!(budget.reserved(), 100);
        assert!(GraphAdmission::reserve(&budget, limits, plan).is_err());
        assert_eq!(budget.reserved(), 100);
        drop(admitted);
        assert_eq!(budget.reserved(), 0);
        for plan in [
            SnapshotPlan { check: 11, ..plan },
            SnapshotPlan {
                source: u64::MAX,
                ..plan
            },
        ] {
            assert!(GraphAdmission::reserve(&budget, limits, plan).is_err());
            assert_eq!(budget.reserved(), 0);
        }
    }
    #[test]
    fn host_admission_rolls_back_partial_grants_before_releasing_idle_metadata() {
        use std::sync::atomic::{AtomicU64, Ordering};

        struct Observed {
            budget: Arc<ProcessBudget>,
            dropped_at: Arc<AtomicU64>,
            _reservation: MemoryReservation,
        }
        impl Drop for Observed {
            fn drop(&mut self) {
                self.dropped_at
                    .store(self.budget.reserved(), Ordering::SeqCst);
            }
        }
        impl GraphProducts for Observed {
            fn select(
                &self,
                _: &CompileOperation,
            ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError> {
                unreachable!("admission must not select idle metadata products")
            }
        }
        let (_, inputs) = hirsel_engine::requests::observe_inputs(
            hirsel_engine::loader_inputs::InputLimits {
                bytes: 0,
                operations: 0,
            },
            1,
            |_| Ok::<_, std::io::Error>(()),
        )
        .unwrap();
        let error = hirsel_engine::store::Domain {
            kind: 0x4600_00fe,
            schema: 1,
        }
        .reference(b"fixture error")
        .unwrap();
        let budget = ProcessBudget::new(100);
        let owner = GraphBudget::shared(&budget);
        let dropped_at = Arc::new(AtomicU64::new(u64::MAX));
        owner
            .metadata
            .lock()
            .unwrap()
            .push(Arc::new(InstalledMetadata {
                path: "idle.hir".into(),
                inputs: inputs.into(),
                products: Arc::new(Observed {
                    budget: budget.clone(),
                    dropped_at: dropped_at.clone(),
                    _reservation: budget.reserve(80).unwrap(),
                }),
                errors: RuntimeErrors {
                    resource: error,
                    cycle: error,
                    cancelled: error,
                    protocol: error,
                    panicked: error,
                },
            }));
        let loading = owner
            .begin(
                RequestLimits {
                    runtime_bytes: 20,
                    snapshot_bytes: 40,
                    transport_bytes: 0,
                },
                false,
                0,
            )
            .unwrap();
        assert_eq!(
            dropped_at.load(Ordering::SeqCst),
            80,
            "the host releases idle owners after the failed loading attempt drops its partial grants"
        );
        assert_eq!(budget.reserved(), 60);
        drop(loading);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn shared_graph_budget_evicts_idle_views_but_refuses_to_overcommit_live_leases() {
        struct Empty {
            source: Arc<MemoryReservation>,
            _producer: MemoryReservation,
        }
        impl GraphProducts for Empty {
            fn check_source_owner(&self) -> Option<Arc<dyn Send + Sync>> {
                Some(self.source.clone())
            }
            fn select(
                &self,
                _: &CompileOperation,
            ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError> {
                let (_, inputs) = hirsel_engine::requests::observe_inputs(
                    hirsel_engine::loader_inputs::InputLimits {
                        bytes: 0,
                        operations: 0,
                    },
                    1,
                    |_| Ok::<_, std::io::Error>(()),
                )
                .unwrap();
                Ok((
                    vec![],
                    CompilerReadiness {
                        read_scope: None,
                        materialize: Arc::new(move |_| {
                            Ok(Arc::new(CompileOutput::new(
                                hirsel_engine::entry_path(&hirsel_engine::default_context()),
                                inputs.clone().into(),
                                Default::default(),
                                CompileResult::Check(CheckOutput::default()),
                            )))
                        }),
                        failure: Arc::new(|_| ToolError::Runtime("fixture runtime error".into())),
                    },
                ))
            }
        }
        let (_, inputs) = hirsel_engine::requests::observe_inputs(
            hirsel_engine::loader_inputs::InputLimits {
                bytes: 0,
                operations: 0,
            },
            1,
            |_| Ok::<_, std::io::Error>(()),
        )
        .unwrap();
        let request = CompileRequest {
            path: "first.hir".into(),
            inputs: inputs.into(),
            operation: CompileOperation::Check {
                lowering: hirsel_engine::LoweringPolicy::Comptime,
            },
        };
        let bytes = hirsel_intern::jobs::COMPILER_STACK_BYTES as u64 + 4096;
        let budget = ProcessBudget::new(bytes + 3);
        let mut first = GraphSelections::new(&budget, true);
        let mut second = GraphSelections::new(&budget, true);
        assert!(Arc::ptr_eq(&first.retention, &second.retention));
        let create = |owner: &GraphBudget, request: &CompileRequest| {
            let admission = owner.admit(
                RequestLimits {
                    runtime_bytes: bytes,
                    snapshot_bytes: 2,
                    transport_bytes: 0,
                },
                SnapshotPlan {
                    source: 1,
                    front: 1,
                    lower: 0,
                    check: 0,
                    growth: 0,
                    run: 0,
                },
            )?;
            let error = hirsel_engine::store::Domain {
                kind: 0x4600_00fe,
                schema: 1,
            }
            .reference(b"fixture error")
            .unwrap();
            let runtime = AdmittedRuntime::new(
                hirsel_intern::jobs::Jobs::ONE,
                admission.runtime,
                RuntimeErrors {
                    resource: error,
                    cycle: error,
                    cancelled: error,
                    protocol: error,
                    panicked: error,
                },
            )
            .map_err(ToolError::Io)?;
            Ok(Arc::new(InstalledGraph {
                path: request.path.clone(),
                inputs: request.inputs.clone(),
                runtime: Arc::new(Mutex::new(runtime)),
                products: Arc::new(Empty {
                    source: Arc::new(admission.source),
                    _producer: admission.front,
                }),
            }))
        };
        let active = first
            .select(&request, |owner, _| create(owner, &request))
            .unwrap();
        let equal = first
            .select(&request, |_, _| {
                panic!("equal immutable inputs must reuse the installed graph")
            })
            .unwrap();
        assert!(Arc::ptr_eq(&active.runtime, &equal.runtime));
        assert!(
            second
                .select(&request, |owner, _| create(owner, &request))
                .is_err()
        );
        assert_eq!(budget.reserved(), bytes + 2);
        let completed = (active.output.materialize)(&[]).unwrap();
        let CompileResult::Check(checked) = &completed.result else {
            unreachable!()
        };
        let detached_sources = checked.sources.clone();
        drop(completed);
        drop(equal);
        drop(active);
        let unrelated = budget
            .reserve(bytes + 1)
            .expect("an unrelated engine must reclaim idle retained graphs");
        assert_eq!(budget.reserved(), bytes + 2);
        drop(unrelated);
        let replacement = second
            .select(&request, |owner, _| create(owner, &request))
            .unwrap();
        assert_eq!(budget.reserved(), bytes + 3);
        drop(replacement);
        drop(first);
        drop(second);
        assert_eq!(
            budget.reserved(),
            1,
            "detached source retains its original grant"
        );
        drop(detached_sources);
        assert_eq!(budget.reserved(), 0);

        // The source-only output survives after the idle runtime is reclaimed.
        // A later request by this same selector must refuse admission instead
        // of waiting for the output its own caller is holding.
        let budget = ProcessBudget::new(bytes + 2);
        let mut selector = GraphSelections::new(&budget, true);
        let active = selector
            .select(&request, |owner, _| create(owner, &request))
            .unwrap();
        let held = (active.output.materialize)(&[]).unwrap();
        drop(active);
        let mut changed = request.clone();
        changed.path = "after-held-output.hir".into();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let selector = &mut selector;
            let changed = &changed;
            let create = &create;
            let worker = scope.spawn(move || {
                let result = selector
                    .select(changed, |owner, may_wait| {
                        let loading = owner.begin(
                            RequestLimits {
                                runtime_bytes: bytes,
                                snapshot_bytes: 2,
                                transport_bytes: 0,
                            },
                            may_wait,
                            0,
                        )?;
                        drop(loading);
                        create(owner, changed)
                    })
                    .map(|_| ());
                tx.send(result).unwrap();
            });
            let result = rx.recv_timeout(std::time::Duration::from_secs(2));
            drop(held);
            if result.is_err() {
                // Release the held source before joining even on the old
                // blocking implementation; this regression cannot hang CI.
                let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
            }
            worker.join().unwrap();
            assert!(
                result
                    .expect("own output caused admission to wait")
                    .is_err()
            );
        });
        drop(selector);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn completed_artifact_export_preserves_bytes_and_incomplete_check_boundary() {
        let checked = CheckOutput::default();
        let artifacts = completed_artifacts(
            &checked,
            Ok(b"canonical TIR".to_vec()),
            Some(NativeArtifacts {
                receipts: b"canonical receipts\n".to_vec(),
                objects: vec![
                    (hirsel_native::Backend::Llvm, Ok(vec![vec![1, 2], vec![3]])),
                    (
                        hirsel_native::Backend::Cranelift,
                        Err("unsupported operation".into()),
                    ),
                ],
            }),
        );
        assert_eq!(artifacts["tir"], b"canonical TIR");
        assert_eq!(artifacts["receipts"], b"canonical receipts\n");
        assert_eq!(artifacts["objects/llvm/0000.o"], [1, 2]);
        assert_eq!(artifacts["objects/llvm/0001.o"], [3]);
        assert_eq!(
            artifacts["objects/cranelift/error"],
            b"unsupported operation"
        );
        let incomplete = CheckOutput {
            completion: CheckCompletion::Incomplete {
                phase: hirsel_engine::QueryKind::Lower,
            },
            ..Default::default()
        };
        let artifacts = completed_artifacts(&incomplete, Ok(b"must not leak".to_vec()), None);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts["diagnostics"], b"incomplete: TIR lowering\n");
    }
}
#[test]
fn native_transport_is_admitted_before_source_work_and_transfers_without_a_second_reservation() {
    let budget = ProcessBudget::new(100);
    let owner = GraphBudget::shared(&budget);
    let limits = RequestLimits {
        runtime_bytes: 10,
        snapshot_bytes: 50,
        transport_bytes: 10,
    };
    let mut loading = owner.begin(limits, false, 20).unwrap();
    let source_transport = loading.take_transport();
    assert_eq!(budget.reserved(), 90);
    assert!(
        budget.reserve(20).is_err(),
        "a late native reservation would exceed the cap"
    );
    let mut graph = loading
        .finish(SnapshotPlan {
            source: 10,
            front: 10,
            lower: 10,
            check: 10,
            growth: 5,
            run: 5,
        })
        .unwrap();
    let native_transport = graph.native_transport.split(20).unwrap();
    assert_eq!(native_transport.bytes(), 20);
    assert_eq!(budget.reserved(), 90);
    drop(graph);
    assert_eq!(budget.reserved(), 30);
    drop(source_transport);
    assert_eq!(budget.reserved(), 20);
    drop(native_transport);
    assert_eq!(budget.reserved(), 0);
    assert!(owner.begin(limits, false, 40).is_err());
    assert_eq!(
        budget.reserved(),
        0,
        "a refused whole admission releases every partial grant"
    );
}

//! Install ordinary immutable source producers, then expose their settled roots.
//! The temporary S0 loader is dropped before the graph becomes visible. No
//! mutable engine, synchronous phase controller or second check cache survives.
use super::{
    CompileInputs, CompileOperation, CompileOutput, CompileRequest, CompileResult,
    CompilerReadiness, RequestLimits, RequestWork,
    host_selector::{
        GraphBudget, GraphProducts, InstalledGraph, InstalledMetadata, LoadingAdmission,
    },
    inspection::{Counters, GraphMeter, InspectionLedger, KINDS, MeterOwner, SourceInspection},
};
use hirsel_engine::{
    CompileConfig, Compiler, FrontBodyQueries, FrontCheckQueries, FrontGrowthQueries,
    FrontLowerQueries, FrontRunQueries, LoadedProgram, LoweringPolicy, QueryError, QueryKind,
    ToolError,
    requests::source_graph::{SourceHost, SourceSelection, SourceShape, registration},
    runtime::*,
    store::{RequestStore, ResultStore},
};
use hirsel_span::SourceMap;
use hirsel_syntax::DefPath;
use std::sync::{Arc, Mutex};

/// The checked-product receipt of the completed Check root: its captured
/// headers, and the one grant its staged construction is admitted from.
struct CheckedReceipt {
    headers: Arc<hirsel_engine::FrontCheckedSignatures>,
    allowance: Mutex<Option<MemoryReservation>>,
    products: std::sync::OnceLock<Arc<hirsel_engine::CheckedProducts>>,
    /// The digest-only view, charged once to what the stages left.
    digest_view: std::sync::OnceLock<Arc<hirsel_engine::CheckedProducts>>,
}
impl CheckedReceipt {
    fn products(
        &self,
        check: &Arc<FrontCheckQueries>,
        product: ProductRef,
        entry: &DefPath,
    ) -> Result<Arc<hirsel_engine::CheckedProducts>, ToolError> {
        if let Some(products) = self.products.get() {
            return Ok(products.clone());
        }
        let refused = |stage: &str, error: hirsel_engine::QueryError| {
            ToolError::Runtime(format!("checked products {stage}: {error:?}"))
        };
        let mut pool = self
            .allowance
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or_else(|| uninstalled())?;
        let mut stage = |bytes: u64, what: &str| {
            pool.split(bytes).ok_or_else(|| {
                ToolError::Runtime(format!("checked product {what} exceeds its admission"))
            })
        };
        // Each stage is sized before it allocates. Scope, input, type capture
        // and construction are released with `built`; the receipt keeps only
        // its output text and read maps charged.
        let scope = check
            .completed_scope_bytes(product.clone())
            .map_err(|error| refused("scope sizing", error))?;
        let scope = Arc::new(
            check
                .completed_scope(product, stage(scope, "scope")?)
                .map_err(|error| refused("scope capture", error))?,
        );
        let input = scope
            .checked_product_input_bytes(&self.headers, entry)
            .map_err(|error| refused("original input sizing", error))?;
        let input = std::rc::Rc::new(
            scope
                .checked_product_input_with_allowance(
                    self.headers.clone(),
                    entry,
                    stage(input, "input")?,
                )
                .map_err(|error| refused("original input capture", error))?,
        );
        let capture = input
            .type_capture_bytes()
            .map_err(|error| refused("type capture sizing", error))?;
        let prepared = input
            .capture_types_with_allowance(stage(capture, "type capture")?)
            .map_err(|error| refused("type capture", error))?;
        // Construction moves the uniquely owned originals and their grants
        // into the export rather than admitting a second payload copy.
        let build = prepared
            .owned_build_bytes()
            .map_err(|error| refused("construction sizing", error))?;
        let built = prepared
            .build_owned_with_allowance(stage(build, "construction")?)
            .map_err(|error| refused("construction", error))?;
        let output = stage(
            built
                .output_bytes()
                .map_err(|error| refused("output sizing", error))?,
            "output",
        )?;
        let receipt = stage(
            built
                .checked_products_bytes()
                .map_err(|error| refused("receipt sizing", error))?,
            "receipt",
        )?;
        let products = built
            .checked_products_with_allowances(output, receipt)
            .map_err(|error| refused("receipt publication", error))?;
        drop(built);
        *self
            .allowance
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(pool);
        Ok(self.products.get_or_init(|| products).clone())
    }
    fn digest_view(
        &self,
        check: &Arc<FrontCheckQueries>,
        product: ProductRef,
        entry: &DefPath,
    ) -> Result<Arc<hirsel_engine::CheckedProducts>, ToolError> {
        if let Some(view) = self.digest_view.get() {
            return Ok(view.clone());
        }
        let products = self.products(check, product, entry)?;
        let view = self
            .allowance
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
            .and_then(|pool| products.digest_view(pool))
            .ok_or_else(|| {
                ToolError::Runtime("checked product digest view exceeds its admission".into())
            })?;
        Ok(self.digest_view.get_or_init(|| view).clone())
    }
}

struct Expansion {
    text: Arc<str>,
    _allowance: MemoryReservation,
}

#[derive(Default)]
pub(super) struct SourceIndexSlot {
    generation: Arc<()>,
    state: Option<hirsel_engine::SourceIndexState>,
}
fn complete_source_index(
    slot: &Mutex<SourceIndexSlot>,
    generation: &Arc<()>,
    snapshot: &Arc<hirsel_engine::SignatureSnapshot>,
) {
    let state = {
        let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        if !Arc::ptr_eq(&slot.generation, generation) {
            return;
        }
        slot.state.take()
    };
    if let Some(state) = state {
        let state = state.complete_headers(snapshot).ok();
        let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        if Arc::ptr_eq(&slot.generation, generation) && slot.state.is_none() {
            slot.state = state;
        }
    }
}
struct SourceProducts {
    source_index: Arc<Mutex<SourceIndexSlot>>,
    source_generation: Arc<()>,
    declaration_export: bool,
    hash_selection: Option<(Option<hirsel_syntax::PackageId>, DefPath)>,
    native_sources: Option<(Vec<DefPath>, Arc<hirsel_engine::FrontNativeQueries>)>,
    expansion: Option<Arc<Expansion>>,
    entry: DefPath,
    inputs: CompileInputs,
    lowering: LoweringPolicy,
    launch_entry: Option<DefPath>,
    run: Option<Arc<FrontRunQueries>>,
    native: Option<Arc<super::native_selection::NativeSelection>>,
    native_artifacts: Option<Arc<super::native_artifacts::NativeArtifacts>>,
    ownership: Option<Arc<super::ownership_selection::OwnershipSelection>>,
    safe: Option<Arc<super::safe_check::SafeCheck>>,
    receipt: Option<Arc<CheckedReceipt>>,
    export: Option<Arc<super::proof_export::ProofExport>>,
    front: Arc<FrontBodyQueries>,
    lower: Arc<FrontLowerQueries>,
    check: Arc<FrontCheckQueries>,
    _growth: Arc<FrontGrowthQueries>,
    storage: Option<(
        Arc<hirsel_engine::FrontStorageQueries>,
        Arc<hirsel_engine::FrontStorageGraphQueries>,
    )>,
    sources: SourceMap,
    store: Arc<RequestStore>,
    initial_operation: Mutex<Option<hirsel_engine::store::StoreOperation>>,
    completed_operation: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    errors: RuntimeErrors,
    meter: Arc<GraphMeter>,
    metadata: Arc<InstalledMetadata>,
}

struct SourceMetadata {
    entry: DefPath,
    inputs: CompileInputs,
    meter: Arc<GraphMeter>,
    // Runtime diagnostic references remain owned by their admitted transport.
    _store: Arc<RequestStore>,
}
impl GraphProducts for SourceMetadata {
    fn accepts(&self, operation: &CompileOperation) -> bool {
        matches!(
            operation,
            CompileOperation::Inspect { .. } | CompileOperation::RecomputeCounts { .. }
        )
    }
    fn select(
        &self,
        operation: &CompileOperation,
    ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError> {
        if !self.accepts(operation) {
            return Err(uninstalled());
        }
        let operation = operation.clone();
        let meter = self.meter.clone();
        let entry = self.entry.clone();
        let inputs = self.inputs.clone();
        Ok((
            vec![],
            CompilerReadiness {
                read_scope: None,
                materialize: Arc::new(move |products| {
                    if !products.is_empty() {
                        return Err(uninstalled());
                    }
                    let work = meter.flush()?;
                    let result = match &operation {
                        CompileOperation::Inspect { definitions } => {
                            CompileResult::Inspect(meter.inspect(definitions)?)
                        }
                        CompileOperation::RecomputeCounts { queries } => {
                            CompileResult::RecomputeCounts(meter.counts(queries)?)
                        }
                        _ => return Err(uninstalled()),
                    };
                    Ok(Arc::new(CompileOutput::new(
                        entry.clone(),
                        inputs.clone(),
                        work,
                        result,
                    )))
                }),
                failure: Arc::new(|_| uninstalled()),
            },
        ))
    }
}
impl GraphProducts for SourceProducts {
    fn metadata(&self) -> Option<Arc<InstalledMetadata>> {
        Some(self.metadata.clone())
    }
    fn check_source_owner(&self) -> Option<Arc<dyn Send + Sync>> {
        // CheckOutput owns diagnostics and contains no producer handles.
        // Its source map keeps the original source/header grant, allowing the
        // idle front, Lower and Check producers to release their own grants.
        Some(Arc::new((self.meter.source_owner(), self.store.clone())))
    }
    fn accepts(&self, operation: &CompileOperation) -> bool {
        if self
            .completed_operation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|completed| !completed.load(std::sync::atomic::Ordering::Acquire))
        {
            return false;
        }
        if self.declaration_export {
            return matches!(operation, CompileOperation::DataAbi) && self.native_sources.is_some();
        }
        match operation {
            CompileOperation::Check { lowering } => *lowering == self.lowering,
            CompileOperation::Precompute => self
                .run
                .as_ref()
                .is_some_and(|run| run.precompute_query().is_some()),
            CompileOperation::Expand => self.expansion.is_some(),
            CompileOperation::Hashes { package } => self
                .hash_selection
                .as_ref()
                .is_some_and(|(selected, _)| package == selected),
            CompileOperation::DataAbi => self.declaration_export && self.native_sources.is_some(),
            CompileOperation::NativeSources { roots } => {
                !self.declaration_export
                    && self
                        .native_sources
                        .as_ref()
                        .is_some_and(|(selected, _)| selected == roots)
            }
            CompileOperation::CheckedProducts { lowering, .. } => {
                *lowering == self.lowering && self.receipt.is_some()
            }
            CompileOperation::Inspect { .. } | CompileOperation::RecomputeCounts { .. } => true,
            CompileOperation::Tir { .. } => self
                .run
                .as_ref()
                .is_some_and(|run| run.precompute_query().is_none() && run.batch_query().is_none()),
            CompileOperation::TirBatch { definitions } => self
                .run
                .as_ref()
                .is_some_and(|run| run.batch_roots() == Some(definitions.as_slice())),
            CompileOperation::SafeCheck => self.safe.is_some() && self.export.is_none(),
            CompileOperation::Artifacts { native: true } => {
                self.native_artifacts.is_some()
                    && self.export.as_ref().is_some_and(|export| export.reusable())
            }
            CompileOperation::Artifacts { native: false } | CompileOperation::ExportTir => {
                self.export.as_ref().is_some_and(|export| export.reusable())
                    && self.native_artifacts.is_none()
            }
            CompileOperation::NativeOwnership(request) => self
                .ownership
                .as_ref()
                .is_some_and(|selected| selected.accepts(request)),
            CompileOperation::NativeBuild(request) => self.native.as_ref().is_some_and(|native| {
                native.accepts(request, super::native_platform::NativePurpose::Build)
            }),
            CompileOperation::NativeRun(request) => self.native.as_ref().is_some_and(|native| {
                native.accepts(request, super::native_platform::NativePurpose::Run)
            }),
            _ => false,
        }
    }
    fn select(
        &self,
        operation: &CompileOperation,
    ) -> Result<(Vec<QueryId>, CompilerReadiness), ToolError> {
        if matches!(
            operation,
            CompileOperation::Inspect { .. } | CompileOperation::RecomputeCounts { .. }
        ) {
            return self.metadata.select(operation);
        }
        let store_operation = self
            .initial_operation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .unwrap_or_else(|| self.store.operation_scope());
        let mut store_operation = if matches!(
            operation,
            CompileOperation::Artifacts { .. }
                | CompileOperation::ExportTir
                | CompileOperation::Precompute
        ) {
            store_operation.completing()
        } else {
            store_operation
        };
        if let Some(native) = &self.native {
            store_operation.join(native.store_operation());
        }
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *self
            .completed_operation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            store_operation.has_publication().then(|| completed.clone());
        let store_operation = super::output::CompileStoreOperation {
            scope: store_operation,
            completed,
        };
        let roots = match operation {
            CompileOperation::NativeOwnership(_) => {
                vec![
                    self.ownership
                        .as_ref()
                        .ok_or_else(|| uninstalled())?
                        .query(),
                ]
            }
            CompileOperation::NativeBuild(_) | CompileOperation::NativeRun(_) => {
                vec![self.native.as_ref().ok_or_else(|| uninstalled())?.query()]
            }
            CompileOperation::SafeCheck => {
                vec![self.safe.as_ref().ok_or_else(|| uninstalled())?.query()]
            }
            CompileOperation::Artifacts { native: true } => {
                vec![
                    self.native_artifacts
                        .as_ref()
                        .ok_or_else(|| uninstalled())?
                        .query(),
                ]
            }
            CompileOperation::Artifacts { native: false } | CompileOperation::ExportTir => {
                vec![self.export.as_ref().ok_or_else(|| uninstalled())?.query()]
            }
            CompileOperation::Check { lowering }
            | CompileOperation::CheckedProducts { lowering, .. }
                if *lowering == self.lowering =>
            {
                vec![self.check.query()]
            }
            CompileOperation::Precompute => vec![
                self.run
                    .as_ref()
                    .and_then(|run| run.precompute_query())
                    .ok_or_else(|| uninstalled())?,
            ],
            CompileOperation::Expand => vec![self.check.query()],
            CompileOperation::Hashes { .. } => vec![
                self.run
                    .as_ref()
                    .and_then(|run| run.query(&self.hash_selection.as_ref()?.1))
                    .ok_or_else(|| uninstalled())?,
            ],
            CompileOperation::NativeSources { .. } | CompileOperation::DataAbi => vec![
                self.native_sources
                    .as_ref()
                    .ok_or_else(|| uninstalled())?
                    .1
                    .query(),
            ],
            CompileOperation::TirBatch { .. } => vec![
                self.run
                    .as_ref()
                    .and_then(|run| run.batch_query())
                    .ok_or_else(|| uninstalled())?,
            ],
            CompileOperation::Tir { definition } => {
                let entry = definition
                    .as_ref()
                    .or(self.launch_entry.as_ref())
                    .ok_or_else(|| ToolError::Runtime("launch manifest requires entry".into()))?;
                vec![
                    self.run
                        .as_ref()
                        .and_then(|run| run.query(entry))
                        .ok_or_else(|| {
                            ToolError::Runtime(format!(
                                "launch: the program declares no entry `pub fn {}`",
                                entry.0.name
                            ))
                        })?,
                ]
            }
            _ => return Err(uninstalled()),
        };
        let selected = roots.first().copied();
        let operation = operation.clone();
        let run = self.run.clone();
        let native = self.native.clone();
        let native_artifacts = self.native_artifacts.clone();
        let ownership = self.ownership.clone();
        let safe = self.safe.clone();
        let export = self.export.clone();
        let lower = self.lower.clone();
        let check = self.check.clone();
        let receipt = self.receipt.clone();
        let expansion = self.expansion.clone();
        let native_sources = self.native_sources.clone();
        let expected = check.query();
        let entry = match &operation {
            CompileOperation::NativeOwnership(request) => request
                .definition
                .as_ref()
                .or(self.launch_entry.as_ref())
                .cloned()
                .ok_or_else(|| uninstalled())?,
            CompileOperation::NativeBuild(request) | CompileOperation::NativeRun(request) => {
                request
                    .definition
                    .as_ref()
                    .or(self.launch_entry.as_ref())
                    .cloned()
                    .ok_or_else(|| uninstalled())?
            }
            CompileOperation::Tir { definition } => definition
                .as_ref()
                .or(self.launch_entry.as_ref())
                .cloned()
                .ok_or_else(|| ToolError::Runtime("launch manifest requires entry".into()))?,
            _ => self.entry.clone(),
        };
        let inputs = self.inputs.clone();
        let meter = self.meter.clone();
        let store = self.store.clone();
        let failure = self.failure_reader();
        let source_index = self.source_index.clone();
        let source_generation = self.source_generation.clone();
        let source_headers = self.front.signatures();
        Ok((
            roots,
            CompilerReadiness {
                read_scope: Some(store_operation),
                materialize: Arc::new(move |products| {
                    if store.exhausted() {
                        return Err(ToolError::Runtime(
                            "compiler product transport allowance exhausted".into(),
                        ));
                    }
                    let work = meter.flush()?;
                    complete_source_index(&source_index, &source_generation, &source_headers);
                    let result = match &operation {
                        CompileOperation::NativeOwnership(_) => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            if selected.is_none_or(|query| query.address != product.address) {
                                return Err(uninstalled());
                            }
                            CompileResult::Ownership(
                                ownership
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .materialize(product.clone())?,
                            )
                        }
                        CompileOperation::NativeBuild(_) | CompileOperation::NativeRun(_) => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            if selected.is_none_or(|query| query.address != product.address) {
                                return Err(uninstalled());
                            }
                            if let CompileOperation::NativeBuild(request) = &operation
                                && matches!(request.output, super::NativeOutput::Ready)
                            {
                                match native
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .readiness
                                    .ready(product.clone())
                                {
                                    Ok(view) => CompileResult::NativeReady(view),
                                    Err(error) => CompileResult::NativeError(error),
                                }
                            } else if let CompileOperation::NativeBuild(request) = &operation
                                && matches!(request.output, super::NativeOutput::Objects { .. })
                            {
                                match native
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .readiness
                                    .objects(product.clone())
                                {
                                    Ok(objects) => CompileResult::NativeObjects(objects),
                                    Err(error) => CompileResult::NativeError(error),
                                }
                            } else {
                                match native
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .readiness
                                    .materialize(product.clone())
                                {
                                    Ok(execution) => CompileResult::Native(execution),
                                    Err(error) => CompileResult::NativeError(error),
                                }
                            }
                        }
                        CompileOperation::SafeCheck => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            if selected.is_none_or(|selected| selected.address != product.address) {
                                return Err(uninstalled());
                            }
                            let (checked, reach) = safe
                                .as_ref()
                                .ok_or_else(|| uninstalled())?
                                .outcome(product.clone())?;
                            CompileResult::SafeCheck { checked, reach }
                        }
                        CompileOperation::Artifacts { native: true } => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            CompileResult::Artifacts(
                                native_artifacts
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .outcome(product.clone())?,
                            )
                        }
                        CompileOperation::Artifacts { native: false }
                        | CompileOperation::ExportTir => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            if selected.is_none_or(|selected| selected.address != product.address) {
                                return Err(uninstalled());
                            }
                            let (checked, tir) = export
                                .as_ref()
                                .ok_or_else(|| uninstalled())?
                                .outcome(product.clone())?;
                            if matches!(operation, CompileOperation::ExportTir) {
                                if checked.diagnostics.iter().any(|diagnostic| {
                                    diagnostic.severity == hirsel_diag::Severity::Error
                                }) {
                                    return Err(ToolError::Query {
                                        error: QueryError::Diagnostics(checked.diagnostics),
                                        phase: None,
                                        sources: checked.sources,
                                    });
                                }
                                if let hirsel_engine::CheckCompletion::Incomplete { phase } =
                                    checked.completion
                                {
                                    return Err(ToolError::Query {
                                        error: QueryError::NotImplemented,
                                        phase: Some(phase),
                                        sources: checked.sources,
                                    });
                                }
                                CompileResult::ExportTir(
                                    String::from_utf8(tir.map_err(ToolError::Runtime)?)
                                        .map_err(|error| ToolError::Runtime(error.to_string()))?,
                                )
                            } else {
                                CompileResult::Artifacts(super::host_selector::completed_artifacts(
                                    &checked, tir, None,
                                ))
                            }
                        }
                        CompileOperation::Precompute => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            let output = run
                                .as_ref()
                                .and_then(|run| run.precompute_outcome(product.clone()))
                                .ok_or_else(|| uninstalled())?;
                            CompileResult::Precomputed {
                                definitions: output.definitions,
                                digest: output.digest,
                                sources: Arc::new(super::PrecomputedSources {
                                    run: run.as_ref().ok_or_else(|| uninstalled())?.clone(),
                                    lower: lower.clone(),
                                }),
                            }
                        }
                        CompileOperation::Hashes { .. } => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            CompileResult::Hashes(
                                run.as_ref()
                                    .and_then(|run| run.outcome(product.clone()))
                                    .and_then(|output| output.definition_hashes.clone())
                                    .ok_or_else(|| uninstalled())?,
                            )
                        }
                        CompileOperation::NativeSources { .. } | CompileOperation::DataAbi => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            CompileResult::NativeSources(
                                native_sources
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .1
                                    .outcome(product.clone())
                                    .ok_or_else(|| uninstalled())?
                                    .products,
                            )
                        }
                        CompileOperation::Expand => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            let completed = check
                                .outcome(product.clone())
                                .ok_or_else(|| uninstalled())?;
                            if completed.checked.completion
                                != hirsel_engine::CheckCompletion::Complete
                            {
                                return Err(ToolError::Query {
                                    error: QueryError::NotImplemented,
                                    phase: None,
                                    sources: completed.checked.sources,
                                });
                            }
                            if completed.checked.diagnostics.iter().any(|diagnostic| {
                                diagnostic.severity == hirsel_diag::Severity::Error
                            }) {
                                return Err(ToolError::Query {
                                    error: QueryError::Diagnostics(completed.checked.diagnostics),
                                    phase: None,
                                    sources: completed.checked.sources,
                                });
                            }
                            CompileResult::Expansion(
                                expansion
                                    .as_ref()
                                    .ok_or_else(|| uninstalled())?
                                    .text
                                    .clone(),
                            )
                        }
                        CompileOperation::Check { .. } => {
                            let [product] = products else {
                                return Err(ToolError::Runtime(
                                    "source check readiness requires exactly one root".into(),
                                ));
                            };
                            if product.address != expected.address {
                                return Err(ToolError::Runtime(
                                    "source check readiness received another root".into(),
                                ));
                            }
                            let completed = check.outcome(product.clone()).ok_or_else(|| {
                                ToolError::Runtime("selected check product is unavailable".into())
                            })?;
                            CompileResult::Check(completed.checked)
                        }
                        CompileOperation::CheckedProducts { canonical, .. } => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            if product.address != expected.address {
                                return Err(uninstalled());
                            }
                            let completed = check.outcome(product.clone()).ok_or_else(|| {
                                ToolError::Runtime("selected check product is unavailable".into())
                            })?;
                            let receipt = receipt.as_ref().ok_or_else(|| uninstalled())?;
                            // The digest commits to the full text either way.
                            let products = if *canonical {
                                receipt.products(&check, product.clone(), &entry)?
                            } else {
                                receipt.digest_view(&check, product.clone(), &entry)?
                            };
                            CompileResult::CheckedProducts {
                                checked: completed.checked,
                                products,
                            }
                        }
                        CompileOperation::TirBatch { .. } => {
                            let [product] = products else {
                                return Err(uninstalled());
                            };
                            if selected.is_none_or(|query| query.address != product.address) {
                                return Err(uninstalled());
                            }
                            let entries = run
                                .as_ref()
                                .and_then(|run| run.batch_outcome(product.clone()))
                                .ok_or_else(|| uninstalled())?;
                            CompileResult::TirBatch(Arc::new(super::TirBatchExecution {
                                entries,
                                lower: lower.clone(),
                                owner: Arc::new(()),
                            }))
                        }
                        CompileOperation::Tir { .. } => {
                            let [product] = products else {
                                return Err(ToolError::Runtime(
                                    "TIR readiness requires exactly one root".into(),
                                ));
                            };
                            if selected.is_none_or(|selected| selected.address != product.address) {
                                return Err(ToolError::Runtime(
                                    "TIR readiness received another root".into(),
                                ));
                            }
                            let facts = run
                                .as_ref()
                                .and_then(|run| run.outcome(product.clone()))
                                .ok_or_else(|| {
                                ToolError::Runtime("selected TIR run facts are unavailable".into())
                            })?;
                            CompileResult::Tir(super::tir_products::materialize(
                                facts,
                                lower.clone(),
                            )?)
                        }
                        _ => return Err(uninstalled()),
                    };
                    Ok(Arc::new(
                        CompileOutput::new(entry.clone(), inputs.clone(), work, result)
                            .with_check_metadata(check.clone()),
                    ))
                }),
                failure,
            },
        ))
    }
}
impl SourceProducts {
    fn failure_reader(
        &self,
    ) -> Arc<dyn Fn(hirsel_engine::store::Reference) -> ToolError + Send + Sync> {
        let store = self.store.clone();
        let check = self.check.clone();
        let lower = self.lower.clone();
        let front = self.front.clone();
        let sources = self.sources.clone();
        let errors = self.errors;
        let growth = self._growth.clone();
        let storage = self.storage.clone();
        let run = self.run.clone();
        let native = self.native.clone();
        let native_artifacts = self.native_artifacts.clone();
        let ownership = self.ownership.clone();
        let native_sources = self.native_sources.clone();
        Arc::new(move |reference| {
            if store.exhausted() {
                return ToolError::Runtime(format!(
                    "compiler product transport allowance exhausted; {store:?}",
                ));
            }
            if let Some(error) = ownership
                .as_ref()
                .and_then(|selected| selected.failure(reference))
            {
                return error;
            }
            if let Some(error) = native
                .as_ref()
                .and_then(|native| native.assembly.failure(reference))
            {
                return error;
            }
            if let Some(error) = native_artifacts
                .as_ref()
                .and_then(|artifacts| artifacts.failure(reference))
            {
                return error;
            }
            let source_failure = native_sources
                .as_ref()
                .and_then(|(_, source)| source.failure(reference));
            let selected = source_failure.map(|error| (error, None)).or_else(|| {
                check
                    .failure(reference)
                    .or_else(|| {
                        lower
                            .failure(reference)
                            .map(|error| (error, Some(QueryKind::Lower)))
                    })
                    .or_else(|| {
                        let outcome = front.outcome(Err(reference))?.outcome;
                        outcome.result.err().map(|error| (error, outcome.phase))
                    })
            });
            let selected = selected.or_else(|| {
                storage
                    .as_ref()
                    .and_then(|(_, graph)| graph.failure(reference))
                    .map(|error| (error, Some(QueryKind::Check)))
            });
            let selected = selected.or_else(|| {
                growth
                    .failure(reference)
                    .map(|error| (error, Some(QueryKind::Check)))
            });
            let selected = selected.or_else(|| {
                run.as_ref()
                    .and_then(|run| run.failure(reference))
                    .map(|error| (error, None))
            });
            if let Some((error, phase)) = selected {
                return ToolError::Query {
                    error,
                    phase,
                    sources: sources.clone(),
                };
            }
            let message = if reference == errors.resource {
                return ToolError::Runtime(format!(
                    "compiler query allowance exhausted; Front retained {}/{}B, Lower retained {}/{}B; Front refusal {:?}",
                    front.retained_bytes(),
                    front.admitted_bytes(),
                    lower.retained_bytes(),
                    lower.admitted_bytes(),
                    front.memory_refusal(),
                ));
            } else if reference == errors.cycle {
                "compiler query dependency cycle"
            } else if reference == errors.cancelled {
                "compiler query cancelled"
            } else if reference == errors.panicked {
                "compiler query producer panicked"
            } else {
                "compiler query protocol failure"
            };
            ToolError::Runtime(message.into())
        })
    }
}
#[track_caller]
fn uninstalled() -> ToolError {
    let site = std::panic::Location::caller();
    ToolError::Runtime(format!(
        "selected compiler operation has no installed producer at {}:{}",
        site.file(),
        site.line()
    ))
}

/// One selector's carries across its source revisions' fresh graphs.
pub(super) use hirsel_engine::requests::source_graph::SourceCarry as Carry;

/// The request host's part of the shared installer: its graph budget, its
/// inspection rows and its native or ownership root footprints.
struct RequestHost<'a> {
    budget: &'a GraphBudget,
    native: bool,
    proofs: bool,
}
impl SourceHost for RequestHost<'_> {
    fn readmit(
        &self,
        loading: &mut LoadingAdmission,
        runtime: u64,
        snapshot: u64,
    ) -> Result<(), ToolError> {
        self.budget.readmit(loading, runtime, snapshot)
    }
    fn inspection_bytes(&self, definitions: &[DefPath], memos: usize) -> Option<u64> {
        GraphMeter::input_bytes(definitions, memos)
    }
    fn extension_minimum(&self, definitions: &[DefPath], catalogue: u64) -> Option<u64> {
        if self.native {
            super::native_selection::Footprint::minimum(definitions)
        } else {
            super::ownership_admission::Footprint::new(definitions, 0)?
                .with_catalogue_bytes(definitions, catalogue)?
                .bytes()
        }
    }
    fn extension_bytes(
        &self,
        definitions: &[DefPath],
        slack: u64,
        catalogue: u64,
    ) -> Option<(u64, u64)> {
        if self.native {
            let native = super::native_selection::Footprint::new(definitions, slack, self.proofs)?;
            Some((native.bytes()?, native.source()))
        } else {
            let ownership = super::ownership_admission::Footprint::new(definitions, slack)?
                .with_catalogue_bytes(definitions, catalogue)?;
            Some((ownership.bytes()?, 0))
        }
    }
}

pub(super) fn install(
    compiler: &Compiler,
    config: &CompileConfig,
    limits: RequestLimits,
    budget: &GraphBudget,
    may_wait: bool,
    request: &CompileRequest,
    ledger: Arc<InspectionLedger>,
    carry: Option<&Carry>,
    source_index: &Arc<Mutex<SourceIndexSlot>>,
    scratch_parent: &std::path::Path,
    toolchain: Option<&Arc<crate::native::Toolchain>>,
    host_toolchain: bool,
    #[cfg(test)] before_check: Option<fn(&mut LoadedProgram)>,
) -> Result<Arc<InstalledGraph>, ToolError> {
    if matches!(request.operation, CompileOperation::Precompute)
        && config.verify == hirsel_engine::Verify::Off
    {
        return Err(ToolError::Runtime(
            "package precomputation requires V".into(),
        ));
    }
    let (lowering, needs_run) = match &request.operation {
        CompileOperation::Check { lowering }
        | CompileOperation::CheckedProducts { lowering, .. } => (*lowering, false),
        CompileOperation::Precompute
        | CompileOperation::Tir { .. }
        | CompileOperation::TirBatch { .. }
        | CompileOperation::Hashes { .. }
        | CompileOperation::NativeOwnership(_)
        | CompileOperation::NativeSources { .. }
        | CompileOperation::DataAbi => (LoweringPolicy::Reachable, true),
        CompileOperation::SafeCheck => (config.lowering, true),
        CompileOperation::Expand => (LoweringPolicy::Comptime, false),
        // The oracle export reads the check and TIR of every reachable body.
        CompileOperation::Artifacts { .. } | CompileOperation::ExportTir => {
            (LoweringPolicy::Reachable, true)
        }
        CompileOperation::NativeBuild(_) | CompileOperation::NativeRun(_) => {
            (LoweringPolicy::Reachable, true)
        }
        _ => return Err(uninstalled()),
    };
    let artifacts = matches!(
        request.operation,
        CompileOperation::Artifacts { .. } | CompileOperation::ExportTir
    );
    let completes = artifacts || matches!(request.operation, CompileOperation::Precompute);
    // The loader reads exactly the supplied immutable observations. Declaration
    // indexing/signature metadata is S0; no P body or Lower task runs here.
    // Reserve both transports while admission may wait without holding a graph.
    // Native selection transfers this grant instead of reserving again after S0.
    let native_transport_bytes = if host_toolchain
        && matches!(
            request.operation,
            CompileOperation::NativeBuild(_) | CompileOperation::NativeRun(_)
        ) {
        limits.transport_bytes
    } else {
        0
    };
    let mut loading = budget.begin(limits, may_wait, native_transport_bytes)?;
    let disk_bytes = limits
        .runtime_bytes
        .checked_add(limits.snapshot_bytes)
        .and_then(|bytes| bytes.checked_add(limits.transport_bytes))
        .ok_or_else(|| uninstalled())?;
    let store = match RequestStore::with_scratch_and_verifier_transport(
        config.store.clone(),
        loading.take_transport(),
        scratch_parent,
        disk_bytes,
    ) {
        Ok(store) => Arc::new(store),
        Err(error) => return Err(loading.failed(ToolError::Io(error))),
    };
    // Precompute and export roots promise their complete closure.
    let store_operation = store.operation_scope();
    let store_operation = if completes {
        store_operation.completing()
    } else {
        store_operation
    };
    // Host requests select tools against this graph's admitted provider before
    // loading source inputs. Explicit selectors never acquire ambient tools.
    let selected_toolchain = match (&request.operation, toolchain, host_toolchain) {
        (
            CompileOperation::NativeBuild(native) | CompileOperation::NativeRun(native),
            None,
            true,
        ) => match crate::native::Toolchain::for_host_request(
            &native.options.passes.target,
            store.clone(),
        ) {
            Ok(tools) => Some(Arc::new(tools)),
            Err(error) => return Err(loading.failed(ToolError::Runtime(error.to_string()))),
        },
        _ => toolchain.cloned(),
    };
    let mut selected_config = config.clone();
    selected_config.store = store.clone();
    let selected_compiler = compiler.clone().with_config(selected_config);
    let (source_generation, prior_index) = {
        let mut slot = source_index.lock().unwrap_or_else(|e| e.into_inner());
        slot.generation = Arc::new(());
        (slot.generation.clone(), slot.state.take())
    };
    let load_started = std::time::Instant::now();
    let loaded = if matches!(request.operation, CompileOperation::DataAbi) {
        selected_compiler.load_export_package_inputs(&request.path, &request.inputs)
    } else {
        match prior_index {
            Some(index) => selected_compiler.load_check_package_inputs_with_source_index(
                &request.path,
                &request.inputs,
                index,
            ),
            None => {
                selected_compiler.load_check_package_inputs(&request.path, &request.inputs, true)
            }
        }
    };
    let load_nanos = load_started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    let mut program = match loaded {
        Ok(program) => program,
        Err(error) => return Err(loading.failed(error)),
    };
    #[cfg(test)]
    if let Some(before_check) = before_check {
        before_check(&mut program);
    }
    if let CompileOperation::NativeBuild(native) | CompileOperation::NativeRun(native) =
        &request.operation
        && let Err(error) = super::native_sources::install(
            &mut program,
            &native.supplemental_sources,
            limits.snapshot_bytes,
        )
    {
        return Err(loading.failed(error));
    }
    let launch_entry = program.launch_entry().cloned();
    let needs_native = matches!(
        request.operation,
        CompileOperation::NativeBuild(_)
            | CompileOperation::NativeRun(_)
            | CompileOperation::Artifacts { native: true }
    );
    let needs_proofs = matches!(&request.operation, CompileOperation::NativeBuild(native) | CompileOperation::NativeRun(native) if native.evidence.is_some());
    let needs_ownership = matches!(
        request.operation,
        CompileOperation::NativeOwnership(_)
            | CompileOperation::NativeSources { .. }
            | CompileOperation::DataAbi
    );
    let shape = SourceShape {
        run: needs_run,
        native: needs_native,
        proofs: needs_proofs,
        ownership: needs_ownership,
        safe: matches!(
            request.operation,
            CompileOperation::SafeCheck
                | CompileOperation::Artifacts { .. }
                | CompileOperation::ExportTir
        ),
        receipt: matches!(request.operation, CompileOperation::CheckedProducts { .. }),
        export: artifacts,
        storage: !matches!(request.operation, CompileOperation::Expand),
        batch: matches!(request.operation, CompileOperation::TirBatch { .. }),
        compact_reads: matches!(
            request.operation,
            CompileOperation::NativeOwnership(_)
                | CompileOperation::NativeBuild(_)
                | CompileOperation::NativeRun(_)
        ),
    };
    let root_package = program.entry.0.package.clone();
    let selection = SourceSelection {
        files: program
            .source_files()
            .map(|(file, context)| {
                (
                    *file,
                    context.clone(),
                    if context.package == root_package {
                        lowering
                    } else {
                        LoweringPolicy::Comptime
                    },
                )
            })
            .collect(),
        entry: program.entry.clone(),
        lint_package: Some(root_package.clone()),
        unsafe_ffi: program.unsafe_ffi.clone(),
        input_bytes: u64::try_from(request.inputs.retained_bytes()).map_err(|_| uninstalled())?,
    };
    let host = RequestHost {
        budget,
        native: needs_native,
        proofs: needs_proofs,
    };
    let mut installed = hirsel_engine::requests::source_graph::install(
        &mut program.engine,
        &selection,
        shape,
        config.jobs,
        limits,
        &host,
        loading,
        store.as_ref(),
        carry,
    )?;
    let definitions = std::mem::take(&mut installed.definitions);
    let registrations = installed.registrations.clone();
    let front = installed.front.clone();
    let lower = installed.lower.clone();
    let growth = installed.growth.clone();
    let storage = installed.storage.clone();
    let errors = installed.errors;
    let sources = installed.sources.clone();
    let source = installed.source.clone();
    let mut run = installed
        .run
        .split(installed.run.bytes())
        .ok_or_else(|| uninstalled())?;
    let native_transport = installed
        .native_transport
        .split(installed.native_transport.bytes())
        .ok_or_else(|| uninstalled())?;
    let source_index_allowance = installed
        .source_index
        .split(installed.source_index.bytes())
        .ok_or_else(|| uninstalled())?;
    let mut run_inputs = installed.run_inputs.take();
    let mut native_catalogue = installed.catalogue.take();
    let query_error = |error| ToolError::Query {
        error,
        phase: None,
        sources: sources.clone().retain_owner(source.clone()),
    };
    // The host's native or ownership footprint, sized from the same slack
    // and final inventory the shared plan charged.
    let catalogue_bytes = native_catalogue
        .as_ref()
        .map_or(0, |inputs| inputs.input_bytes());
    let native_footprint = if needs_native {
        Some(
            super::native_selection::Footprint::new(
                &definitions,
                installed.plan.extension_slack,
                needs_proofs,
            )
            .ok_or_else(|| uninstalled())?,
        )
    } else {
        None
    };
    let ownership_footprint = if needs_ownership {
        Some(
            super::ownership_admission::Footprint::new(
                &definitions,
                installed.plan.extension_slack,
            )
            .and_then(|footprint| footprint.with_catalogue_bytes(&definitions, catalogue_bytes))
            .ok_or_else(|| uninstalled())?,
        )
    } else {
        None
    };
    // The receipt's headers share the Check partition; their constructor
    // refuses metadata beyond this split.
    let receipt = if matches!(request.operation, CompileOperation::CheckedProducts { .. }) {
        let headers = program
            .engine
            .front_checked_signatures_bytes(&front)
            .map_err(&query_error)?;
        let headers = installed
            .check
            .split(headers)
            .ok_or_else(|| uninstalled())?;
        let headers = program
            .engine
            .front_checked_signatures(front.clone(), headers)
            .map_err(&query_error)?;
        // The receipt has no Run, so its stages draw from Run's partition.
        let allowance = run.split(run.bytes()).ok_or_else(|| uninstalled())?;
        Some(Arc::new(CheckedReceipt {
            headers,
            allowance: Mutex::new(Some(allowance)),
            products: std::sync::OnceLock::new(),
            digest_view: std::sync::OnceLock::new(),
        }))
    } else {
        None
    };
    let expansion = if matches!(request.operation, CompileOperation::Expand) {
        let mut generated = program
            .engine
            .generated_sources()
            .filter(|(_, generated)| {
                program.engine.sources.resolve(generated.site).file == program.primary
            })
            .collect::<Vec<_>>();
        generated
            .sort_by_key(|(_, generated)| program.engine.sources.resolve(generated.site).start);
        let bytes = generated
            .iter()
            .try_fold(0usize, |bytes, (path, source)| {
                bytes
                    .checked_add(path.to_string().len())?
                    .checked_add(source.text.len())?
                    .checked_add(24)
            })
            .ok_or_else(|| uninstalled())?;
        let allowance = installed
            .check
            .split(bytes as u64)
            .ok_or_else(|| uninstalled())?;
        let mut text = String::with_capacity(bytes);
        for (path, generated) in generated {
            if !text.is_empty() {
                text.push('\n');
            }
            use std::fmt::Write;
            writeln!(text, "// generated body of {path}\n{}", generated.text).unwrap();
        }
        Some(Arc::new(Expansion {
            text: text.into(),
            _allowance: allowance,
        }))
    } else {
        None
    };
    let check = installed.check(&mut program.engine, &selection)?;
    let plan = &installed.plan;
    let safe_allowance = if matches!(
        request.operation,
        CompileOperation::SafeCheck
            | CompileOperation::Artifacts { .. }
            | CompileOperation::ExportTir
    ) {
        Some(
            run.split(super::safe_check::BYTES)
                .ok_or_else(|| uninstalled())?,
        )
    } else {
        None
    };
    // Source metadata shares Run's existing snapshot partition. Export stages
    // reserve their measured buffers separately from the same process budget,
    // after source queries settle, preserving the front and Lower allowances.
    let mut native_allowance = native_footprint
        .as_ref()
        .map(|native| {
            run.split(native.bytes().ok_or_else(|| uninstalled())?)
                .ok_or_else(|| uninstalled())
        })
        .transpose()?;
    let export_allowance = if artifacts {
        let headers = program
            .engine
            .front_checked_signatures_bytes(&front)
            .map_err(&query_error)?;
        let headers = run.split(headers).ok_or_else(|| uninstalled())?;
        let export = run.split(run.bytes() / 4).ok_or_else(|| uninstalled())?;
        let output = run
            .split(super::proof_export::BYTES)
            .ok_or_else(|| uninstalled())?;
        Some((headers, export, output))
    } else {
        None
    };
    let mut ownership_allowance = ownership_footprint
        .as_ref()
        .map(|ownership| {
            run.split(ownership.bytes().ok_or_else(|| uninstalled())?)
                .ok_or_else(|| uninstalled())
        })
        .transpose()?;
    let mut run = if needs_run {
        let inputs = run_inputs.take().ok_or_else(|| uninstalled())?;
        let selected = if matches!(request.operation, CompileOperation::Precompute) {
            program
                .engine
                .front_semantic_queries_with_inputs(
                    front.clone(),
                    lower.clone(),
                    inputs,
                    plan.run,
                    run,
                    errors,
                )
                .and_then(|source| source.with_precompute(check.clone()))
        } else {
            program.engine.front_run_queries_with_inputs(
                check.clone(),
                front.clone(),
                lower.clone(),
                inputs,
                plan.run,
                run,
                errors,
            )
        };
        Some(selected.map_err(&query_error)?)
    } else {
        drop(run);
        None
    };
    if let CompileOperation::TirBatch { definitions } = &request.operation {
        run = Some(
            run.take()
                .ok_or_else(|| uninstalled())?
                .with_batch_roots(definitions.clone())
                .map_err(&query_error)?,
        );
    }
    let hash_selection = if let CompileOperation::Hashes { package } = &request.operation {
        let selected = package.as_ref().unwrap_or(&root_package);
        let roots = definitions
            .iter()
            .filter(|path| &path.0.package == selected)
            .cloned()
            .collect::<Vec<_>>();
        let root = roots
            .iter()
            .find(|path| **path == program.entry)
            .or_else(|| roots.first())
            .cloned()
            .ok_or_else(|| {
                ToolError::Runtime("hash request selected an empty source package".into())
            })?;
        run = Some(
            run.take()
                .ok_or_else(|| uninstalled())?
                .with_definition_hashes(roots)
                .map_err(&query_error)?,
        );
        Some((package.clone(), root))
    } else {
        None
    };
    let artifact_assembly = if matches!(
        request.operation,
        CompileOperation::Artifacts { native: true }
    ) {
        let entry = launch_entry
            .clone()
            .unwrap_or_else(|| program.entry.clone());
        Some(super::native_selection::artifacts(
            &mut program,
            super::native_assembly::NativeSourceInputs {
                definitions: definitions.clone(),
                entry,
                front: front.clone(),
                lower: lower.clone(),
                run: run.take().ok_or_else(|| uninstalled())?,
                store: store.clone(),
                registrations: registrations.clone(),
            },
            native_footprint.as_ref().ok_or_else(|| uninstalled())?,
            &mut native_allowance.take().ok_or_else(|| uninstalled())?,
            plan.body,
            plan.native_root,
            errors,
            config.verify_store,
        )?)
    } else {
        None
    };
    // The export selects its own unshared Run view of the entry package; that
    // view replaces the ordinary Run for this graph.
    let mut export_products = None;
    if let Some((headers, allowance, output)) = export_allowance {
        let headers = program
            .engine
            .front_checked_signatures(front.clone(), headers)
            .map_err(&query_error)?;
        let entry = program.entry.clone();
        let export = if let Some((native, _)) = &artifact_assembly {
            program.engine.front_proof_export_from_native(
                &entry,
                native.source(),
                headers,
                allowance,
            )
        } else {
            program.engine.front_proof_export(
                &entry,
                run.take().ok_or_else(|| uninstalled())?,
                headers,
                allowance,
            )
        }
        .map_err(&query_error)?;
        run = Some(export.run().clone());
        export_products = Some((export, output));
    }
    let safe = safe_allowance
        .map(|allowance| {
            super::safe_check::SafeCheck::new(
                check.clone(),
                run.as_ref().unwrap().clone(),
                launch_entry.clone(),
                store.clone(),
                errors,
                allowance,
            )
        })
        .transpose()?;
    let export_products = export_products.map(|(capture, allowance)| {
        super::proof_export::ProofExport::new(
            safe.as_ref().unwrap().clone(),
            capture,
            allowance,
            budget.process_budget(),
            store.clone(),
            errors,
        )
    });
    let native_artifacts = artifact_assembly.map(|(assembly, allowance)| {
        super::native_artifacts::NativeArtifacts::new(
            assembly,
            export_products.as_ref().unwrap().clone(),
            budget.process_budget(),
            store.clone(),
            errors,
            plan.body,
            allowance,
        )
    });
    let native = if let Some(footprint) = native_footprint
        .as_ref()
        .filter(|_| native_artifacts.is_none())
    {
        let (native_request, purpose) = match &request.operation {
            CompileOperation::NativeBuild(request) => {
                (request, super::native_platform::NativePurpose::Build)
            }
            CompileOperation::NativeRun(request) => {
                (request, super::native_platform::NativePurpose::Run)
            }
            _ => return Err(uninstalled()),
        };
        let entry = native_request
            .definition
            .as_ref()
            .or(launch_entry.as_ref())
            .cloned()
            .ok_or_else(|| ToolError::Runtime("launch manifest requires entry".into()))?;
        Some(
            super::native_selection::NativeSelection::new(
                &mut program,
                super::native_assembly::NativeSourceInputs {
                    definitions: definitions.clone(),
                    entry,
                    front: front.clone(),
                    lower: lower.clone(),
                    run: run.take().ok_or_else(|| uninstalled())?,
                    store: if host_toolchain {
                        super::native_store::capture(
                            store.clone(),
                            native_request,
                            native_transport,
                            scratch_parent,
                            disk_bytes,
                        )?
                    } else {
                        store.clone()
                    },
                    registrations: registrations.clone(),
                },
                native_request.clone(),
                purpose,
                config.verify_store,
                selected_toolchain.ok_or_else(|| {
                    ToolError::Runtime("native request requires its selected toolchain".into())
                })?,
                footprint,
                &mut native_allowance.take().ok_or_else(|| uninstalled())?,
                plan.body,
                plan.native_root,
                errors,
                disk_bytes,
            )
            .map_err(|error| super::host_selector::retain_error(error, source.clone()))?,
        )
    } else {
        None
    };
    let ownership = if let CompileOperation::NativeOwnership(selected) = &request.operation {
        let entry = selected
            .definition
            .as_ref()
            .or(launch_entry.as_ref())
            .cloned()
            .ok_or_else(|| uninstalled())?;
        Some(
            super::ownership_selection::OwnershipSelection::new(
                &mut program,
                super::ownership_admission::SourceInputs {
                    definitions: definitions.clone(),
                    catalogue: native_catalogue.take().ok_or_else(|| uninstalled())?,
                    entry,
                    front: front.clone(),
                    lower: lower.clone(),
                    run: run.take().ok_or_else(|| uninstalled())?,
                    store: store.clone(),
                },
                selected,
                config.verify_store,
                ownership_footprint.as_ref().ok_or_else(|| uninstalled())?,
                &mut ownership_allowance.take().ok_or_else(|| uninstalled())?,
                plan.body,
                errors,
            )
            .map_err(|error| super::host_selector::retain_error(error, source.clone()))?,
        )
    } else {
        None
    };
    let source_roots = match &request.operation {
        CompileOperation::NativeSources { roots } => Some(roots.as_slice()),
        CompileOperation::DataAbi => Some(&[][..]),
        _ => None,
    };
    let native_sources = if let Some(roots) = source_roots {
        let entry = roots
            .first()
            .cloned()
            .or_else(|| definitions.iter().find(|p| **p == program.entry).cloned())
            .or_else(|| {
                definitions
                    .iter()
                    .find(|p| p.0.package == root_package)
                    .cloned()
            })
            .ok_or_else(|| uninstalled())?;
        let footprint = ownership_footprint.as_ref().ok_or_else(|| uninstalled())?;
        let mut allowance = ownership_allowance.take().ok_or_else(|| uninstalled())?;
        let catalogue = program
            .engine
            .front_native_definition_queries_with_inputs(
                front.clone(),
                lower.clone(),
                native_catalogue.take().ok_or_else(|| uninstalled())?,
                plan.body,
                allowance
                    .split(footprint.catalogue)
                    .ok_or_else(|| uninstalled())?,
                errors,
            )
            .map_err(&query_error)?;
        let source = program
            .engine
            .front_native_queries(
                hirsel_engine::FrontNativePlan {
                    entry,
                    roots: roots.to_vec(),
                    run: run.take().ok_or_else(|| uninstalled())?,
                    definitions: catalogue,
                },
                plan.run,
                allowance
                    .split(footprint.source)
                    .ok_or_else(|| uninstalled())?,
                errors,
            )
            .map_err(&query_error)?;
        Some((roots.to_vec(), source))
    } else {
        None
    };
    // The ordinary source namespace is immutable before the first request.
    // Preserve the original small-graph subscription/factory headroom while
    // admitting every actual P/Lower/Evaluate/Run address of larger packages.
    let mut inventory = std::collections::BTreeSet::new();
    for path in &definitions {
        for query in [
            front.query(path),
            front.body_query(path),
            lower.query(path),
            lower.evaluate_query(path),
            run.as_ref().and_then(|run| run.query(path)),
        ]
        .into_iter()
        .flatten()
        {
            inventory.insert((query.kind, query.address.0));
        }
    }
    for query in [
        Some(growth.query()),
        Some(check.query()),
        safe.as_ref().map(|safe| safe.query()),
        export_products.as_ref().map(|export| export.query()),
    ]
    .into_iter()
    .flatten()
    {
        inventory.insert((query.kind, query.address.0));
    }
    if let Some(artifacts) = &native_artifacts {
        inventory.insert((artifacts.query().kind, artifacts.query().address.0));
        for query in artifacts.assembly.queries(&definitions) {
            inventory.insert((query.kind, query.address.0));
        }
    }
    if let Some(native) = &native {
        for query in native.assembly.queries(&definitions) {
            inventory.insert((query.kind, query.address.0));
        }
    }
    if let Some(ownership) = &ownership {
        for query in ownership.queries(&definitions) {
            inventory.insert((query.kind, query.address.0));
        }
    }
    if let Some((_, source)) = &native_sources {
        inventory.insert((source.query().kind, source.query().address.0));
        for query in source.run_query_inventory() {
            inventory.insert((query.kind, query.address.0));
        }
        for path in &definitions {
            for query in [source.definition_query(path)].into_iter().flatten() {
                inventory.insert((query.kind, query.address.0));
            }
        }
    }
    if let Some((_, graph)) = &storage {
        inventory.insert((graph.query().kind, graph.query().address.0));
    }
    if let Some(query) = run.as_ref().and_then(|run| run.batch_query()) {
        inventory.insert((query.kind, query.address.0));
    }
    if inventory.len() > plan.query_inventory {
        return Err(ToolError::Runtime(
            "compiler query inventory exceeds its planned admission".into(),
        ));
    }
    let runtime = program
        .engine
        .release_private_inference_admission()
        .map_err(query_error)?;
    let native_budget = runtime.bytes();
    let mut runtime =
        AdmittedRuntime::with_query_capacity(config.jobs, runtime, errors, plan.query_capacity)
            .map_err(ToolError::Io)?;
    if (native.is_some() || native_artifacts.is_some()) && hirsel_intern::timings::enabled() {
        use hirsel_intern::timings::{Count, count_peak};
        let workers = runtime.stats().workers as u64;
        count_peak(Count::NativeBudgetBytes, native_budget);
        count_peak(Count::NativeWorkers, workers);
        count_peak(
            Count::NativeStackBytes,
            if workers > 1 {
                workers * hirsel_intern::jobs::COMPILER_STACK_BYTES as u64
            } else {
                0
            },
        );
    }
    installed.register(&mut runtime, &check)?;
    if let Some(artifacts) = &native_artifacts {
        for (kind, factory) in artifacts.assembly.factories() {
            runtime.register(kind, factory).map_err(registration)?;
        }
        runtime
            .register(super::native_artifacts::KIND, artifacts.factory())
            .map_err(registration)?;
    } else if let Some(native) = &native {
        for (kind, factory) in native.assembly.factories() {
            runtime.register(kind, factory).map_err(registration)?;
        }
    } else if let Some(ownership) = &ownership {
        for (kind, factory) in ownership.factories() {
            runtime.register(kind, factory).map_err(registration)?;
        }
    } else if let Some((_, source)) = &native_sources {
        runtime
            .register(hirsel_engine::FRONT_RUN_KIND, source.run_factory())
            .map_err(registration)?;
        runtime
            .register(
                hirsel_engine::FRONT_NATIVE_DEFINITION_KIND,
                source.definition_factory(),
            )
            .map_err(registration)?;
        runtime
            .register(hirsel_engine::FRONT_NATIVE_ROOT_KIND, source.factory())
            .map_err(registration)?;
    } else if let Some(run) = &run {
        runtime
            .register(hirsel_engine::FRONT_RUN_KIND, run.factory())
            .map_err(registration)?;
        if run.batch_query().is_some() {
            runtime
                .register(hirsel_engine::FRONT_RUN_BATCH_KIND, run.factory())
                .map_err(registration)?;
        }
    }
    if let Some(safe) = &safe {
        runtime
            .register(super::safe_check::KIND, safe.factory())
            .map_err(registration)?;
    }
    if let Some(export) = &export_products {
        runtime
            .register(super::proof_export::KIND, export.factory())
            .map_err(registration)?;
    }
    let peak_type_bytes = program.engine.types().stats().row_bytes;
    let (input_operations, input_bytes) = request.inputs.read_work();
    let initial = RequestWork {
        hold_bytes: config.hold_bytes,
        store_adoptions: program.engine.store_adoptions(),
        load_nanos,
        dropped_engines: 1,
        peak_type_bytes,
        front: program.engine.work_stats(),
        memo_operations: program.engine.memo_stats().touched,
        input_operations,
        input_bytes,
        ..Default::default()
    };
    let counts = definitions
        .iter()
        .map(|path| {
            let phases = KINDS.map(|kind| program.engine.recompute_count(path, kind) as u64);
            (
                path.clone(),
                Counters {
                    phases,
                    validations: program.engine.body_validation_runs(path),
                    lower_calls: program.engine.request_lower_adapter_calls(path),
                },
            )
        })
        .collect();
    let touched_memos = program.engine.take_touched_memos();
    let initial = RequestWork {
        touched_memos,
        ..initial
    };
    let meter = GraphMeter::new(
        ledger,
        front.clone(),
        lower.clone(),
        SourceInspection {
            counts,
            memos: program.engine.memo_snapshot(),
            stats: program.engine.memo_stats().clone(),
            initial,
            owner: source,
        },
    );
    runtime
        .observe(Arc::new(MeterOwner(meter.clone())))
        .map_err(registration)?;
    // Move only source/header state. Scalar bodies, S1 and every controller
    // are dropped before the immutable graph becomes visible.
    if plan.source_index_bytes != 0 {
        let state = program
            .engine
            .into_source_index(source_index_allowance)
            .ok();
        let mut slot = source_index.lock().unwrap_or_else(|e| e.into_inner());
        if Arc::ptr_eq(&slot.generation, &source_generation) {
            slot.state = state;
        }
    }
    let metadata = Arc::new(InstalledMetadata {
        path: request.path.clone(),
        inputs: request.inputs.clone(),
        products: Arc::new(SourceMetadata {
            entry: program.entry.clone(),
            inputs: request.inputs.clone(),
            meter: meter.clone(),
            _store: store.clone(),
        }),
        errors,
    });
    let products = Arc::new(SourceProducts {
        source_index: source_index.clone(),
        source_generation,
        declaration_export: matches!(request.operation, CompileOperation::DataAbi),
        hash_selection,
        native_sources,
        expansion,
        entry: program.entry,
        inputs: request.inputs.clone(),
        lowering,
        launch_entry,
        run,
        native,
        native_artifacts,
        ownership,
        safe,
        receipt,
        export: export_products,
        front,
        lower,
        check,
        _growth: growth,
        storage,
        sources,
        store,
        initial_operation: Mutex::new(Some(store_operation)),
        completed_operation: Mutex::new(None),
        errors,
        meter,
        metadata,
    });
    Ok(Arc::new(InstalledGraph {
        path: request.path.clone(),
        inputs: request.inputs.clone(),
        runtime: Arc::new(Mutex::new(runtime)),
        products,
    }))
}

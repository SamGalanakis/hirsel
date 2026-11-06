//! Immutable source inputs and ordinary per-definition signature products.
//! Execution uses the scalar producer's stack-local recorder and canonical store
//! codec. No mutable query engine survives a production call.
use super::*;
use std::sync::{
    OnceLock, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
mod private;
mod sources;
pub(crate) use private::AdoptedPrivate;
pub use private::ModuleProof;
use sources::HeaderSources;
pub(super) use sources::source_refusal;
pub(crate) use sources::{CapturedText, SourceInputs};

type SignatureOutput = Result<Arc<QueryOutput<hirsel_types::SignatureRef>>, QueryError>;
pub(crate) struct HeaderProduct {
    pub(crate) result: SignatureOutput,
    pub(crate) entry: Option<Arc<Entry<hirsel_types::SignatureRef>>>,
    pub(crate) recomputes: usize,
    graph_work: usize,
    publication: bool,
    charge: Charge,
    _row_charge: Charge,
    origin: Weak<Entry<hirsel_types::SignatureRef>>,
    accounted: AtomicBool,
}
pub(crate) trait HeaderAllowance: Send + Sync {
    fn take_header(&self, bytes: u64) -> Option<hirsel_engine_admission::MemoryReservation>;
}
/// What one settled header row or module proof holds against a budget.
enum HeaderCharge {
    /// Charged to the shared standalone source-inference budget.
    Standalone(
        #[expect(dead_code, reason = "held for its drop")]
        hirsel_engine_admission::MemoryReservation,
    ),
    /// Owned by shared syntax until its last source owner drops. It may use
    /// a Front allowance, but is not a row of the portable Front facts cache.
    Source(
        #[expect(dead_code, reason = "held for its drop")]
        hirsel_engine_admission::MemoryReservation,
    ),
    /// Lent by a live Front partition; evictable while unpinned.
    Front(FrontCharge),
}
/// A charge a Front partition lent. Dropping it returns the bytes to the
/// partition's grant and the reservation to its allowance, exactly once.
struct FrontCharge {
    credit: Arc<std::sync::Mutex<u64>>,
    bytes: u64,
    #[expect(dead_code, reason = "held for its drop")]
    allowance: hirsel_engine_admission::MemoryReservation,
}
impl Drop for FrontCharge {
    fn drop(&mut self) {
        *self.credit.lock().unwrap_or_else(|e| e.into_inner()) += self.bytes;
    }
}
/// One admitted payload, shared by its row, entry and raw signature readers.
#[derive(Clone)]
struct Charge(Option<Arc<std::sync::Mutex<HeaderCharge>>>);
impl Charge {
    fn engine() -> Self {
        Self(None)
    }
    fn retained(charge: HeaderCharge) -> Self {
        Self(Some(Arc::new(std::sync::Mutex::new(charge))))
    }
    fn front_bytes(&self) -> u64 {
        let Some(charge) = &self.0 else {
            return 0;
        };
        match &*charge.lock().unwrap_or_else(|e| e.into_inner()) {
            HeaderCharge::Front(charge) => charge.bytes,
            _ => 0,
        }
    }
    fn evictable(&self) -> bool {
        self.0.as_ref().is_some_and(|charge| {
            matches!(
                &*charge.lock().unwrap_or_else(|e| e.into_inner()),
                HeaderCharge::Front(_)
            )
        })
    }
}
/// The payload's aliases hold one reservation. Distinct memo rows retain their
/// own allocation charge until both their entry and output leases have ended.
pub(in crate::queries) struct HeaderPayload {
    charge: Charge,
    rows: std::sync::Mutex<Vec<HeaderRowLease>>,
}
struct HeaderRowLease {
    entry: Weak<Entry<hirsel_types::SignatureRef>>,
    output: Weak<QueryOutput<hirsel_types::SignatureRef>>,
    _charge: Charge,
}
type ValidationCaches = Arc<std::sync::Mutex<Vec<Weak<crate::snapshot::ValidationState>>>>;
#[derive(Clone)]
struct HeaderAdmission {
    validation_caches: ValidationCaches,
    remaining: Arc<std::sync::Mutex<u64>>,
    partition: Weak<dyn HeaderAllowance>,
    tables: Arc<std::sync::Mutex<Vec<Weak<HeaderProducts>>>>,
}
/// Exact inference inputs and admission identity, retained by the operation.
/// Every payload reference is weak; this value owns no reservation.
#[derive(Default)]
pub(crate) struct PrivateInferenceScope {
    inputs: Box<[[ContentHash; 3]]>,
    working: AtomicU64,
    registrations: Vec<Weak<AcceptedFrontRegistration>>,
    admission: std::sync::Mutex<Option<HeaderAdmission>>,
}
impl operation_inputs::OperationOwner {
    fn inference_scope(&self, inputs: &[[ContentHash; 3]]) -> Arc<PrivateInferenceScope> {
        self.inference
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .find(|scope| scope.inputs.as_ref() == inputs)
            .cloned()
            .unwrap_or_else(|| {
                Arc::new(PrivateInferenceScope {
                    inputs: inputs.into(),
                    ..Default::default()
                })
            })
    }
}
impl HeaderAdmission {
    fn lend(&self, charge: FrontCharge) -> Charge {
        Charge::retained(HeaderCharge::Front(charge))
    }
}
#[derive(Default)]
struct HeaderSlot {
    product: std::sync::Mutex<Option<Arc<HeaderProduct>>>,
    work: AtomicU64,
}
impl HeaderSlot {
    fn get(&self) -> Option<Arc<HeaderProduct>> {
        self.product
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Settle `product` unless a row is already resident. Returns the
    /// resident row and whether it is this one.
    fn set(&self, product: HeaderProduct) -> (Arc<HeaderProduct>, bool) {
        self.set_shared(Arc::new(product))
    }
    fn set_shared(&self, product: Arc<HeaderProduct>) -> (Arc<HeaderProduct>, bool) {
        let mut slot = self.product.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(resident) = slot.as_ref() {
            return (resident.clone(), false);
        }
        self.work
            .fetch_add(product.graph_work as u64, Ordering::Relaxed);
        *slot = Some(product.clone());
        (product, true)
    }
}
#[derive(Clone)]
struct HeaderPhases {
    lexer: Option<Arc<dyn LexerPhase>>,
    parser: Option<Arc<dyn ParserPhase>>,
    signature: Option<Arc<dyn SignaturePhase>>,
    resolver: Option<Arc<dyn ResolverPhase>>,
    checker: Option<Arc<dyn CheckerPhase>>,
    modes: Option<Arc<dyn ModesPhase>>,
}
impl HeaderPhases {
    fn capture(adapters: &PhaseAdapters) -> Self {
        Self {
            lexer: adapters.lexer.clone(),
            parser: adapters.parser.clone(),
            signature: adapters.signature.clone(),
            resolver: adapters.resolver.clone(),
            checker: adapters.checker.clone(),
            modes: adapters.modes.clone(),
        }
    }
    fn adapters(&self) -> PhaseAdapters {
        PhaseAdapters {
            lexer: self.lexer.clone(),
            parser: self.parser.clone(),
            signature: self.signature.clone(),
            resolver: self.resolver.clone(),
            checker: self.checker.clone(),
            modes: self.modes.clone(),
            ..Default::default()
        }
    }
}
pub(crate) struct HeaderProducts {
    operation: Arc<operation_inputs::OperationOwner>,
    rows: hirsel_syntax::DefMap<HeaderSlot>,
    paths: Arc<[DefId]>,
    generation: AtomicU64,
    /// Bumped by every row this table publishes after construction. An engine
    /// adopts the completed rows once per revision, not once per read.
    revision: AtomicU64,
    scope: std::sync::Mutex<Arc<PrivateInferenceScope>>,
    validation_caches: ValidationCaches,
    environment: QueryEnvironment,
    adapters: HeaderPhases,
    sources: SourceMap,
    owners: Arc<crate::NativeRecordContext>,
    provisional: bool,
    standalone: bool,
    failure: Option<QueryError>,
    syntax: Arc<HeaderSources>,
    inputs: Arc<SourceInputs>,
    private: Option<private::PrivateProducts>,
    declarations: crate::DeclarationIndex,
    catalog: catalog::Catalog,
    installed_sources: BTreeSet<FileId>,
    types: Arc<hirsel_types::TypeInterner>,
    memo: Arc<hirsel_types::TypeMemo>,
    builtins: BTreeMap<DefPath, hirsel_types::SignatureRef>,
    registry: hirsel_registry::BuiltinRegistry,
    package_manifests: BTreeMap<hirsel_syntax::PackageId, ContentHash>,
    package_sources: BTreeMap<hirsel_syntax::PackageId, ContentHash>,
    published_manifest: Option<ContentHash>,
    store: Option<Arc<dyn hirsel_engine_store::ResultStore>>,
    identity: ContentHash,
    verify_store: hirsel_engine_store::VerifyStore,
    verify: Verify,
    limits: crate::ComptimeLimits,
    staged: BTreeMap<DefPath, crate::comptime::StagedConstant>,
    summaries: Arc<hirsel_syntax::DefMap<hirsel_types::UniqueSummary>>,
    /// The reads each carried summary's module proof recorded: a module
    /// producer validates them like any previous proof.
    summary_reads: Arc<hirsel_syntax::DefMap<Arc<hirsel_query_products::reads::Reads>>>,
    failures: BTreeMap<DefId, Vec<hirsel_types::UniqueFailure>>,
}
impl HeaderProducts {
    pub(in crate::queries) fn structural_checks(
        &self,
        checker: &dyn CheckerPhase,
    ) -> Option<(&parallel::structural::Checks, Arc<dyn CheckerPhase>)> {
        let owned = self.adapters.checker.as_ref()?;
        std::ptr::eq(checker, &**owned).then(|| (&self.operation.structural, owned.clone()))
    }

    pub(crate) fn fact_graph(&self) -> Arc<hirsel_validate::FactGraph> {
        self.store
            .as_ref()
            .and_then(|store| store.verifier())
            .map_or_else(
                || self.operation.fact_graph.clone(),
                |verifier| verifier.fact_graph(),
            )
    }

    fn scope(&self) -> Arc<PrivateInferenceScope> {
        self.scope.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn track_scope(self: &Arc<Self>) {
        if let Some(admission) = self
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let mut tables = admission.tables.lock().unwrap_or_else(|e| e.into_inner());
            tables.retain(|table| table.strong_count() != 0);
            if !tables
                .iter()
                .any(|table| table.ptr_eq(&Arc::downgrade(self)))
            {
                tables.push(Arc::downgrade(self));
            }
        }
    }
    pub(crate) fn operation_owned(&self) -> bool {
        self.inputs.unadmitted_bytes() == 0
    }
    pub(crate) fn needs_written_signatures(&self) -> bool {
        self.adapters.signature.is_none()
    }
    pub(crate) fn staged_constants(&self) -> &BTreeMap<DefPath, crate::comptime::StagedConstant> {
        &self.staged
    }
    pub(crate) fn capture(engine: &QueryEngine) -> Arc<Self> {
        let inputs = SourceInputs::capture(engine);
        let private = private::PrivateProducts::capture(engine);
        let declarations = engine.declarations.frozen();
        let paths: Arc<[_]> = engine
            .source_paths()
            .into_iter()
            .map(|path| DefId::intern(&path))
            .collect();
        let rows = paths
            .iter()
            .copied()
            .map(|id| {
                let cell = HeaderSlot::default();
                if engine.unmarked_signature(id).is_some()
                    && private
                        .as_ref()
                        .is_none_or(|private| !private.requires_proof(id))
                {
                    let entry = engine.signatures[&id].clone();
                    let projected = engine.operation.project_header(&entry);
                    let publication = engine.private_signature_publications.contains(&id);
                    let existing = engine.operation.header(&projected, publication);
                    if let Some(existing) = existing {
                        cell.set_shared(existing);
                    } else {
                        let _ = cell.set(HeaderProduct {
                            result: projected.result(),
                            entry: Some(projected),
                            recomputes: 0,
                            graph_work: 0,
                            publication: engine.private_signature_publications.contains(&id),
                            charge: Charge::engine(),
                            _row_charge: Charge::engine(),
                            origin: Arc::downgrade(&entry),
                            accounted: AtomicBool::new(true),
                        });
                    }
                }
                (id, cell)
            })
            .collect();
        let owners = Arc::new(engine.source_records());
        let summaries: hirsel_syntax::DefMap<_> = engine
            .private_summaries
            .iter()
            .filter(|(id, _)| {
                declarations
                    .metadata()
                    .definitions
                    .get(id.path())
                    .is_some_and(|metadata| !metadata.public)
                    && engine.source_facts(id.path()).is_some_and(|facts| {
                        (facts.body_kind == BodyKind::Function || facts.generator)
                            && facts.body_kind != BodyKind::Constant
                            && !facts.instance_bodied
                    })
            })
            .map(|(id, summary)| (*id, summary.clone()))
            .collect();
        let headers = Arc::new(Self {
            operation: engine.operation.clone(),
            rows,
            paths,
            generation: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            scope: std::sync::Mutex::new(
                engine
                    .header_products
                    .as_ref()
                    .map(|headers| headers.scope())
                    .filter(|scope| scope.inputs == engine.private_summary_inputs)
                    .unwrap_or_else(|| {
                        engine
                            .operation
                            .inference_scope(&engine.private_summary_inputs)
                    }),
            ),
            validation_caches: Default::default(),
            environment: engine.environment.clone(),
            adapters: HeaderPhases::capture(&engine.adapters),
            sources: engine.sources.clone(),
            owners: owners.clone(),
            provisional: engine.private_summary_active,
            standalone: !engine.private_inference_hosted && !engine.private_summary_active,
            failure: engine.current_private_summary_failure().cloned(),
            syntax: inputs.syntax.clone(),
            inputs: inputs.clone(),
            private,
            declarations: engine.declarations.header_inputs(),
            catalog: inputs.catalog.clone(),
            installed_sources: engine.installed_sources.clone(),
            types: engine.types.clone(),
            memo: engine.memo.clone(),
            builtins: engine.builtins.clone(),
            registry: engine.registry.clone(),
            package_manifests: engine.package_manifests.clone(),
            package_sources: engine.package_sources.clone(),
            published_manifest: engine.published_manifest,
            store: engine.result_store.clone(),
            identity: engine.store_identity,
            verify_store: engine.verify_store,
            verify: engine.verify,
            limits: engine.comptime_limits,
            staged: engine.staged_constants.clone(),
            summary_reads: Arc::new(
                summaries
                    .keys()
                    .filter_map(|id| Some((*id, engine.private_summary_reads.get(id)?.clone())))
                    .collect(),
            ),
            summaries: Arc::new(summaries),
            failures: engine.private_unique_failures.clone(),
        });
        headers.track_scope();
        headers
    }
    fn declared_inputs(self: &Arc<Self>, engine: &QueryEngine) -> Option<Arc<Self>> {
        (self.provisional
            && engine.private_summary_active
            && self.declarations.revision() == engine.declarations.revision()
            && self.staged.len() == engine.staged_constants.len()
            && self.staged.iter().all(|(path, previous)| {
                engine.staged_constants.get(path).is_some_and(|current| {
                    previous.owner == current.owner
                        && previous.expression == current.expression
                        && previous.inputs == current.inputs
                        && previous.signature.fingerprint == current.signature.fingerprint
                })
            })
            && self.environment == engine.environment
            && self.verify == engine.verify
            && self.limits == engine.comptime_limits
            && self.published_manifest == engine.published_manifest
            && Arc::ptr_eq(&self.types, &engine.types)
            && engine.private_signature_publications.is_empty())
        .then(|| self.clone())
    }
    pub(in crate::queries) fn execution(self: &Arc<Self>) -> QueryEngine {
        let mut engine =
            QueryEngine::new_with_hold_bytes(self.environment.clone(), self.adapters.adapters(), 0);
        engine.operation = self.operation.clone();
        engine.private_summary_inputs = self.scope().inputs.clone();
        engine.sources = self.sources.clone();
        engine.source_ownership = Arc::default();
        engine.private_summary_active = self.provisional;
        engine.private_summary_failure = self
            .failure
            .clone()
            .map(|error| (engine.graph.revision(), error));
        engine.items = native_records::SourceTable::new(engine.source_ownership.clone());
        engine.declarations = self.declarations.clone();
        engine.catalog = self.catalog.clone();
        engine.installed_sources = self.installed_sources.clone();
        engine
            .catalog
            .bind_ownership(engine.source_ownership.clone());
        engine.types = self.types.clone();
        engine.memo = self.memo.clone();
        engine.builtins = self.builtins.clone();
        engine.registry = self.registry.clone();
        engine.package_manifests = self.package_manifests.clone();
        engine.package_sources = self.package_sources.clone();
        engine.published_manifest = self.published_manifest;
        engine.result_store = self.store.clone();
        engine.store_identity = self.identity;
        engine.verify_store = self.verify_store;
        engine.verify = self.verify;
        engine.comptime_limits = self.limits;
        engine.staged_constants = self.staged.clone();
        engine.source_inputs = Some(self.inputs.clone());
        engine.header_products = Some(self.clone());
        engine.inherit_source_records((*self.owners).clone());
        engine
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    /// Publish one row; a row already present keeps its product.
    /// Returns the resident row and whether it is this one.
    fn publish(&self, cell: &HeaderSlot, product: HeaderProduct) -> (Arc<HeaderProduct>, bool) {
        let mut rows = self
            .operation
            .headers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let id = product.entry.as_ref().map(|entry| entry.memo.key.def);
        let product = if let Some(id) = id {
            let versions = rows.entry(id).or_default();
            versions.retain(|row| row.strong_count() != 0);
            if let Some(existing) = versions.iter().filter_map(Weak::upgrade).find(|existing| {
                existing.publication == product.publication
                    && existing
                        .entry
                        .as_ref()
                        .zip(product.entry.as_ref())
                        .is_some_and(|(old, new)| Arc::ptr_eq(old, new))
            }) {
                existing
            } else {
                let product = Arc::new(product);
                versions.push(Arc::downgrade(&product));
                product
            }
        } else {
            Arc::new(product)
        };
        drop(rows);
        let (product, settled) = cell.set_shared(product);
        if settled {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        (product, settled)
    }
    pub(crate) fn source_facts(&self, id: DefId) -> Option<hirsel_query_products::SourceFacts> {
        self.syntax.facts(id)
    }
    pub(crate) fn captured_text(&self, id: DefId) -> Option<sources::CapturedText> {
        self.syntax.text(id)
    }
    pub(crate) fn owns_written(&self, syntax: &Arc<ItemSyntax>) -> bool {
        self.syntax.owns(syntax)
    }
    pub(crate) fn written_source_bytes(engine: &QueryEngine) -> u64 {
        HeaderSources::written_bytes(engine)
    }
    pub(crate) fn settled_signature(&self, id: DefId) -> Option<hirsel_types::SignatureRef> {
        self.rows
            .get(&id)?
            .get()?
            .result
            .as_ref()
            .ok()
            .map(|output| output.value.clone())
    }
    pub(crate) fn source_fact_index_bytes(engine: &QueryEngine, private: bool) -> u64 {
        let definitions = engine.source_paths().len();
        HeaderSources::facts_index_bytes(definitions)
            .saturating_add((size_of::<Arc<[DefId]>>() + 2 * size_of::<usize>()) as u64)
            .saturating_add(definitions.saturating_mul(size_of::<DefId>()) as u64)
            .saturating_add(
                engine
                    .catalog
                    .source_carry_bytes(&engine.items, &engine.indexed_sources),
            )
            .saturating_add(if private {
                private::PrivateProducts::index_bytes(engine)
            } else {
                0
            })
    }
    pub(crate) fn source_environment(&self) -> &QueryEnvironment {
        &self.environment
    }
    pub(crate) fn source_types(&self) -> &Arc<hirsel_types::TypeInterner> {
        &self.types
    }
    pub(crate) fn work_snapshot(&self) -> Vec<(DefPath, QueryKind, u64)> {
        self.rows
            .iter()
            .filter_map(|(id, slot)| {
                let work = slot.work.load(Ordering::Relaxed);
                (work != 0
                    || slot
                        .product
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some())
                .then(|| (id.path().clone(), QueryKind::Signature, work))
            })
            .collect()
    }
    pub(crate) fn definitions(&self) -> impl Iterator<Item = DefId> + '_ {
        self.paths.iter().copied()
    }
    /// The settled summary of `id`. Err only for a resource refusal of its
    /// module proof; a failed proof has no summary.
    pub(super) fn summary(
        &self,
        id: DefId,
    ) -> Result<Option<hirsel_types::UniqueSummary>, QueryError> {
        if self.private.is_some() {
            self.demanded_summary(id)
        } else {
            Ok(self.summaries.get(&id).cloned())
        }
    }
    pub(crate) fn owners(&self) -> &Arc<crate::NativeRecordContext> {
        &self.owners
    }
    pub(crate) fn materialize(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.syntax.materialize(engine, path)
    }
    pub(crate) fn contains(&self, id: DefId) -> bool {
        self.rows.contains_key(&id)
    }
    /// The settled row of `id`, produced on demand. A failed demand that
    /// settled nothing (a resource refusal) returns its error and leaves the
    /// row unsettled, so a later demand recomputes it.
    pub(crate) fn product(
        self: &Arc<Self>,
        id: DefId,
    ) -> Option<Result<Arc<HeaderProduct>, QueryError>> {
        let cell = self.rows.get(&id)?;
        if let Some(product) = cell.get() {
            return Some(Ok(product));
        }
        let demanded = self.demand(&mut self.execution(), id);
        match (cell.get(), demanded) {
            (Some(product), _) => Some(Ok(product)),
            (None, Err(error)) => Some(Err(error)),
            (None, Ok(_)) => None,
        }
    }
    /// A task may use an uncached header within its active scratch grant.
    /// Only a retained row enters the shared table or advances its revision.
    pub(crate) fn working_product(
        self: &Arc<Self>,
        id: DefId,
        scratch: u64,
    ) -> Option<Result<Arc<HeaderProduct>, QueryError>> {
        let cell = self.rows.get(&id)?;
        if let Some(product) = cell.get() {
            return Some(Ok(product));
        }
        let mut working = None;
        let demanded =
            self.produce_with_working(&mut self.execution(), id, Some((&mut working, scratch)));
        match (cell.get().or(working), demanded) {
            (Some(product), _) => Some(Ok(product)),
            (None, Err(error)) => Some(Err(error)),
            (None, Ok(_)) => None,
        }
    }
    fn admitted(&self) -> bool {
        self.scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    /// Demand the row of `id` for `engine`. A demand that fails without
    /// settling the row is a resource refusal: it is recorded on `engine`, so
    /// a producer whose read it was never publishes, and a later demand
    /// recomputes the row.
    pub(crate) fn demand(self: &Arc<Self>, engine: &mut QueryEngine, id: DefId) -> SignatureOutput {
        let result = self.produce(engine, id);
        if let Err(refusal @ QueryError::Operational(_)) = &result
            && self.rows.get(&id).is_some_and(|cell| cell.get().is_none())
        {
            engine.record_header_refusal(Some(refusal.clone()));
        }
        result
    }
    fn produce(self: &Arc<Self>, engine: &mut QueryEngine, id: DefId) -> SignatureOutput {
        self.produce_with_working(engine, id, None)
    }
    fn produce_with_working(
        self: &Arc<Self>,
        engine: &mut QueryEngine,
        id: DefId,
        working: Option<(&mut Option<Arc<HeaderProduct>>, u64)>,
    ) -> SignatureOutput {
        if let Some(failures) = self.failures.get(&id) {
            engine
                .private_unique_failures
                .entry(id)
                .or_insert_with(|| failures.clone());
        }
        // Only producing a row needs its module proof. A settled row is used
        // as is, so an evicted proof is not re-inferred for it.
        if engine.header_products.is_some()
            && let Some(product) = self.rows.get(&id).and_then(HeaderSlot::get)
            && !engine
                .active
                .contains(&(id.path().clone(), QueryKind::Signature))
        {
            return self.use_product(engine, id, &product);
        }
        // The held proof pins this module while its header is produced. A
        // failed module proof is a verdict, not a refusal: the header settles
        // through its ordinary query, which reads that same failure.
        let mut proof = match self.private_product(id) {
            Ok(proof) => proof,
            Err(QueryError::Diagnostics(_)) => None,
            Err(error) => return Err(error),
        };
        if engine.header_products.is_none() {
            self.adopt_private_products(engine)?;
        }
        let Some(cell) = self.rows.get(&id) else {
            return engine.signature_query_local(id);
        };
        if engine
            .active
            .contains(&(id.path().clone(), QueryKind::Signature))
        {
            return engine.signature_query_local(id);
        }
        if let Some(product) = cell.get() {
            return self.use_product(engine, id, &product);
        }
        // A header or proof read refused inside this producer leaves a
        // conservative result, so the result is never published.
        let outer = engine.take_header_refusal();
        let result = engine.signature_query_local(id);
        let refusal = engine.take_header_refusal();
        engine.record_header_refusal(outer.or_else(|| refusal.clone()));
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        if let Some(stale) = proof.take().filter(|proof| {
            proof
                .identities
                .get(&id)
                .is_some_and(|expected| *expected != private::identity(&result))
        }) {
            // The recomputed payload disagrees with the proof that named it.
            // Recompute the proof once; a second disagreement is a fault.
            self.invalidate_private(id, &stale);
            let fresh = match self.private_product(id) {
                Ok(proof) => proof,
                Err(QueryError::Diagnostics(_)) => None,
                Err(error) => return Err(error),
            };
            if engine.header_products.is_none() {
                self.adopt_private_products(engine)?;
            }
            if let Some(product) = cell.get() {
                return self.use_product(engine, id, &product);
            }
            if fresh.is_some_and(|proof| {
                proof
                    .identities
                    .get(&id)
                    .is_some_and(|expected| *expected != private::identity(&result))
            }) {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "private signature identity".into(),
                    message: format!(
                        "{} recomputes to a different signature than its module proof",
                        id.path()
                    ),
                }));
            }
        }
        let source = engine.signatures.get(&id);
        let local = engine.header_products.is_none();
        let origin = source
            .filter(|_| local)
            .map_or_else(Weak::new, Arc::downgrade);
        let entry = source.map(|entry| self.operation.project_header(entry));
        if let Some(existing) = entry
            .as_ref()
            .and_then(|entry| self.operation.header(entry, self.provisional))
        {
            let (product, settled) = cell.set_shared(existing);
            if settled {
                self.revision.fetch_add(1, Ordering::Release);
                self.generation.fetch_add(1, Ordering::Relaxed);
            }
            return self.use_product(engine, id, &product);
        }
        // Never wait while holding a recursive header stack. Concurrent pure
        // normalizations race only to publish the same immutable source product.
        let recomputes = engine.recompute_count(id.path(), QueryKind::Signature);
        let bytes = entry
            .as_ref()
            .and_then(|entry| crate::memo::retained::retained_signature_entry_bytes(entry))
            .unwrap_or(0)
            .saturating_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<HeaderProduct>::uninit(),
            ));
        let (charge, transient) = match self.admit_signature(entry.as_ref(), bytes) {
            Ok(charge) => (charge, false),
            Err(_)
                if working.as_ref().is_some_and(|(_, scratch)| {
                    bytes
                        .checked_add(hirsel_engine_admission::retained_arc_bytes(
                            &std::mem::MaybeUninit::<std::sync::Mutex<HeaderCharge>>::uninit(),
                        ))
                        .is_some_and(|required| required <= *scratch)
                }) =>
            {
                ((Charge::engine(), Charge::engine()), true)
            }
            Err(error) => return Err(error),
        };
        let product = HeaderProduct {
            _row_charge: charge.1,
            result: entry.as_ref().map_or(result, |entry| entry.result()),
            entry,
            recomputes,
            graph_work: if local { 0 } else { recomputes },
            publication: self.provisional,
            charge: charge.0,
            origin,
            accounted: AtomicBool::new(local),
        };
        if transient {
            let product = Arc::new(product);
            let result = product.result.clone();
            *working.expect("working header input").0 = Some(product);
            return result;
        }
        let (product, settled) = self.publish(cell, product);
        if settled {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        self.use_product(engine, id, &product)
    }
    fn use_product(
        &self,
        engine: &mut QueryEngine,
        id: DefId,
        product: &HeaderProduct,
    ) -> SignatureOutput {
        if engine.header_products.is_none() {
            engine.adopt_header_product(id, product);
        } else if let Some(entry) = &product.entry {
            engine.graph.adopt(
                entry.memo.key,
                entry.memo.reads.clone(),
                entry.memo.durability,
            );
            if product.charge.evictable()
                && !engine.private_summary_active
                && product.result.is_ok()
            {
                engine.signatures.remove(&id);
                engine.snapshot_sources.remove(&id);
            } else {
                engine.signatures.insert(id, entry.clone());
            }
            engine.adopted_headers = None;
        }
        product.result.clone()
    }
    /// Reserve syntax against the existing source allowance without debiting
    /// the Front metadata grant. The file owner retains this reservation even
    /// after the partition ends; its aliases never reserve the payload again.
    fn admit_source(&self, bytes: u64) -> Result<Charge, QueryError> {
        let bytes = bytes
            .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<std::sync::Mutex<HeaderCharge>>::uninit(),
            ))
            .ok_or_else(|| sources::source_refusal("syntax size overflow"))?;
        let partition = self
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|admission| admission.partition.upgrade());
        let allowance = match partition {
            Some(partition) => loop {
                if let Some(allowance) = partition.take_header(bytes) {
                    break allowance;
                }
                if !self.evict() {
                    return Err(sources::source_refusal(format!(
                        "source allowance cannot retain {bytes} bytes"
                    )));
                }
            },
            None => self.reserve_standalone(bytes)?,
        };
        Ok(Charge::retained(HeaderCharge::Source(allowance)))
    }
    pub(crate) fn validation_lease(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, QueryError> {
        self.validation_lease_with(bytes, true)
    }
    /// Optional cache publication must not discard existing source owners to
    /// attempt retention. A refusal leaves the exact caller-owned result live.
    pub(crate) fn try_validation_lease(
        &self,
        bytes: u64,
    ) -> Result<Arc<dyn Send + Sync>, QueryError> {
        self.validation_lease_with(bytes, false)
    }
    fn validation_lease_with(
        &self,
        bytes: u64,
        reclaim: bool,
    ) -> Result<Arc<dyn Send + Sync>, QueryError> {
        Ok(Arc::new(
            self.admit_with_eviction(
                bytes
                    .checked_add((size_of::<Charge>() + 2 * size_of::<usize>()) as u64)
                    .ok_or_else(|| sources::source_refusal("validation size overflow"))?,
                reclaim,
            )?,
        ))
    }
    /// Charge one settled row or proof to the front's header grant, evicting
    /// unpinned entries first. A refusal is a resource failure, never a verdict.
    fn admit(&self, bytes: u64) -> Result<Charge, QueryError> {
        self.admit_with_eviction(bytes, true)
    }
    fn admit_with_eviction(&self, bytes: u64, reclaim: bool) -> Result<Charge, QueryError> {
        let admission = self
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(admission) = admission else {
            return Ok(if self.standalone {
                Charge::retained(HeaderCharge::Standalone(self.reserve_standalone(bytes)?))
            } else {
                Charge::engine()
            });
        };
        let Some(partition) = admission.partition.upgrade() else {
            return Ok(Charge::retained(HeaderCharge::Standalone(
                self.reserve_standalone(bytes)?,
            )));
        };
        // The charge cell itself is part of what the partition retains.
        let bytes = bytes.saturating_add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<std::sync::Mutex<HeaderCharge>>::uninit(),
        ));
        let allowance = loop {
            let mut grant = admission
                .remaining
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *grant >= bytes
                && let Some(allowance) = partition.take_header(bytes)
            {
                *grant -= bytes;
                break allowance;
            }
            // Syntax and other source owners share the backing pool without
            // debiting this row grant. Reclaim optional facts on either limit.
            drop(grant);
            if !reclaim || !self.evict() {
                if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
                    let payloads = self
                        .operation
                        .payloads
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect::<Vec<_>>();
                    let mut payload_bytes = 0u64;
                    let mut row_bytes = 0u64;
                    let mut row_count = 0;
                    for payload in &payloads {
                        payload_bytes += payload.charge.front_bytes();
                        if let Ok(rows) = payload.rows.try_lock() {
                            for row in rows.iter() {
                                row_count += 1;
                                row_bytes += row._charge.front_bytes();
                            }
                        }
                    }
                    let wrappers = self
                        .operation
                        .headers
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .values()
                        .flatten()
                        .filter_map(Weak::upgrade)
                        .map(|product| product._row_charge.front_bytes())
                        .sum::<u64>();
                    eprintln!(
                        "OWNER HEADER SET payloads={} payload_bytes={} rows={} row_bytes={} wrappers={}",
                        payloads.len(),
                        payload_bytes,
                        row_count,
                        row_bytes,
                        wrappers
                    );
                    let tables = admission
                        .tables
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect::<Vec<_>>();
                    let mut seen = BTreeSet::new();
                    for table in tables {
                        let proofs = table.private.as_ref().map(|private| private.trace());
                        let mut rows = 0;
                        let mut charged = 0;
                        let mut aliases = 0;
                        for cell in table.rows.values() {
                            if let Some(product) = cell.get()
                                && let Some(cell) = &product.charge.0
                            {
                                let charge = cell.lock().unwrap_or_else(|e| e.into_inner());
                                if let HeaderCharge::Front(charge) = &*charge {
                                    rows += 1;
                                    if seen.insert(Arc::as_ptr(cell) as usize) {
                                        charged += charge.bytes;
                                    }
                                    aliases += product
                                        .result
                                        .as_ref()
                                        .map_or(0, |output| Arc::strong_count(&output.value));
                                }
                            }
                        }
                        eprintln!(
                            "OWNER HEADERS table={:p} rows={} unique_bytes={} signature_aliases={}",
                            Arc::as_ptr(&table),
                            rows,
                            charged,
                            aliases
                        );
                        eprintln!(
                            "OWNER PROOFS table={:p} counts={proofs:?}",
                            Arc::as_ptr(&table)
                        );
                    }
                    eprintln!(
                        "OWNER HEADER REFUSAL required={} credit={}",
                        bytes,
                        *admission
                            .remaining
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                    );
                }
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "header admission".into(),
                    message: format!("header source allowance cannot retain {bytes} bytes"),
                }));
            }
        };
        Ok(admission.lend(FrontCharge {
            credit: admission.remaining.clone(),
            bytes,
            allowance,
        }))
    }
    fn admit_signature(
        &self,
        entry: Option<&Arc<Entry<hirsel_types::SignatureRef>>>,
        bytes: u64,
    ) -> Result<(Charge, Charge), QueryError> {
        let Some((entry, output)) =
            entry.and_then(|entry| Some((entry, entry.memo.value.as_ref().ok()?)))
        else {
            return Ok((self.admit(bytes)?, Charge::engine()));
        };
        let signature = &output.value;
        let payload_bytes =
            hirsel_query_products::retained::retained_signature_payload_bytes(signature);
        let payload = match signature.metadata.retention.get::<HeaderPayload>() {
            Some(payload) => payload,
            None => {
                let overhead = (size_of::<HeaderPayload>()
                    + 2 * size_of::<usize>()
                    + 4 * size_of::<Weak<HeaderPayload>>()) as u64;
                let charge = self.admit(payload_bytes.saturating_add(overhead))?;
                if charge.0.is_none() {
                    return Ok((charge, Charge::engine()));
                }
                let payload = signature.metadata.retention.retain(Arc::new(HeaderPayload {
                    charge,
                    rows: Default::default(),
                }));
                let mut payloads = self
                    .operation
                    .payloads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                payloads.retain(|payload| payload.strong_count() != 0);
                if payloads.capacity() > 4 * payloads.len().max(1) {
                    payloads.shrink_to_fit();
                }
                payloads.push(Arc::downgrade(&payload));
                payload
            }
        };
        let wrapper_bytes = hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
            HeaderProduct,
        >::uninit());
        let row_bytes = bytes
            .saturating_sub(payload_bytes)
            .saturating_sub(wrapper_bytes)
            .saturating_add((4 * size_of::<HeaderRowLease>()) as u64);
        let mut rows = payload.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.retain(|row| row.entry.strong_count() != 0 || row.output.strong_count() != 0);
        if !rows
            .iter()
            .any(|row| row.entry.ptr_eq(&Arc::downgrade(entry)))
        {
            let charge = self.admit(row_bytes)?;
            rows.push(HeaderRowLease {
                entry: Arc::downgrade(entry),
                output: Arc::downgrade(output),
                _charge: charge,
            });
        }
        // Bound spare Vec allocation by the charged four slots per live row.
        if rows.capacity() > 4 * rows.len().max(1) {
            rows.shrink_to_fit();
        }
        drop(rows);
        let wrapper = self.admit(wrapper_bytes.saturating_add((24 * size_of::<usize>()) as u64))?;
        Ok((payload.charge.clone(), wrapper))
    }
    /// Retain a payload a module proof settled, when the header grant admits it.
    fn seed(&self, id: DefId, entry: Arc<Entry<hirsel_types::SignatureRef>>) {
        let Some(cell) = self.rows.get(&id) else {
            return;
        };
        if cell.get().is_some() {
            return;
        }
        if let Some(existing) = self.operation.header(&entry, false) {
            let (_, settled) = cell.set_shared(existing);
            if settled {
                self.revision.fetch_add(1, Ordering::Release);
            }
            return;
        }
        let Some(bytes) = crate::memo::retained::retained_signature_entry_bytes(&entry) else {
            return;
        };
        let bytes = bytes.saturating_add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<HeaderProduct>::uninit(),
        ));
        let Ok(charge) = self.admit_signature(Some(&entry), bytes) else {
            return;
        };
        self.publish(
            cell,
            HeaderProduct {
                _row_charge: charge.1,
                result: entry.result(),
                entry: Some(entry),
                recomputes: 0,
                graph_work: 0,
                publication: false,
                charge: charge.0,
                origin: Weak::new(),
                accounted: AtomicBool::new(true),
            },
        );
    }
    pub(crate) fn ready(&self, id: DefId) -> Option<Arc<HeaderProduct>> {
        self.rows.get(&id)?.get()
    }
    pub(crate) fn configure_source_backing(
        self: &Arc<Self>,
        backing: Arc<dyn HeaderAllowance>,
        remaining: Arc<std::sync::Mutex<u64>>,
    ) -> Arc<dyn HeaderAllowance> {
        let scope = self.scope();
        let mut admission = scope.admission.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(partition) = admission
            .as_ref()
            .and_then(|admission| admission.partition.upgrade())
        {
            return partition;
        }
        *admission = Some(HeaderAdmission {
            validation_caches: self.validation_caches.clone(),
            remaining,
            partition: Arc::downgrade(&backing),
            tables: Arc::default(),
        });
        admission
            .as_ref()
            .unwrap()
            .tables
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::downgrade(self));
        backing
    }
    /// Headers a later snapshot of the same request captured are charged to
    /// the Front partition, and prove private modules in its working space,
    /// exactly as the Front producer's own headers are.
    pub(crate) fn inherit_snapshot_scope(
        self: &Arc<Self>,
        front: &Self,
        inputs: &[[ContentHash; 3]],
    ) {
        let shared = front
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let scope = self.scope();
        let mut admission = scope.admission.lock().unwrap_or_else(|e| e.into_inner());
        if admission.is_none() {
            if let Some(shared) = &shared {
                let mut tables = shared.tables.lock().unwrap_or_else(|e| e.into_inner());
                tables.retain(|table| table.strong_count() != 0);
                tables.push(Arc::downgrade(self));
            }
            *admission = shared;
        }
        drop(admission);
        self.inherit_private_snapshot(front, inputs);
    }
    pub(crate) fn inherit_registration_scope(
        self: &Arc<Self>,
        previous: &Self,
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) {
        let shared = previous
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let scope = self.scope();
        let mut admission = scope.admission.lock().unwrap_or_else(|e| e.into_inner());
        if admission.is_none() {
            if let Some(shared) = &shared {
                let mut tables = shared.tables.lock().unwrap_or_else(|e| e.into_inner());
                tables.retain(|table| table.strong_count() != 0);
                tables.push(Arc::downgrade(self));
            }
            *admission = shared;
        }
        drop(admission);
        self.inherit_private_scope(previous, registrations);
    }
    pub(crate) fn retain_validation_cache(
        &self,
        state: &Arc<crate::snapshot::ValidationState>,
    ) -> Result<(), hirsel_validate::ViewRefusal> {
        // Four vector slots per live owner bound minimum/spare allocation.
        // The live validation state owns its registry allocation charge.
        let bytes = 4 * size_of::<std::sync::Weak<crate::snapshot::ValidationState>>() + 128;
        let lease = self.validation_lease(bytes as u64).map_err(|cause| {
            hirsel_validate::ViewRefusal::Unreadable {
                definition: self
                    .rows
                    .keys()
                    .next()
                    .copied()
                    .unwrap_or_else(|| DefId::intern(&DefPath::core("validation-cache"))),
                cause,
            }
        })?;
        let registry = self.validation_cache_registry();
        let mut caches = registry.lock().unwrap();
        caches.retain(|state| state.strong_count() != 0);
        if caches.capacity() > 4 * caches.len().max(1) {
            caches.shrink_to_fit();
        }
        let _ = state.registration.set(lease);
        caches.push(Arc::downgrade(state));
        Ok(())
    }
    fn validation_cache_registry(&self) -> ValidationCaches {
        self.scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(
                || self.validation_caches.clone(),
                |scope| scope.validation_caches.clone(),
            )
    }
    fn evict_validation_cache(&self) -> bool {
        let registry = self.validation_cache_registry();
        let mut caches = registry.lock().unwrap();
        caches.retain(|state| state.strong_count() != 0);
        for state in caches.iter() {
            if let Some(state) = state.upgrade()
                && state.evict_cached()
            {
                return true;
            }
        }
        false
    }
    /// Evict one unpinned settled entry: a header row, else a module proof.
    pub(crate) fn evict(&self) -> bool {
        if self.operation.structural.evict()
            || self.reclaim_header_rows()
            || self.evict_validation_cache()
            || self.evict_row()
            || self.evict_proof()
            || self.syntax.evict()
        {
            return true;
        }
        let tables = self
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|admission| admission.tables.clone());
        let Some(tables) = tables else {
            return false;
        };
        let tables = tables
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        tables
            .iter()
            .filter(|table| !std::ptr::eq(self, &***table))
            .any(|table| table.evict_row() || table.evict_proof() || table.syntax.evict())
    }
    fn reclaim_header_rows(&self) -> bool {
        let payloads = self
            .operation
            .payloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        let mut reclaimed = false;
        for payload in payloads {
            let Ok(mut rows) = payload.rows.try_lock() else {
                continue;
            };
            let before = rows.len();
            rows.retain(|row| row.entry.strong_count() != 0 || row.output.strong_count() != 0);
            reclaimed |= before != rows.len();
            if rows.capacity() > 4 * rows.len().max(1) {
                rows.shrink_to_fit();
            }
        }
        reclaimed
    }
    fn evict_row(&self) -> bool {
        for (id, cell) in &self.rows {
            let Some(product) = cell.get() else {
                continue;
            };
            if !product.charge.evictable() {
                continue;
            }
            let Some(entry) = &product.entry else {
                continue;
            };
            if Arc::strong_count(entry) != 1 {
                continue;
            }
            let Ok(output) = &product.result else {
                continue;
            };
            // Raw signature aliases lease only their shared payload. Entry and
            // output consumers additionally retain the row's observations.
            if Arc::strong_count(output) != 2 {
                continue;
            }
            if entry
                .products
                .representation
                .as_ref()
                .and_then(|owner| owner.view.get())
                .is_some_and(|(view, _)| Arc::strong_count(view) != 1)
            {
                continue;
            }
            let tables = self
                .scope()
                .admission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|admission| admission.tables.clone());
            let Some(tables) = tables else {
                continue;
            };
            let tables = tables
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            let mut holders = 0;
            for table in &tables {
                if let Some(cell) = table.rows.get(id) {
                    let slot = cell.product.lock().unwrap_or_else(|e| e.into_inner());
                    if slot
                        .as_ref()
                        .is_some_and(|held| Arc::ptr_eq(held, &product))
                    {
                        holders += 1;
                    }
                }
            }
            // The local handle plus cache aliases are the only row holders.
            // A racing consumer retains the product's charges independently.
            if Arc::strong_count(&product) != holders + 1 {
                continue;
            }
            for table in &tables {
                if let Some(cell) = table.rows.get(id) {
                    let mut slot = cell.product.lock().unwrap_or_else(|e| e.into_inner());
                    if slot
                        .as_ref()
                        .is_some_and(|held| Arc::ptr_eq(held, &product))
                    {
                        *slot = None;
                    }
                }
            }
            if holders != 0 {
                return true;
            }
        }
        false
    }
    pub(crate) fn pending_recomputes(&self, id: DefId) -> usize {
        self.ready(id)
            .filter(|p| !p.accounted.load(Ordering::Relaxed))
            .map_or(0, |p| p.recomputes)
    }
    pub(crate) fn completed(&self) -> impl Iterator<Item = (DefId, Arc<HeaderProduct>)> + '_ {
        self.rows
            .iter()
            .filter_map(|(id, cell)| cell.get().map(|product| (*id, product)))
    }
}

/// The header cache as the source of V's sourced views: a settled row is
/// read as it is, and an evicted one is produced again under admission.
pub(crate) struct RowSource(pub(crate) Arc<HeaderProducts>);
impl hirsel_validate::SignatureSource for RowSource {
    fn reread(&self, id: DefId) -> Result<hirsel_types::SignatureRef, QueryError> {
        let headers = &self.0;
        let output = match headers.ready(id) {
            Some(product) => product.result.clone(),
            None => headers.demand(&mut headers.execution(), id),
        };
        output.map(|output| output.value.clone())
    }
}

/// The header table an engine last adopted, at the revision it adopted.
pub(crate) struct AdoptedHeaders {
    headers: Weak<HeaderProducts>,
    revision: u64,
    current: bool,
}
impl QueryEngine {
    pub(crate) fn declared_header_inputs(&self) -> Option<Arc<HeaderProducts>> {
        self.header_products
            .as_ref()
            .and_then(|headers| headers.declared_inputs(self))
    }
    pub(crate) fn inherit_demand_header_scope(
        &self,
        headers: &Arc<header_products::HeaderProducts>,
    ) {
        if self.private_bootstrap_active
            && let Some(previous) = self
                .snapshot
                .as_ref()
                .and_then(|(_, snapshot)| snapshot.get())
                .and_then(|snapshot| snapshot.header_products().cloned())
        {
            headers.inherit_snapshot_scope(&previous, &self.private_summary_inputs);
        }
    }
    pub(crate) fn adopt_completed_headers(&mut self) {
        let Some((stamp, snapshot)) = &self.snapshot else {
            return;
        };
        let current = *stamp == self.snapshot_stamp();
        let Some(snapshot) = snapshot.get() else {
            return;
        };
        let Some(headers) = snapshot.header_products().cloned() else {
            return;
        };
        if current {
            let _ = headers.adopt_private_products(self);
        }
        // Adopting a row is idempotent until the table publishes another row
        // or this engine replaces a signature (which clears the record).
        let revision = headers.revision();
        if self.adopted_headers.as_ref().is_some_and(|adopted| {
            adopted.headers.ptr_eq(&Arc::downgrade(&headers))
                && adopted.revision == revision
                && (adopted.current || !current)
        }) {
            return;
        }
        if current {
            self.adopt_header_products(&headers);
            // These signatures and private facts came from this exact table.
            // Their publication advances the engine, not the captured inputs.
            // Preserve the cache ownership mode when adopting completed rows.
            self.snapshot.as_mut().unwrap().0 = self.snapshot_stamp();
        } else {
            for (id, product) in headers.completed() {
                self.account_header_work(id, &product);
            }
        }
        self.adopted_headers = Some(AdoptedHeaders {
            headers: Arc::downgrade(&headers),
            revision,
            current,
        });
    }
    pub(crate) fn adopt_header_products(&mut self, headers: &header_products::HeaderProducts) {
        for (id, product) in headers.completed() {
            self.adopt_header_product(id, &product);
        }
    }
    fn account_header_work(&mut self, id: DefId, product: &HeaderProduct) {
        if !product.accounted.swap(true, Ordering::Relaxed) {
            *self
                .recomputes
                .entry((id.path().clone(), QueryKind::Signature))
                .or_default() += product.recomputes;
        }
    }
    fn adopt_header_product(&mut self, id: DefId, product: &HeaderProduct) {
        self.account_header_work(id, product);
        let Some(entry) = &product.entry else {
            return;
        };
        let same = self.signatures.get(&id).is_some_and(|current| {
            Arc::ptr_eq(current, entry)
                || product
                    .origin
                    .upgrade()
                    .is_some_and(|origin| Arc::ptr_eq(current, &origin))
        });
        // The scalar memo cache is optional. It must not pin an admitted
        // reader's payload after that reader finishes.
        if product.charge.evictable()
            && (self.header_products.is_none()
                || (!self.private_summary_active && product.result.is_ok()))
        {
            if same {
                self.signatures.remove(&id);
                self.snapshot_sources.remove(&id);
            }
            return;
        }
        if same {
            return;
        }
        self.graph.adopt(
            entry.memo.key,
            entry.memo.reads.clone(),
            entry.memo.durability,
        );
        self.signatures.insert(id, entry.clone());
        if self.private_summary_active {
            if product.publication {
                self.private_signature_publications.insert(id);
            }
        } else {
            self.private_signature_publications.remove(&id);
        }
        self.snapshot_sources.insert(id, Arc::downgrade(entry));
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn project(
    entry: &Arc<Entry<hirsel_types::SignatureRef>>,
) -> Arc<Entry<hirsel_types::SignatureRef>> {
    // Validity uses the complete memo. Program revision and semantic body
    // nodes still need the closure, so keep only the definitions it names.
    if entry.products.resolution.is_none()
        && !crate::memo::retained::source_projection(entry).detail
    {
        return entry.clone();
    }
    #[cfg(test)]
    PROJECTIONS.update(|count| count + 1);
    Arc::new(Entry {
        memo: entry.memo.clone(),
        detail: DependencyReads {
            operational_failure: entry.detail.operational_failure.clone(),
            closure: {
                let mut closure: Vec<_> = entry.detail.semantic_definitions().collect();
                closure.sort_unstable();
                closure.dedup();
                closure.shrink_to_fit();
                closure
            },
            ..Default::default()
        },
        products: Arc::new(PhaseProducts {
            resolution: None,
            ..(*entry.products).clone()
        }),
        covered: entry.covered.clone(),
        depth: entry.depth.clone(),
        acceptance: entry.acceptance.clone(),
        verification: Default::default(),
        selections: entry.selections.clone(),
        consistent: entry.consistent,
    })
}

impl operation_inputs::OperationOwner {
    fn header(
        &self,
        entry: &Arc<Entry<hirsel_types::SignatureRef>>,
        publication: bool,
    ) -> Option<Arc<HeaderProduct>> {
        self.headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&entry.memo.key.def)?
            .iter()
            .filter_map(Weak::upgrade)
            .find(|product| {
                product.publication == publication
                    && product
                        .entry
                        .as_ref()
                        .is_some_and(|old| Arc::ptr_eq(old, entry))
            })
    }
    fn project_header(
        &self,
        entry: &Arc<Entry<hirsel_types::SignatureRef>>,
    ) -> Arc<Entry<hirsel_types::SignatureRef>> {
        let mut entries = self.projections.lock().unwrap_or_else(|e| e.into_inner());
        let key = Arc::as_ptr(entry) as usize;
        if let Some((source, projected)) = entries.get(&key)
            && source
                .upgrade()
                .is_some_and(|source| Arc::ptr_eq(&source, entry))
            && let Some(projected) = projected.upgrade()
        {
            return projected;
        }
        if entries.len().is_multiple_of(256) {
            entries.retain(|_, (source, projected)| {
                source.strong_count() != 0 && projected.strong_count() != 0
            });
        }
        let projected = project(entry);
        entries.insert(key, (Arc::downgrade(entry), Arc::downgrade(&projected)));
        projected
    }
}

/// Demand syntax under the source partition already used by ordinary parsing.
/// The raw syntax Arc retains that debit beyond the capture or engine lifetime.
pub(super) fn decode_installed_syntax(
    headers: Option<&HeaderProducts>,
    records: &crate::NativeRecords,
    source: &dyn crate::NativeRecordSource,
    bytes: hirsel_artifact_store::Bytes,
) -> Result<Arc<ItemSyntax>, QueryError> {
    let requirement = records
        .installed_syntax(source, bytes)
        .ok_or_else(|| source_refusal("invalid installed body"))?;
    let charge = match headers {
        Some(headers) => headers.admit_source(requirement.required_bytes())?,
        None => sources::standalone_syntax(requirement.required_bytes())?,
    };
    let syntax = {
        let held = charge
            .0
            .as_ref()
            .ok_or_else(|| source_refusal("missing installed body reservation"))?
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let reservation = match &*held {
            HeaderCharge::Standalone(reservation) | HeaderCharge::Source(reservation) => {
                reservation
            }
            HeaderCharge::Front(front) => &front.allowance,
        };
        requirement
            .decode(reservation)
            .ok_or_else(|| source_refusal("invalid installed body"))?
    };
    syntax.retention.retain(Arc::new(charge));
    Ok(syntax)
}

//! Immutable source inputs and ordinary per-definition signature products.
//! Execution uses the scalar producer's stack-local recorder and canonical store
//! codec. No mutable query engine survives a production call.
use super::*;
use std::sync::{
    OnceLock, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
mod sources;
use sources::HeaderSources;
pub(super) use sources::source_refusal;
pub(crate) use sources::{CapturedText, SourceInputs};

type SignatureOutput = Result<Arc<QueryOutput<hirsel_types::SignatureRef>>, QueryError>;
pub(crate) struct HeaderProduct {
    pub(crate) result: SignatureOutput,
    pub(crate) entry: Option<Arc<Entry<hirsel_types::SignatureRef>>>,
    pub(crate) recomputes: usize,
    graph_work: usize,
    charge: Charge,
    _row_charge: Charge,
    origin: Weak<Entry<hirsel_types::SignatureRef>>,
    accounted: AtomicBool,
    recent: AtomicBool,
}
/// A partition's budget, which charges header facts as its evictable class.
pub(crate) trait HeaderAllowance: Send + Sync {
    fn header_budget(&self) -> &Arc<hirsel_engine_admission::ProcessBudget>;
    #[track_caller]
    fn take_header(&self, bytes: u64) -> Option<hirsel_engine_admission::MemoryReservation> {
        self.header_budget().reserve_evictable(bytes).ok()
    }
    /// A required header charge once nothing more evicts: it may use the
    /// headroom rather than refuse while the limit still has room.
    #[track_caller]
    fn take_required_header(
        &self,
        bytes: u64,
    ) -> Option<hirsel_engine_admission::MemoryReservation> {
        self.header_budget()
            .reserve_evictable_in_headroom(bytes)
            .ok()
    }
    fn available_header_bytes(&self) -> u64 {
        self.header_budget().evictable_free()
    }
}
/// What one settled header row holds against a budget.
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
/// A charge on a Front source's request footprint, released on drop.
struct FrontCharge {
    allowance: hirsel_engine_admission::MemoryReservation,
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
            HeaderCharge::Front(charge) => charge.allowance.bytes(),
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
    rows: std::sync::Mutex<HeaderRows>,
}
struct ValidationPayload {
    signature: Weak<hirsel_types::Signature>,
    _charge: Charge,
}
pub(in crate::queries) struct ValidationPayloadRegistration {
    owner: Weak<ValidationPayload>,
    _charge: Charge,
}
struct HeaderRowLease {
    entry: Weak<Entry<hirsel_types::SignatureRef>>,
    output: Weak<QueryOutput<hirsel_types::SignatureRef>>,
    _charge: Charge,
}
#[derive(Default)]
struct HeaderRows {
    leases: std::collections::HashMap<usize, HeaderRowLease>,
    // HashMap's free capacity can exclude tombstones without freeing buckets.
    // Keep the allocation's high-water capacity until an explicit shrink.
    capacity: usize,
}
impl HeaderRows {
    fn insert(&mut self, key: usize, lease: HeaderRowLease) {
        self.leases.entry(key).or_insert(lease);
        self.capacity = self.capacity.max(self.leases.capacity());
        self.shrink();
    }
    fn shrink(&mut self) {
        if self.leases.is_empty() || self.capacity > 4 * self.leases.len() {
            self.leases.shrink_to_fit();
            self.capacity = self.leases.capacity();
        }
    }
}
impl HeaderRowLease {
    fn live(&self) -> bool {
        self.entry.strong_count() != 0 || self.output.strong_count() != 0
    }
}
impl HeaderPayload {
    fn reclaim_row(&self, key: usize) {
        let Ok(mut rows) = self.rows.try_lock() else {
            return;
        };
        if rows.leases.get(&key).is_some_and(|row| !row.live()) {
            let expired = rows.leases.remove(&key);
            rows.shrink();
            drop(expired);
        }
    }
}
type ValidationCaches = Arc<std::sync::Mutex<Vec<Weak<crate::snapshot::SignatureSnapshot>>>>;
#[derive(Clone)]
struct HeaderAdmission {
    validation_caches: ValidationCaches,
    partition: Weak<dyn HeaderAllowance>,
    tables: Arc<std::sync::Mutex<Vec<Weak<HeaderProducts>>>>,
}
/// The header admission every table of one operation shares, retained by
/// the operation. Every payload reference is weak; this value owns no reservation.
#[derive(Default)]
pub(crate) struct HeaderScope {
    admission: std::sync::Mutex<Option<HeaderAdmission>>,
}
impl operation_inputs::OperationOwner {
    fn header_scope(&self) -> Arc<HeaderScope> {
        self.header_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default()
    }
}
/// Headers produced without a host partition reserve from this shared budget.
pub(in crate::queries) fn standalone_budget() -> &'static Arc<hirsel_engine_admission::ProcessBudget>
{
    static BUDGET: OnceLock<Arc<hirsel_engine_admission::ProcessBudget>> = OnceLock::new();
    BUDGET.get_or_init(|| hirsel_engine_admission::ProcessBudget::new(512 << 20))
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
        let product = self.peek();
        if let Some(product) = &product {
            product.recent.store(true, Ordering::Relaxed);
        }
        product
    }
    fn peek(&self) -> Option<Arc<HeaderProduct>> {
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
        product.recent.store(true, Ordering::Relaxed);
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
    eviction_hand: AtomicU64,
    /// Bumped by every row this table publishes after construction. An engine
    /// adopts the completed rows once per revision, not once per read.
    revision: AtomicU64,
    scope: std::sync::Mutex<Arc<HeaderScope>>,
    validation_caches: ValidationCaches,
    environment: QueryEnvironment,
    adapters: HeaderPhases,
    sources: SourceMap,
    owners: Arc<crate::NativeRecordContext>,
    standalone: bool,
    syntax: Arc<HeaderSources>,
    inputs: Arc<SourceInputs>,
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
}
impl HeaderProducts {
    /// A header produced without a host partition charges the shared
    /// standalone budget, with its reservation handle.
    fn reserve_standalone(
        &self,
        bytes: u64,
    ) -> Result<hirsel_engine_admission::MemoryReservation, QueryError> {
        let bytes = bytes
            .checked_add(
                (size_of::<hirsel_engine_admission::MemoryReservation>() + 2 * size_of::<usize>())
                    as u64,
            )
            .ok_or_else(|| source_refusal("standalone header charge overflows"))?;
        standalone_budget().reserve(bytes).map_err(|error| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "header admission".into(),
                message: format!("standalone header owner: {error}"),
            })
        })
    }
    pub(in crate::queries) fn structural_checks(
        &self,
        checker: &dyn CheckerPhase,
    ) -> Option<(&parallel::structural::Checks, Arc<dyn CheckerPhase>)> {
        let owned = self.adapters.checker.as_ref()?;
        std::ptr::eq(checker, &**owned).then(|| (&self.operation.structural, owned.clone()))
    }

    pub(crate) fn fact_graph(&self) -> Arc<hirsel_types::FactGraph> {
        self.store
            .as_ref()
            .and_then(|store| store.verifier())
            .map_or_else(
                || self.operation.fact_graph.clone(),
                |verifier| verifier.fact_graph(),
            )
    }

    fn scope(&self) -> Arc<HeaderScope> {
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
        let paths: Arc<[_]> = engine
            .header_products
            .as_ref()
            .filter(|headers| Arc::ptr_eq(&headers.inputs, &inputs))
            .map(|headers| headers.paths.clone())
            .unwrap_or_else(|| engine.source_paths_iter().map(DefId::intern).collect());
        let rows = paths
            .iter()
            .copied()
            .map(|id| {
                let cell = HeaderSlot::default();
                if engine.unmarked_signature(id).is_some() {
                    let entry = engine.signatures[&id].clone();
                    let projected = engine.operation.header_entry(&entry);
                    let existing = engine.operation.header(&projected);
                    if let Some(existing) = existing {
                        cell.set_shared(existing);
                    } else {
                        let _ = cell.set(HeaderProduct {
                            result: projected.result(),
                            entry: Some(projected),
                            recomputes: 0,
                            graph_work: 0,
                            charge: Charge::engine(),
                            _row_charge: Charge::engine(),
                            origin: Arc::downgrade(&entry),
                            recent: AtomicBool::new(true),
                            accounted: AtomicBool::new(true),
                        });
                    }
                }
                (id, cell)
            })
            .collect();
        let owners = Arc::new(engine.source_records());
        let headers = Arc::new(Self {
            operation: engine.operation.clone(),
            rows,
            paths,
            generation: AtomicU64::new(0),
            eviction_hand: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            scope: std::sync::Mutex::new(
                engine
                    .header_products
                    .as_ref()
                    .map(|headers| headers.scope())
                    .unwrap_or_else(|| engine.operation.header_scope()),
            ),
            validation_caches: Default::default(),
            environment: engine.environment.clone(),
            adapters: HeaderPhases::capture(&engine.adapters),
            sources: engine.sources.clone(),
            owners: owners.clone(),
            standalone: !engine.headers_hosted,
            syntax: inputs.syntax.clone(),
            inputs: inputs.clone(),
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
        });
        headers.track_scope();
        headers
    }
    pub(in crate::queries) fn execution(self: &Arc<Self>) -> QueryEngine {
        let mut engine =
            QueryEngine::new_with_hold_bytes(self.environment.clone(), self.adapters.adapters(), 0);
        engine.operation = self.operation.clone();
        engine.sources = self.sources.clone();
        engine.source_ownership = Arc::default();
        engine.headers_hosted = !self.standalone;
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
    /// An execution that produces one header and is then dropped.
    fn signature_execution(self: &Arc<Self>) -> QueryEngine {
        let mut engine = self.execution();
        engine.single_header = true;
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
                existing
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
        self.syntax
            .facts(id)
            .or_else(|| self.inputs.catalog.facts(id.path()).cloned())
    }
    pub(crate) fn captured_text(&self, id: DefId) -> Option<sources::CapturedText> {
        self.syntax.text(id)
    }
    pub(crate) fn declaration_candidates(&self, id: DefId) -> Option<ContentHash> {
        self.contains(id).then(|| {
            self.declarations
                .candidates(id.path())
                .or_else(|| self.syntax.text(id).map(|text| text.candidates))
                .unwrap_or_else(memos::no_candidates)
        })
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
    pub(crate) fn source_fact_index_bytes(engine: &QueryEngine) -> u64 {
        let definitions = engine.source_paths_iter().count();
        HeaderSources::facts_index_bytes(definitions)
            .saturating_add(size_of::<AtomicU64>() as u64)
            .saturating_add((size_of::<Arc<[DefId]>>() + 2 * size_of::<usize>()) as u64)
            .saturating_add(definitions.saturating_mul(size_of::<DefId>()) as u64)
            .saturating_add(
                engine
                    .catalog
                    .source_carry_bytes(&engine.items, &engine.indexed_sources),
            )
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
    pub(crate) fn materialize_siblings(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.syntax.materialize_siblings(engine, path)
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
            header_entry(id).hit();
            return Some(Ok(product));
        }
        let demanded = self.produce(&mut self.signature_execution(), id);
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
        cx: &mut dyn hirsel_engine_runtime::QueryContext,
    ) -> Option<Result<Arc<HeaderProduct>, QueryError>> {
        let cell = self.rows.get(&id)?;
        let mut credit = 0;
        loop {
            if let Some(product) = cell.get() {
                header_entry(id).hit();
                return Some(Ok(product));
            }
            let mut working = None;
            let demanded = self.produce_with_working(
                &mut self.signature_execution(),
                id,
                Some((&mut working, cx.scratch_bytes())),
            );
            match (cell.get().or(working), demanded) {
                (Some(product), _) => return Some(Ok(product)),
                (None, Err(error)) => {
                    if matches!(&error, QueryError::Operational(failure)
                        if matches!(failure.operation.as_str(), "header admission"))
                    {
                        let refused_credit = self.header_credit();
                        loop {
                            let available = self.header_credit();
                            if available > credit && available > refused_credit {
                                credit = available;
                                break;
                            }
                            if !cx.evict_body() {
                                return Some(Err(error));
                            }
                        }
                    } else {
                        return Some(Err(error));
                    }
                }
                (None, Ok(_)) => return None,
            }
        }
    }
    fn header_credit(&self) -> u64 {
        let admission = self
            .scope()
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(admission) = admission else {
            return 0;
        };
        let Some(partition) = admission.partition.upgrade() else {
            return 0;
        };
        partition.available_header_bytes()
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
        self.track_scope();
        if engine.header_products.is_some()
            && let Some(product) = self.rows.get(&id).and_then(HeaderSlot::get)
            && !engine
                .active
                .contains(&(id.path().clone(), QueryKind::Signature))
        {
            return self.use_product(engine, id, &product);
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
        // A header read refused inside this producer leaves a
        // conservative result, so the result is never published.
        crate::queries::operation_owner_trace(format_args!(
            "OWNER HEADER PRODUCE table={:p} id={id:?}",
            Arc::as_ptr(self)
        ));
        let outer = engine.take_header_refusal();
        let result = header_entry(id).filling(|| engine.signature_query_local(id));
        let refusal = engine.take_header_refusal();
        engine.record_header_refusal(outer.or_else(|| refusal.clone()));
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        let source = engine.signatures.get(&id);
        let local = engine.header_products.is_none();
        let origin = source
            .filter(|_| local)
            .map_or_else(Weak::new, Arc::downgrade);
        let entry = source.map(|entry| self.operation.header_entry(entry));
        if let Some(existing) = entry
            .as_ref()
            .and_then(|entry| self.operation.header(entry))
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
            charge: charge.0,
            origin,
            recent: AtomicBool::new(true),
            accounted: AtomicBool::new(local),
        };
        if transient {
            cell.work
                .fetch_add(product.graph_work as u64, Ordering::Relaxed);
            let product = Arc::new(product);
            let result = product.result.clone();
            if let Some((working, _)) = working {
                *working = Some(product);
            }
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
        header_entry(id).hit();
        if engine.header_products.is_none() {
            engine.adopt_header_product(id, product);
        } else if let Some(entry) = &product.entry {
            if product.charge.evictable() && product.result.is_ok() {
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
                self.restore_headroom(partition.header_budget(), 0);
                if let Some(allowance) = partition.take_header(bytes) {
                    break allowance;
                }
                if !self.evict() {
                    if let Some(allowance) = partition.take_required_header(bytes) {
                        break allowance;
                    }
                    return Err(sources::source_refusal(format!(
                        "source allowance cannot retain {bytes} bytes"
                    )));
                }
            },
            None => self.reserve_standalone(bytes)?,
        };
        Ok(Charge::retained(HeaderCharge::Source(allowance)))
    }
    pub(crate) fn validation_signature_lease(
        &self,
        signature: &hirsel_types::SignatureRef,
    ) -> Result<Arc<dyn Send + Sync>, QueryError> {
        if let Some(payload) = signature.metadata.retention.get::<HeaderPayload>() {
            return Ok(payload);
        }
        // Shared core payloads are process-owned registry inputs. A request
        // retains only an alias, not another allocation of their fields.
        if hirsel_registry::BuiltinRegistry::shared()
            .get(signature.definition)
            .is_some_and(|shared| Arc::ptr_eq(&shared, signature))
        {
            return Ok(signature.clone());
        }
        let mut payloads = self
            .operation
            .validation_payloads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        payloads.retain(|registration| registration.owner.strong_count() != 0);
        if payloads.capacity() > 4 * payloads.len() {
            payloads.shrink_to_fit();
        }
        if let Some(owner) = payloads
            .iter()
            .filter_map(|registration| registration.owner.upgrade())
            .find(|owner| std::ptr::eq(owner.signature.as_ptr(), Arc::as_ptr(signature)))
        {
            return Ok(owner);
        }
        // The registry keeps the weak allocation and its vector slots charged
        // until pressure reclaims them, even after the last view drops.
        let registration = self.admit(
            (size_of::<ValidationPayload>()
                + 2 * size_of::<usize>()
                + 4 * size_of::<ValidationPayloadRegistration>()) as u64,
        )?;
        let charge = self
            .admit(hirsel_query_products::retained::retained_signature_payload_bytes(signature))?;
        let owner = Arc::new(ValidationPayload {
            signature: Arc::downgrade(signature),
            _charge: charge,
        });
        payloads.push(ValidationPayloadRegistration {
            owner: Arc::downgrade(&owner),
            _charge: registration,
        });
        Ok(owner)
    }
    fn reclaim_validation_payloads(&self) -> bool {
        let Ok(mut payloads) = self.operation.validation_payloads.try_lock() else {
            return false;
        };
        let before = payloads.len();
        payloads.retain(|registration| registration.owner.strong_count() != 0);
        if payloads.capacity() > 4 * payloads.len() {
            payloads.shrink_to_fit();
        }
        before != payloads.len()
    }
    pub(crate) fn validation_demand_lease(
        &self,
        bytes: u64,
    ) -> Result<Arc<dyn Send + Sync>, QueryError> {
        if !self.standalone
            && self
                .scope()
                .admission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            return Err(sources::source_refusal(
                "validation demand precedes source admission",
            ));
        }
        self.validation_lease(bytes)
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
        if !self.standalone
            && self
                .scope()
                .admission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            return Err(sources::source_refusal(
                "optional validation retention precedes source admission",
            ));
        }
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
    /// Charge one settled row to the front's header grant, evicting
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
            if reclaim {
                self.restore_headroom(partition.header_budget(), 0);
            }
            if let Some(allowance) = partition.take_header(bytes) {
                break allowance;
            }
            // Reclaim optional facts until the request footprint has room.
            if !reclaim || !self.evict() {
                // A reclaiming admission is required once nothing more evicts;
                // a non-reclaiming one is optional and declines instead.
                if reclaim && let Some(allowance) = partition.take_required_header(bytes) {
                    break allowance;
                }
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
                            for row in rows.leases.values() {
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
                    crate::queries::operation_owner_trace(format_args!(
                        "OWNER HEADER SET payloads={} payload_bytes={} rows={} row_bytes={} wrappers={}",
                        payloads.len(),
                        payload_bytes,
                        row_count,
                        row_bytes,
                        wrappers
                    ));
                    let tables = admission
                        .tables
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect::<Vec<_>>();
                    let mut seen = BTreeSet::new();
                    for table in tables {
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
                                        charged += charge.allowance.bytes();
                                    }
                                    aliases += product
                                        .result
                                        .as_ref()
                                        .map_or(0, |output| Arc::strong_count(&output.value));
                                }
                            }
                        }
                        crate::queries::operation_owner_trace(format_args!(
                            "OWNER HEADERS table={:p} rows={} unique_bytes={} signature_aliases={}",
                            Arc::as_ptr(&table),
                            rows,
                            charged,
                            aliases
                        ));
                    }
                    crate::queries::operation_owner_trace(format_args!(
                        "OWNER HEADER REFUSAL required={} request_free={}",
                        bytes,
                        partition.available_header_bytes()
                    ));
                    crate::queries::operation_owner_trace(format_args!(
                        "OWNER HEADER REFUSAL STACK {}",
                        std::backtrace::Backtrace::force_capture()
                    ));
                }
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "header admission".into(),
                    message: format!("header source allowance cannot retain {bytes} bytes"),
                }));
            }
        };
        Ok(admission.lend(FrontCharge { allowance }))
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
        // Four buckets per lease cover its key, control bytes and spare capacity.
        let row_bytes = bytes
            .saturating_sub(payload_bytes)
            .saturating_sub(wrapper_bytes)
            .saturating_add((4 * (size_of::<HeaderRowLease>() + 2 * size_of::<usize>())) as u64);
        let key = Arc::as_ptr(entry) as usize;
        let owned = payload
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .leases
            .contains_key(&key);
        if !owned {
            // Admission can evict this payload's other rows. Never hold its
            // registry lock while asking the partition to reclaim memory.
            let charge = self.admit(row_bytes)?;
            let mut rows = payload.rows.lock().unwrap_or_else(|e| e.into_inner());
            rows.insert(
                key,
                HeaderRowLease {
                    entry: Arc::downgrade(entry),
                    output: Arc::downgrade(output),
                    _charge: charge,
                },
            );
        }
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
        if let Some(existing) = self.operation.header(&entry) {
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
                charge: charge.0,
                origin: Weak::new(),
                recent: AtomicBool::new(true),
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
    /// the Front partition exactly as the Front producer's own headers are.
    pub(crate) fn inherit_scope(self: &Arc<Self>, previous: &Self) {
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
    }
    /// Later captures of this operation share this table's header admission.
    pub(crate) fn share_scope(&self) {
        *self
            .operation
            .header_scope
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(self.scope());
    }
    pub(crate) fn retain_validation_cache(
        &self,
        snapshot: Weak<crate::snapshot::SignatureSnapshot>,
    ) -> Result<Arc<dyn Send + Sync>, QueryError> {
        let bytes = 4 * size_of::<Weak<crate::snapshot::SignatureSnapshot>>() + 128;
        let lease = self.validation_lease(bytes as u64)?;
        let registry = self.validation_cache_registry();
        let mut caches = registry.lock().unwrap();
        caches.retain(|cache| cache.strong_count() != 0);
        if caches.capacity() > 4 * caches.len().max(1) {
            caches.shrink_to_fit();
        }
        if !caches.iter().any(|cache| cache.ptr_eq(&snapshot)) {
            caches.push(snapshot);
        }
        Ok(lease)
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
        let snapshots = {
            let mut caches = registry.lock().unwrap();
            caches.retain(|cache| cache.strong_count() != 0);
            caches.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for snapshot in snapshots {
            if snapshot.evict_cached() {
                let mut caches = registry.lock().unwrap();
                caches.retain(|cache| {
                    cache.upgrade().is_some_and(|snapshot| {
                        snapshot
                            .cache_registration
                            .try_lock()
                            .map_or(true, |registration| registration.is_some())
                    })
                });
                caches.shrink_to_fit();
                return true;
            }
        }
        false
    }
    /// An eviction point: evict header facts, then the budget's other
    /// registered evictable charges, until `budget`'s headroom stays free
    /// after a further unevictable `charge`, or nothing more evicts.
    /// Unevictable growth is measured from here.
    pub(crate) fn restore_headroom(
        &self,
        budget: &hirsel_engine_admission::ProcessBudget,
        charge: u64,
    ) {
        while budget.short_of_headroom(charge) && (self.evict() || budget.evict_one()) {}
        budget.eviction_point();
    }
    /// Evict one unpinned settled entry.
    pub(crate) fn evict(&self) -> bool {
        if self.reclaim_validation_payloads()
            || self.operation.structural.evict()
            || self.evict_validation_cache()
            || self.evict_row()
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
        if let Some(tables) = tables {
            let tables = tables
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            if tables
                .iter()
                .filter(|table| !std::ptr::eq(self, &***table))
                .any(|table| table.evict_row() || table.syntax.evict())
            {
                return true;
            }
        }
        // A reader can outlive its evicted cache wrapper. Sweep those leases
        // only when no ordinary victim can make progress, before refusing.
        self.reclaim_header_rows()
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
            let before = rows.leases.len();
            rows.leases.retain(|_, row| row.live());
            reclaimed |= before != rows.leases.len();
            rows.shrink();
        }
        reclaimed
    }
    // PostgreSQL REL_17_STABLE src/backend/storage/buffer/freelist.c:315-354.
    // The clock advances past pinned rows and ages referenced rows before
    // selecting a cold victim. One reference bit replaces its usage counter;
    // immutable aliases share the decision, and retain their existing charges.
    fn evict_row(&self) -> bool {
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
        let count = self.paths.len();
        let mut remaining = count;
        while remaining != 0 {
            let index = self.eviction_hand.fetch_add(1, Ordering::Relaxed) as usize % count;
            let id = &self.paths[index];
            let cell = &self.rows[id];
            remaining -= 1;
            let Some(product) = cell.peek() else {
                continue;
            };
            if !product.charge.evictable() {
                continue;
            }
            let Some(entry) = &product.entry else {
                continue;
            };
            let Ok(_) = &product.result else {
                continue;
            };
            // Entry/output aliases retain their row lease in HeaderPayload.
            // Only a reader holding this wrapper pins its separate allocation.
            if entry
                .products
                .representation
                .as_ref()
                .and_then(|owner| owner.view.get())
                .is_some_and(|(view, _)| Arc::strong_count(view) != 1)
            {
                continue;
            }
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
            if product.recent.swap(false, Ordering::Relaxed) {
                remaining = count;
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
                crate::queries::operation_owner_trace(format_args!(
                    "OWNER HEADER EVICT table={:p} id={id:?} entry_aliases={} output_aliases={} payload_aliases={}",
                    self,
                    Arc::strong_count(entry),
                    product.result.as_ref().map_or(0, Arc::strong_count),
                    product
                        .result
                        .as_ref()
                        .map_or(0, |o| Arc::strong_count(&o.value))
                ));
                let key = Arc::as_ptr(entry) as usize;
                let payload = product
                    .result
                    .as_ref()
                    .ok()
                    .and_then(|output| output.value.metadata.retention.get::<HeaderPayload>());
                drop(product);
                if let Some(payload) = payload {
                    payload.reclaim_row(key);
                }
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
    fn retained_control_bytes(&self) -> Option<u64> {
        Some((size_of::<Self>() + 2 * size_of::<usize>()) as u64)
    }
    fn reread(&self, id: DefId) -> Result<hirsel_types::SignatureRef, QueryError> {
        let headers = &self.0;
        let output = match headers.ready(id) {
            Some(product) => product.result.clone(),
            None => headers.demand(&mut headers.signature_execution(), id),
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
    pub(crate) fn inherit_demand_header_scope(
        &self,
        headers: &Arc<header_products::HeaderProducts>,
    ) {
        if self.bootstrap_active
            && let Some(previous) = self
                .snapshot
                .as_ref()
                .and_then(|(_, snapshot)| snapshot.get())
                .and_then(|snapshot| snapshot.header_products().cloned())
        {
            headers.inherit_scope(&previous);
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
            // These signatures came from this exact table.
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
        if product.charge.evictable() && (self.header_products.is_none() || product.result.is_ok())
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
        self.signatures.insert(id, entry.clone());
        self.snapshot_sources.insert(id, Arc::downgrade(entry));
    }
}

impl QueryEngine {
    /// A header read refused while this engine produces a header. The
    /// producing demand fails with it instead of publishing a conservative row.
    pub(super) fn record_header_refusal(&self, refusal: Option<QueryError>) {
        if let Some(refusal) = refusal {
            self.header_refusal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert(refusal);
        }
    }
    pub(super) fn take_header_refusal(&self) -> Option<QueryError> {
        self.header_refusal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}
/// The complete product a header row owns. Only engine-local evaluation
/// state stays with its producer; resolver products and dependency detail
/// stay, so no reader ever recomputes what a row already settled.
fn owned_entry(
    entry: &Arc<Entry<hirsel_types::SignatureRef>>,
) -> Arc<Entry<hirsel_types::SignatureRef>> {
    if entry.products.mode_archive.is_none() && entry.verification.lock().unwrap().is_none() {
        return entry.clone();
    }
    Arc::new(Entry {
        memo: entry.memo.clone(),
        detail: entry.detail.clone(),
        products: Arc::new(PhaseProducts {
            mode_archive: None,
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
    fn header(&self, entry: &Arc<Entry<hirsel_types::SignatureRef>>) -> Option<Arc<HeaderProduct>> {
        self.headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&entry.memo.key.def)?
            .iter()
            .filter_map(Weak::upgrade)
            .find(|product| {
                product
                    .entry
                    .as_ref()
                    .is_some_and(|old| Arc::ptr_eq(old, entry))
            })
    }
    pub(super) fn header_entry(
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
        let projected = owned_entry(entry);
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
    let owner_bytes =
        hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<Charge>::uninit());
    let decode_bytes = requirement
        .required_bytes()
        .checked_add(owner_bytes)
        .ok_or_else(|| source_refusal("installed body size overflow"))?;
    let charge = match headers {
        Some(headers) => headers.admit_source(decode_bytes)?,
        None => sources::standalone_syntax(decode_bytes)?,
    };
    let syntax = {
        let mut held = charge
            .0
            .as_ref()
            .ok_or_else(|| source_refusal("missing installed body reservation"))?
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let reservation = match &mut *held {
            HeaderCharge::Standalone(reservation) | HeaderCharge::Source(reservation) => {
                reservation
            }
            HeaderCharge::Front(front) => &mut front.allowance,
        };
        let syntax = requirement
            .decode(reservation)
            .ok_or_else(|| source_refusal("invalid installed body"))?;
        // Preflight admits the decode peak. Only the output and its ownership
        // allocations survive the decoder's temporary buffers and byte view.
        let retained = hirsel_query_products::retained::retained_syntax_bytes(&syntax)
            .checked_add(owner_bytes)
            .and_then(|bytes| {
                bytes.checked_add(hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<std::sync::Mutex<HeaderCharge>>::uninit(),
                ))
            })
            .ok_or_else(|| source_refusal("installed body size overflow"))?;
        let scratch = reservation
            .bytes()
            .checked_sub(retained)
            .ok_or_else(|| source_refusal("installed body exceeds preflight admission"))?;
        drop(
            reservation
                .split(scratch)
                .ok_or_else(|| source_refusal("installed body scratch partition"))?,
        );
        syntax
    };
    syntax.retention.retain(Arc::new(charge));
    Ok(syntax)
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use hirsel_engine_admission::ProcessBudget;

    struct Allowance(Arc<ProcessBudget>);
    impl HeaderAllowance for Allowance {
        fn header_budget(&self) -> &Arc<ProcessBudget> {
            &self.0
        }
    }

    #[test]
    fn released_validation_demand_does_not_pin_its_capture() {
        let mut engine = QueryEngine::new(default_environment(), PhaseAdapters::default());
        let capture = engine.capture_namespace(&[], 1 << 20).unwrap();
        let headers = capture.snapshot.header_products().unwrap();
        let budget = ProcessBudget::new(1 << 19);
        let backing: Arc<dyn HeaderAllowance> = Arc::new(Allowance(budget.clone()));
        let _partition = headers.configure_source_backing(backing);
        let before = budget.free();
        let demand = Box::new(capture.demand_validation().unwrap());
        assert!(budget.free() < before);
        drop(demand);
        while headers.evict() {}
        assert_eq!(
            budget.free(),
            before,
            "an immutable capture alias must not retain a finished demand's reservation"
        );
    }

    #[test]
    fn idle_front_factory_does_not_pin_validation_demand() {
        use hirsel_engine_runtime::{MemoryWeight, RuntimeErrors};
        let mut engine = QueryEngine::new(default_environment(), PhaseAdapters::default())
            .with_result_store(Arc::new(hirsel_engine_store::MemStore::default()));
        let reference = |text: &[u8]| {
            hirsel_engine_store::Domain {
                kind: 0x4600_00ff,
                schema: 1,
            }
            .reference(text)
            .unwrap()
        };
        let budget = ProcessBudget::new(1 << 20);
        let front = engine
            .front_body_queries(
                &[],
                MemoryWeight {
                    retained: 0,
                    scratch: 1 << 19,
                    stack: 0,
                },
                &budget,
                RuntimeErrors {
                    resource: reference(b"resource"),
                    cycle: reference(b"cycle"),
                    cancelled: reference(b"cancelled"),
                    protocol: reference(b"protocol"),
                    panicked: reference(b"panicked"),
                },
            )
            .unwrap();
        let snapshot = front.signatures();
        let before = front.retained_bytes();
        let demand = snapshot.demand_validation().unwrap();
        assert!(
            front.retained_bytes() > before,
            "an idle factory must not already own an active validation demand"
        );
        drop(demand);
        while snapshot.header_products().unwrap().evict() {}
        assert_eq!(
            front.retained_bytes(),
            before,
            "a finished demand returns its reservation while its factory stays reusable"
        );
    }

    #[test]
    fn installed_closure_header_retention_is_bounded_by_demanded_rows() {
        use hirsel_validate::{
            Installed, InstalledViews, ValidationMemory, View, ViewSource, ViewTable,
        };
        struct Memory(Arc<HeaderProducts>);
        impl ValidationMemory for Memory {
            fn retain(
                &self,
                bytes: u64,
            ) -> Result<Arc<dyn Send + Sync>, hirsel_validate::ViewRefusal> {
                self.0.try_validation_lease(bytes).map_err(|cause| {
                    hirsel_validate::ViewRefusal::Unreadable {
                        definition: DefId::intern(&DefPath::core("validation-storage")),
                        cause,
                    }
                })
            }
            fn retain_read(
                &self,
                bytes: u64,
            ) -> Result<Arc<dyn Send + Sync>, hirsel_validate::ViewRefusal> {
                self.0.validation_demand_lease(bytes).map_err(|cause| {
                    hirsel_validate::ViewRefusal::Unreadable {
                        definition: DefId::intern(&DefPath::core("validation-storage")),
                        cause,
                    }
                })
            }
            fn retain_signature(
                &self,
                signature: &hirsel_types::SignatureRef,
            ) -> Result<Arc<dyn Send + Sync>, hirsel_validate::ViewRefusal> {
                self.0
                    .validation_signature_lease(signature)
                    .map_err(|cause| hirsel_validate::ViewRefusal::Unreadable {
                        definition: signature.definition,
                        cause,
                    })
            }
        }
        struct Source {
            types: Arc<hirsel_types::TypeInterner>,
            reads: Arc<AtomicU64>,
            retention: std::sync::OnceLock<Arc<dyn Send + Sync>>,
        }
        fn signature(types: &hirsel_types::TypeInterner, id: DefId) -> hirsel_types::SignatureRef {
            Arc::new(hirsel_types::Signature {
                metadata: Default::default(),
                definition: id,
                kind: hirsel_types::SignatureKind::Const {
                    ty: types.intern(hirsel_types::Type::unit()),
                    generics: Default::default(),
                },
                fingerprint: ContentHash::of_bytes(id.path().to_string().as_bytes()),
            })
        }
        impl ViewSource for Source {
            fn view(&self, id: DefId) -> Result<Option<View>, hirsel_validate::ViewRefusal> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                Ok(Some(View::new(
                    &self.types,
                    signature(&self.types, id),
                    None,
                )))
            }
            fn retain_control(
                &self,
                memory: &Arc<dyn ValidationMemory>,
            ) -> Result<(), hirsel_validate::ViewRefusal> {
                if self.retention.get().is_none() {
                    let _ = self.retention.set(memory.retain(size_of::<Self>() as u64)?);
                }
                Ok(())
            }
        }
        let mut bounded = true;
        for count in [64, 4096] {
            let mut engine = QueryEngine::new(default_environment(), PhaseAdapters::default());
            let types = engine.types.clone();
            let ids = (0..count)
                .map(|index| DefId::intern(&DefPath::core(&format!("installed_retention_{index}"))))
                .collect::<Vec<_>>();
            let summary = ViewTable::new(
                &types,
                ids.iter().map(|id| signature(&types, *id)),
                None,
                &mut hirsel_validate::ValidatedSignatures::default(),
                None,
            )
            .unwrap()
            .universe();
            let donor = DefId::intern(&DefPath::core("installed_pressure_donor"));
            let mut headers = HeaderProducts::capture(&engine);
            let mutable = Arc::get_mut(&mut headers).unwrap();
            mutable.paths = Arc::from([donor]);
            mutable.rows.insert(donor, HeaderSlot::default());
            let budget = ProcessBudget::new(16 << 20);
            let backing = Arc::new(Allowance(budget.clone()));
            let _partition = headers.configure_source_backing(backing.clone());
            let memory: Arc<dyn ValidationMemory> = Arc::new(Memory(headers.clone()));
            let reads = Arc::new(AtomicU64::new(0));
            let before = budget.free();
            let (table, allocation) = crate::test_alloc::measured(|| {
                let installed = Arc::new(
                    InstalledViews::new(
                        ids.iter().copied(),
                        Box::new(Source {
                            types: types.clone(),
                            reads: reads.clone(),
                            retention: std::sync::OnceLock::new(),
                        }),
                    )
                    .unwrap(),
                );
                let table = ViewTable::with_installed(
                    &types,
                    [],
                    Installed::new(installed, [summary]),
                    &mut hirsel_validate::ValidatedSignatures::default(),
                    None,
                )
                .unwrap();
                table.retain_control(&memory).unwrap();
                table
            });
            let charged = before - budget.free();
            // Membership needs one ID and one lazy pointer per definition. All
            // other unopened table/control storage fits an independent 64 KiB.
            let bound = 64 * 1024 + 16 * count as u64;
            println!(
                "installed closure={count} peak={} live={} charged={charged} bound={bound}",
                allocation.peak, allocation.live
            );
            assert_eq!(
                reads.load(Ordering::Relaxed),
                0,
                "unread headers remain unmaterialized"
            );
            assert!(
                allocation.live.max(0) as u64 <= charged,
                "retained storage must stay admitted"
            );
            bounded &= allocation.peak.max(0) as u64 <= bound && charged <= bound;
            let universe = table.universe();
            for _ in 0..2 {
                let read = table.get(ids[0]).unwrap().unwrap();
                assert_eq!(
                    read.digest(),
                    hirsel_validate::digest(&types, &signature(&types, ids[0]))
                );
            }
            assert_eq!(
                reads.load(Ordering::Relaxed),
                1,
                "only the demanded row gets a cache cell"
            );
            assert_eq!(
                table.universe(),
                universe,
                "cache construction preserves replay identity"
            );
            let donor_signature = signature(&types, donor);
            let entry = Arc::new(engine.record(
                MemoKey::new(QueryKind::Signature, donor),
                donor_signature.fingerprint,
                Ok(Arc::new(output(donor_signature))),
                DependencyReads::default(),
                &Observed::default(),
            ));
            headers.seed(donor, entry);
            assert!(headers.ready(donor).is_some());
            let pressure = backing.0.reserve(backing.0.free() - 128).unwrap();
            let demanded = table.get(ids[1]).unwrap().unwrap();
            assert_eq!(
                demanded.digest(),
                hirsel_validate::digest(&types, &signature(&types, ids[1]))
            );
            assert_eq!(table.universe(), universe);
            assert_eq!(reads.load(Ordering::Relaxed), 2);
            drop(pressure);
        }
        assert!(
            bounded,
            "unread installed view caches exceed the independent membership/control bound"
        );
    }

    #[test]
    fn validation_aliases_retain_one_read_signature_payload() {
        struct Memory(Arc<HeaderProducts>);
        impl hirsel_validate::ValidationMemory for Memory {
            fn retain(
                &self,
                bytes: u64,
            ) -> Result<Arc<dyn Send + Sync>, hirsel_validate::ViewRefusal> {
                self.0.validation_demand_lease(bytes).map_err(|cause| {
                    hirsel_validate::ViewRefusal::Unreadable {
                        definition: DefId::intern(&DefPath::core("validation-storage")),
                        cause,
                    }
                })
            }
            fn retain_signature(
                &self,
                signature: &hirsel_types::SignatureRef,
            ) -> Result<Arc<dyn Send + Sync>, hirsel_validate::ViewRefusal> {
                self.0
                    .validation_signature_lease(signature)
                    .map_err(|cause| hirsel_validate::ViewRefusal::Unreadable {
                        definition: signature.definition,
                        cause,
                    })
            }
        }
        let engine = QueryEngine::new(default_environment(), PhaseAdapters::default());
        let headers = HeaderProducts::capture(&engine);
        let budget = ProcessBudget::new(1 << 19);
        let backing: Arc<dyn HeaderAllowance> = Arc::new(Allowance(budget.clone()));
        headers.configure_source_backing(backing.clone());
        let memory = Memory(headers);
        let registry = hirsel_registry::BuiltinRegistry::shared();
        let shared = registry.signatures().next().unwrap().clone();
        let initial = budget.free();
        let shared_lease = memory.0.validation_signature_lease(&shared).unwrap();
        assert_eq!(
            budget.free(),
            initial,
            "aliasing a process-owned core payload allocates no request storage"
        );
        drop(shared_lease);
        let signature = Arc::new(hirsel_types::Signature {
            metadata: hirsel_types::SignatureMetadata {
                constant_alias: Some(Box::new(DefPath::core(&"a".repeat(8192)))),
                ..Default::default()
            },
            definition: shared.definition,
            kind: hirsel_types::SignatureKind::Const {
                ty: engine.types.intern(hirsel_types::Type::unit()),
                generics: Default::default(),
            },
            fingerprint: ContentHash::of_bytes(b"shared-payload"),
        });
        let first = hirsel_validate::View::new(&engine.types, signature.clone(), None);
        let second = hirsel_validate::View::new(&engine.types, signature, None);
        let initial = budget.free();
        first.identified().retain_control(&memory).unwrap();
        let after_first = budget.free();
        second.identified().retain_control(&memory).unwrap();
        let after_second = budget.free();
        assert!(
            after_first - after_second <= initial - after_first - 8192,
            "a second identity must not debit the shared 8192-byte signature field twice"
        );
        drop(first);
        assert!(budget.free() < initial);
        assert_eq!(
            second
                .signature()
                .unwrap()
                .metadata
                .constant_alias
                .as_ref()
                .unwrap()
                .0
                .name
                .len(),
            8192
        );
        drop(second);
        while memory.0.evict() {}
        assert_eq!(budget.free(), initial);
    }

    #[test]
    fn repeated_header_read_survives_pressure_while_a_cold_row_can_be_reclaimed() {
        let mut engine = QueryEngine::new(default_environment(), PhaseAdapters::default());
        let mut headers = HeaderProducts::capture(&engine);
        for name in ["first_header", "second_header", "third_header"] {
            Arc::get_mut(&mut headers)
                .unwrap()
                .rows
                .insert(DefId::intern(&DefPath::core(name)), HeaderSlot::default());
        }
        let ids: Vec<_> = headers.rows.keys().copied().collect();
        Arc::get_mut(&mut headers).unwrap().paths = ids.clone().into();
        let budget = ProcessBudget::new(1 << 20);
        let backing: Arc<dyn HeaderAllowance> = Arc::new(Allowance(budget.clone()));
        headers.configure_source_backing(backing.clone());
        for id in ids {
            let signature = Arc::new(hirsel_types::Signature {
                metadata: Default::default(),
                definition: id,
                kind: hirsel_types::SignatureKind::Const {
                    ty: engine.types.intern(hirsel_types::Type::unit()),
                    generics: Default::default(),
                },
                fingerprint: ContentHash::of_bytes(id.path().to_string().as_bytes()),
            });
            let entry = Arc::new(engine.record(
                MemoKey::new(QueryKind::Signature, id),
                signature.fingerprint,
                Ok(Arc::new(output(signature))),
                DependencyReads::default(),
                &Observed::default(),
            ));
            let bytes = crate::memo::retained::retained_signature_entry_bytes(&entry).unwrap()
                + hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                    HeaderProduct,
                >::uninit());
            let (charge, row_charge) = headers.admit_signature(Some(&entry), bytes).unwrap();
            headers.publish(
                &headers.rows[&id],
                HeaderProduct {
                    result: entry.result(),
                    entry: Some(entry),
                    recomputes: 0,
                    graph_work: 0,
                    charge,
                    _row_charge: row_charge,
                    origin: Weak::new(),
                    recent: AtomicBool::new(true),
                    accounted: AtomicBool::new(true),
                },
            );
        }
        assert!(headers.evict_row());
        let resident: Vec<_> = headers
            .rows
            .iter()
            .filter_map(|(id, slot)| slot.peek().map(|_| *id))
            .collect();
        assert_eq!(resident.len(), 2);
        let [hot, cold] = resident.as_slice() else {
            unreachable!()
        };
        drop(headers.ready(*hot).unwrap());
        assert!(headers.evict_row());
        assert!(
            headers.ready(*hot).is_some(),
            "a second read must protect the hot header while a cold victim exists"
        );
        assert!(
            headers.ready(*cold).is_none(),
            "pressure must still reclaim the cold header"
        );
    }

    #[test]
    fn metadata_pressure_reclaims_cache_wrappers_while_readers_retain_their_rows() {
        for retain_readers in [false, true] {
            let mut engine = QueryEngine::new(default_environment(), PhaseAdapters::default());
            let id = DefId::intern(&DefPath::core("leased_header"));
            let signature = Arc::new(hirsel_types::Signature {
                metadata: Default::default(),
                definition: id,
                kind: hirsel_types::SignatureKind::Const {
                    ty: engine.types.intern(hirsel_types::Type::unit()),
                    generics: Default::default(),
                },
                fingerprint: ContentHash::of_bytes(b"leased_header"),
            });
            let entry = Arc::new(engine.record(
                MemoKey::new(QueryKind::Signature, id),
                signature.fingerprint,
                Ok(Arc::new(output(signature.clone()))),
                DependencyReads::default(),
                &Observed::default(),
            ));
            let mut headers = HeaderProducts::capture(&engine);
            Arc::get_mut(&mut headers).unwrap().paths = Arc::from([id]);
            Arc::get_mut(&mut headers)
                .unwrap()
                .rows
                .insert(id, HeaderSlot::default());
            let budget = ProcessBudget::new(1 << 19);
            let backing: Arc<dyn HeaderAllowance> = Arc::new(Allowance(budget.clone()));
            headers.configure_source_backing(backing.clone());
            let bytes = crate::memo::retained::retained_signature_entry_bytes(&entry).unwrap()
                + hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                    HeaderProduct,
                >::uninit());
            let (charge, row_charge) = headers.admit_signature(Some(&entry), bytes).unwrap();
            let wrapper = row_charge.front_bytes();
            let payload = charge.front_bytes();
            headers.publish(
                &headers.rows[&id],
                HeaderProduct {
                    result: entry.result(),
                    entry: Some(entry.clone()),
                    recomputes: 0,
                    graph_work: 0,
                    charge,
                    _row_charge: row_charge,
                    origin: Weak::new(),
                    recent: AtomicBool::new(true),
                    accounted: AtomicBool::new(true),
                },
            );
            if !retain_readers {
                drop(entry);
                drop(engine);
                assert!(headers.evict_row());
                assert_eq!(
                    budget.free(),
                    (1 << 19) - payload,
                    "evicting the last row owner must return its metadata charge"
                );
                drop(signature);
                assert_eq!(budget.free(), 1 << 19);
                continue;
            }
            let raw = entry.result().unwrap();
            let before = budget.free();
            assert!(headers.evict_row());
            assert!(headers.ready(id).is_none());
            assert_eq!(budget.free() - before, wrapper);
            assert_eq!(raw.value.fingerprint, signature.fingerprint);
            assert!(payload > 0);
            assert!(budget.free() < 1 << 19);
            drop(raw);
            drop(entry);
            drop(engine);
            assert!(headers.reclaim_header_rows());
            assert_eq!(budget.free(), (1 << 19) - payload);
            drop(signature);
            assert_eq!(budget.free(), 1 << 19);
        }
    }
}

/// The traced work that fills `id`'s row: one node per definition per run.
fn header_entry(id: DefId) -> hirsel_intern::attribution::Filled {
    hirsel_intern::attribution::Filled::keyed(hirsel_intern::attribution::layer::HEADER_PRODUCT, id)
}

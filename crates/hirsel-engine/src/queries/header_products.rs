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
pub(crate) use sources::{CapturedText, SourceInputs};

type SignatureOutput = Result<Arc<QueryOutput<hirsel_types::SignatureRef>>, QueryError>;
pub(crate) struct HeaderProduct {
    pub(crate) result: SignatureOutput,
    pub(crate) entry: Option<Arc<Entry<hirsel_types::SignatureRef>>>,
    pub(crate) recomputes: usize,
    graph_work: usize,
    publication: bool,
    charge: Charge,
    origin: Weak<Entry<hirsel_types::SignatureRef>>,
    accounted: AtomicBool,
}
pub(crate) trait HeaderAllowance: Send + Sync {
    fn take_header(&self, bytes: u64) -> Option<crate::runtime::MemoryReservation>;
}
/// What one settled header row or module proof holds against a budget.
enum HeaderCharge {
    /// Uncharged, like every engine-retained signature; admission never
    /// evicts it.
    Engine,
    /// Charged to the shared standalone source-inference budget.
    Standalone(
        #[expect(dead_code, reason = "held for its drop")] crate::runtime::MemoryReservation,
    ),
    /// Owned by shared syntax until its last source owner drops. It may use
    /// a Front allowance, but is not a row of the portable Front facts cache.
    Source(#[expect(dead_code, reason = "held for its drop")] crate::runtime::MemoryReservation),
    /// Lent by a live Front partition; evictable while unpinned.
    Front(FrontCharge),
}
/// A charge a Front partition lent. Dropping it returns the bytes to the
/// partition's grant and the reservation to its allowance, exactly once.
struct FrontCharge {
    partition: Weak<dyn HeaderAllowance>,
    credit: Arc<std::sync::Mutex<u64>>,
    bytes: u64,
    #[expect(dead_code, reason = "held for its drop")]
    allowance: crate::runtime::MemoryReservation,
}
impl Drop for FrontCharge {
    fn drop(&mut self) {
        *self.credit.lock().unwrap_or_else(|e| e.into_inner()) += self.bytes;
    }
}
/// The charge cell of one row or proof. The partition that lent a `Front`
/// charge keeps a weak handle to the cell and turns it to `Engine` when the
/// partition ends.
struct Charge(Arc<std::sync::Mutex<HeaderCharge>>);
impl Charge {
    fn engine() -> Self {
        Self(Arc::new(std::sync::Mutex::new(HeaderCharge::Engine)))
    }
    /// Admission may evict only what a live partition lent.
    fn evictable(&self) -> bool {
        matches!(
            &*self.0.lock().unwrap_or_else(|e| e.into_inner()),
            HeaderCharge::Front(charge) if charge.partition.strong_count() != 0
        )
    }
}
type ValidationCaches =
    Arc<std::sync::Mutex<Vec<(Weak<crate::snapshot::ValidationState>, Arc<dyn Send + Sync>)>>>;
#[derive(Clone)]
struct HeaderAdmission {
    validation_caches: ValidationCaches,
    remaining: Arc<std::sync::Mutex<u64>>,
    partition: Weak<dyn HeaderAllowance>,
    /// Every charge cell this partition lent, rows and proofs alike.
    lent: Arc<std::sync::Mutex<Vec<Weak<std::sync::Mutex<HeaderCharge>>>>>,
}
impl HeaderAdmission {
    fn lend(&self, charge: FrontCharge) -> Charge {
        let cell = Arc::new(std::sync::Mutex::new(HeaderCharge::Front(charge)));
        let mut lent = self.lent.lock().unwrap_or_else(|e| e.into_inner());
        if lent.len() == lent.capacity() {
            lent.retain(|cell| cell.strong_count() != 0);
        }
        lent.push(Arc::downgrade(&cell));
        Charge(cell)
    }
    /// The partition ended: every charge it lent returns now, and the entry
    /// stays as an uncharged engine row whose later eviction credits nothing.
    fn release(&self) {
        let lent = std::mem::take(&mut *self.lent.lock().unwrap_or_else(|e| e.into_inner()));
        for cell in lent.iter().filter_map(Weak::upgrade) {
            let charge = std::mem::replace(
                &mut *cell.lock().unwrap_or_else(|e| e.into_inner()),
                HeaderCharge::Engine,
            );
            drop(charge);
        }
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
        let mut slot = self.product.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(resident) = slot.as_ref() {
            return (resident.clone(), false);
        }
        self.work
            .fetch_add(product.graph_work as u64, Ordering::Relaxed);
        let product = Arc::new(product);
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
    rows: hirsel_syntax::DefMap<HeaderSlot>,
    generation: AtomicU64,
    /// Bumped by every row this table publishes after construction. An engine
    /// adopts the completed rows once per revision, not once per read.
    revision: AtomicU64,
    admission: std::sync::Mutex<Option<HeaderAdmission>>,
    validation_caches: ValidationCaches,
    environment: QueryEnvironment,
    adapters: HeaderPhases,
    sources: SourceMap,
    owners: Arc<crate::NativeRecordContext>,
    provisional: bool,
    standalone: bool,
    failure: Option<QueryError>,
    syntax: Arc<HeaderSources>,
    private: Option<private::PrivateProducts>,
    private_working: AtomicU64,
    private_registrations: OnceLock<Vec<Arc<AcceptedFrontRegistration>>>,
    declarations: crate::DeclarationIndex,
    catalog: catalog::Catalog,
    installed_sources: BTreeSet<FileId>,
    types: Arc<hirsel_types::TypeInterner>,
    memo: Arc<hirsel_types::TypeMemo>,
    builtins: BTreeMap<DefPath, hirsel_types::SignatureRef>,
    registry: crate::registry::BuiltinRegistry,
    package_manifests: BTreeMap<hirsel_syntax::PackageId, ContentHash>,
    package_sources: BTreeMap<hirsel_syntax::PackageId, ContentHash>,
    published_manifest: Option<ContentHash>,
    store: Option<Arc<dyn crate::store::ResultStore>>,
    identity: ContentHash,
    verify_store: crate::store::VerifyStore,
    verify: Verify,
    limits: crate::ComptimeLimits,
    staged: BTreeMap<DefPath, crate::comptime::StagedConstant>,
    summaries: Arc<hirsel_syntax::DefMap<hirsel_types::UniqueSummary>>,
    /// The reads each carried summary's module proof recorded: a module
    /// producer validates them like any previous proof.
    summary_reads: Arc<hirsel_syntax::DefMap<Arc<crate::memo::Reads>>>,
    failures: BTreeMap<DefId, Vec<hirsel_types::UniqueFailure>>,
}
impl HeaderProducts {
    pub(crate) fn needs_written_signatures(&self) -> bool {
        self.adapters.signature.is_none()
    }
    pub(crate) fn staged_constants(&self) -> &BTreeMap<DefPath, crate::comptime::StagedConstant> {
        &self.staged
    }
    pub(crate) fn capture(engine: &QueryEngine) -> Arc<Self> {
        let private = private::PrivateProducts::capture(engine);
        let declarations = engine.declarations.frozen();
        let rows = engine
            .source_paths()
            .into_iter()
            .map(|path| {
                let id = DefId::intern(&path);
                let cell = HeaderSlot::default();
                if engine.unmarked_signature(id).is_some()
                    && private
                        .as_ref()
                        .is_none_or(|private| !private.requires_proof(id))
                {
                    let entry = engine.signatures[&id].clone();
                    let _ = cell.set(HeaderProduct {
                        result: entry.result(),
                        entry: Some(project(&entry)),
                        recomputes: 0,
                        graph_work: 0,
                        publication: engine.private_signature_publications.contains(&id),
                        charge: Charge::engine(),
                        origin: Arc::downgrade(&entry),
                        accounted: AtomicBool::new(true),
                    });
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
                        (facts.body || facts.generator) && !facts.constant && !facts.instance_bodied
                    })
            })
            .map(|(id, summary)| (*id, summary.clone()))
            .collect();
        Arc::new(Self {
            rows,
            generation: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            admission: std::sync::Mutex::new(None),
            validation_caches: Default::default(),
            environment: engine.environment.clone(),
            adapters: HeaderPhases::capture(&engine.adapters),
            sources: engine.sources.clone(),
            owners: owners.clone(),
            provisional: engine.private_summary_active,
            standalone: !engine.private_inference_hosted && !engine.private_summary_active,
            failure: engine.current_private_summary_failure().cloned(),
            syntax: Arc::new(HeaderSources::capture(engine)),
            private,
            private_working: AtomicU64::new(0),
            private_registrations: Default::default(),
            declarations: engine.declarations.header_inputs(),
            catalog: engine
                .catalog
                .source_carry(&engine.sources, &engine.items, &engine.indexed_sources)
                .restore(),
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
        })
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
        let (product, settled) = cell.set(product);
        if settled {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        (product, settled)
    }
    pub(crate) fn source_facts(&self, id: DefId) -> Option<catalog::SourceFacts> {
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
        HeaderSources::facts_index_bytes(engine.source_paths().len())
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
        self.rows.keys().copied()
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
        self.admission
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
        let entry = source.map(project);
        // Never wait while holding a recursive header stack. Concurrent pure
        // normalizations race only to publish the same immutable source product.
        let recomputes = engine.recompute_count(id.path(), QueryKind::Signature);
        let bytes = entry
            .as_ref()
            .and_then(|entry| crate::memo::retained::retained_signature_entry_bytes(entry))
            .unwrap_or(0)
            .saturating_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                HeaderProduct,
            >::uninit()));
        let (charge, transient) = match self.admit(bytes) {
            Ok(charge) => (charge, false),
            Err(_)
                if working.as_ref().is_some_and(|(_, scratch)| {
                    bytes
                        .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                            std::sync::Mutex<HeaderCharge>,
                        >::uninit(
                        )))
                        .is_some_and(|required| required <= *scratch)
                }) =>
            {
                (Charge::engine(), true)
            }
            Err(error) => return Err(error),
        };
        let product = HeaderProduct {
            result,
            entry,
            recomputes,
            graph_work: if local { 0 } else { recomputes },
            publication: self.provisional,
            charge,
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
            engine.signatures.insert(id, entry.clone());
            engine.adopted_headers = None;
        }
        product.result.clone()
    }
    /// Reserve syntax against the existing source allowance without debiting
    /// the Front metadata grant. The file owner retains this reservation even
    /// after the partition ends; its aliases never reserve the payload again.
    fn admit_source(&self, bytes: u64) -> Result<Charge, QueryError> {
        let bytes = bytes
            .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                std::sync::Mutex<HeaderCharge>,
            >::uninit()))
            .ok_or_else(|| sources::source_refusal("syntax size overflow"))?;
        let partition = self
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
        Ok(Charge(Arc::new(std::sync::Mutex::new(
            HeaderCharge::Source(allowance),
        ))))
    }
    pub(crate) fn validation_lease(&self, bytes: u64) -> Result<Arc<dyn Send + Sync>, QueryError> {
        Ok(Arc::new(
            self.admit(
                bytes
                    .checked_add((size_of::<Charge>() + 2 * size_of::<usize>()) as u64)
                    .ok_or_else(|| sources::source_refusal("validation size overflow"))?,
            )?,
        ))
    }
    /// Charge one settled row or proof to the front's header grant, evicting
    /// unpinned entries first. A refusal is a resource failure, never a verdict.
    fn admit(&self, bytes: u64) -> Result<Charge, QueryError> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(admission) = admission else {
            return Ok(if self.standalone {
                Charge(Arc::new(std::sync::Mutex::new(HeaderCharge::Standalone(
                    self.reserve_standalone(bytes)?,
                ))))
            } else {
                Charge::engine()
            });
        };
        // An ended Front returns its lent charges and leaves this table with
        // the engine. Later rows and proofs have that same engine owner; the
        // ended source pool cannot lend another reservation.
        let Some(partition) = admission.partition.upgrade() else {
            admission.release();
            return Ok(Charge::engine());
        };
        // The charge cell itself is part of what the partition retains.
        let bytes =
            bytes.saturating_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                std::sync::Mutex<HeaderCharge>,
            >::uninit()));
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
            if !self.evict() {
                return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "header admission".into(),
                    message: format!("header source allowance cannot retain {bytes} bytes"),
                }));
            }
        };
        Ok(admission.lend(FrontCharge {
            partition: admission.partition.clone(),
            credit: admission.remaining.clone(),
            bytes,
            allowance,
        }))
    }
    /// Retain a payload a module proof settled, when the header grant admits it.
    fn seed(&self, id: DefId, entry: Arc<Entry<hirsel_types::SignatureRef>>) {
        let Some(cell) = self.rows.get(&id) else {
            return;
        };
        if cell.get().is_some() {
            return;
        }
        let Some(bytes) = crate::memo::retained::retained_signature_entry_bytes(&entry) else {
            return;
        };
        let bytes =
            bytes.saturating_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                HeaderProduct,
            >::uninit()));
        let Ok(charge) = self.admit(bytes) else {
            return;
        };
        self.publish(
            cell,
            HeaderProduct {
                result: entry.result(),
                entry: Some(entry),
                recomputes: 0,
                graph_work: 0,
                publication: false,
                charge,
                origin: Weak::new(),
                accounted: AtomicBool::new(true),
            },
        );
    }
    pub(crate) fn ready(&self, id: DefId) -> Option<Arc<HeaderProduct>> {
        self.rows.get(&id)?.get()
    }
    pub(crate) fn configure_source_backing(
        &self,
        backing: Arc<dyn HeaderAllowance>,
        remaining: Arc<std::sync::Mutex<u64>>,
    ) -> Arc<dyn HeaderAllowance> {
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(partition) = admission
            .as_ref()
            .and_then(|admission| admission.partition.upgrade())
        {
            return partition;
        }
        if let Some(ended) = admission.take() {
            ended.release();
        }
        *admission = Some(HeaderAdmission {
            validation_caches: self.validation_caches.clone(),
            remaining,
            partition: Arc::downgrade(&backing),
            lent: Arc::default(),
        });
        backing
    }
    /// Headers a later snapshot of the same request captured are charged to
    /// the Front partition, and prove private modules in its working space,
    /// exactly as the Front producer's own headers are.
    pub(crate) fn inherit_snapshot_scope(&self, front: &Self, inputs: &[[ContentHash; 3]]) {
        let shared = front
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if admission.is_none() {
            *admission = shared;
        }
        drop(admission);
        self.inherit_private_snapshot(front, inputs);
    }
    pub(crate) fn inherit_registration_scope(
        &self,
        previous: &Self,
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) {
        let shared = previous
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if admission.is_none() {
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
        // Retain the charge until a dead weak owner is pruned.
        let bytes = 4 * size_of::<(
            std::sync::Weak<crate::snapshot::ValidationState>,
            Arc<dyn Send + Sync>,
        )>() + 128;
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
        caches.retain(|(state, _)| state.strong_count() != 0);
        if caches.capacity() > 4 * caches.len().max(1) {
            caches.shrink_to_fit();
        }
        caches.push((Arc::downgrade(state), lease));
        Ok(())
    }
    fn validation_cache_registry(&self) -> ValidationCaches {
        self.admission
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
        caches.retain(|(state, _)| state.strong_count() != 0);
        for (state, _) in caches.iter() {
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
        self.evict_validation_cache() || self.evict_row() || self.evict_proof()
    }
    fn evict_row(&self) -> bool {
        for cell in self.rows.values() {
            let mut slot = cell.product.lock().unwrap_or_else(|e| e.into_inner());
            let Some(product) = slot.as_ref() else {
                continue;
            };
            if Arc::strong_count(product) != 1 || !product.charge.evictable() {
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
            // The row and its Entry share the output. Scoped raw signatures,
            // validation views and active normalizers pin the exact payload.
            if Arc::strong_count(output) != 2 || Arc::strong_count(&output.value) != 1 {
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

            *slot = None;
            return true;
        }
        false
    }
    /// A Front partition ended: no charge it lent, to a row or a proof,
    /// outlives it. The entries stay as engine-retained signatures did.
    pub(crate) fn release_ended_partition(&self) {
        let admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ended) = admission
            .as_ref()
            .filter(|admission| admission.partition.strong_count() == 0)
        {
            ended.release();
        }
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
    pub(crate) fn inherit_demand_header_scope(&self, headers: &header_products::HeaderProducts) {
        if self.private_bootstrap_active
            && let Some(previous) = self
                .snapshot
                .as_ref()
                .and_then(|(_, snapshot)| snapshot.header_products())
        {
            headers.inherit_snapshot_scope(previous, &self.private_summary_inputs);
        }
    }
    pub(crate) fn adopt_completed_headers(&mut self) {
        let Some((stamp, snapshot)) = &self.snapshot else {
            return;
        };
        let current = *stamp == self.snapshot_stamp();
        let Some(headers) = snapshot.header_products().cloned() else {
            return;
        };
        let snapshot = snapshot.clone();
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
            self.snapshot = Some((self.snapshot_stamp(), snapshot));
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
        if self.signatures.get(&id).is_some_and(|current| {
            Arc::ptr_eq(current, entry)
                || product
                    .origin
                    .upgrade()
                    .is_some_and(|origin| Arc::ptr_eq(current, &origin))
        }) {
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

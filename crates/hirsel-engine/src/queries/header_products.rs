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
pub use private::ModuleProof;
pub(crate) use sources::CapturedText;
use sources::HeaderSources;

type SignatureOutput = Result<Arc<QueryOutput<hirsel_types::SignatureRef>>, QueryError>;
pub(crate) struct HeaderProduct {
    pub(crate) result: SignatureOutput,
    pub(crate) entry: Option<Arc<Entry<hirsel_types::SignatureRef>>>,
    pub(crate) recomputes: usize,
    graph_work: usize,
    publication: bool,
    _standalone: Option<Arc<crate::runtime::MemoryReservation>>,
    _lease: Option<HeaderLease>,
    origin: Weak<Entry<hirsel_types::SignatureRef>>,
    accounted: AtomicBool,
}
pub(crate) trait HeaderAllowance: Send + Sync {
    fn take_header(&self, bytes: u64) -> Option<crate::runtime::MemoryReservation>;
}
#[derive(Clone)]
struct HeaderAdmission {
    remaining: Arc<std::sync::Mutex<u64>>,
    source: Weak<dyn HeaderAllowance>,
}
struct HeaderLease {
    remaining: Arc<std::sync::Mutex<u64>>,
    bytes: u64,
    _allowance: Option<crate::runtime::MemoryReservation>,
}
impl Drop for HeaderLease {
    fn drop(&mut self) {
        *self.remaining.lock().unwrap_or_else(|e| e.into_inner()) += self.bytes;
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
    fn set(&self, product: HeaderProduct) -> Result<(), HeaderProduct> {
        let mut slot = self.product.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err(product);
        }
        self.work
            .fetch_add(product.graph_work as u64, Ordering::Relaxed);
        *slot = Some(Arc::new(product));
        Ok(())
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
    admission: std::sync::Mutex<Option<HeaderAdmission>>,
    environment: QueryEnvironment,
    adapters: HeaderPhases,
    sources: SourceMap,
    owners: Arc<crate::NativeRecordContext>,
    provisional: bool,
    standalone: bool,
    failure: Option<QueryError>,
    /// A proof reader that cannot return an error records its refusal here;
    /// every later header demand of this snapshot fails with it.
    refusal: OnceLock<QueryError>,
    syntax: Arc<HeaderSources>,
    private: Option<private::PrivateProducts>,
    declarations: crate::DeclarationIndex,
    catalog: catalog::Catalog,
    types: Arc<hirsel_types::TypeInterner>,
    memo: Arc<hirsel_types::TypeMemo>,
    builtins: BTreeMap<DefPath, hirsel_types::SignatureRef>,
    registry: crate::registry::BuiltinRegistry,
    package_manifests: BTreeMap<hirsel_syntax::PackageId, ContentHash>,
    published_manifest: Option<ContentHash>,
    store: Option<Arc<dyn crate::store::ResultStore>>,
    identity: ContentHash,
    verify_store: crate::store::VerifyStore,
    limits: crate::ComptimeLimits,
    staged: BTreeMap<DefPath, crate::comptime::StagedConstant>,
    summaries: Arc<hirsel_syntax::DefMap<hirsel_types::UniqueSummary>>,
    /// The reads each carried summary's module proof recorded: a module
    /// producer validates them like any previous proof.
    summary_reads: Arc<hirsel_syntax::DefMap<Arc<crate::memo::Reads>>>,
    failures: BTreeMap<DefId, Vec<hirsel_types::UniqueFailure>>,
}
impl HeaderProducts {
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
                        _standalone: None,
                        _lease: None,
                        origin: Arc::downgrade(&entry),
                        accounted: AtomicBool::new(true),
                    });
                }
                (id, cell)
            })
            .collect();
        let owners = Arc::new(crate::NativeRecordContext::capture(engine));
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
            admission: std::sync::Mutex::new(None),
            environment: engine.environment.clone(),
            adapters: HeaderPhases::capture(&engine.adapters),
            sources: engine.sources.clone(),
            owners: owners.clone(),
            provisional: engine.private_summary_active,
            standalone: !engine.private_inference_hosted && !engine.private_summary_active,
            failure: engine.current_private_summary_failure().cloned(),
            refusal: OnceLock::new(),
            syntax: engine
                .header_products
                .as_ref()
                .map(|headers| headers.syntax.clone())
                .unwrap_or_else(|| Arc::new(HeaderSources::capture(engine))),
            private,
            declarations: engine.declarations.header_inputs(),
            catalog: engine.catalog.clone(),
            types: engine.types.clone(),
            memo: engine.memo.clone(),
            builtins: engine.builtins.clone(),
            registry: engine.registry.clone(),
            package_manifests: engine.package_manifests.clone(),
            published_manifest: engine.published_manifest,
            store: engine.result_store.clone(),
            identity: engine.store_identity,
            verify_store: engine.verify_store,
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
            && self.environment == engine.environment
            && self.limits == engine.comptime_limits
            && self.published_manifest == engine.published_manifest
            && Arc::ptr_eq(&self.types, &engine.types)
            && engine.private_signature_publications.is_empty())
        .then(|| self.clone())
    }
    fn execution(self: &Arc<Self>) -> QueryEngine {
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
        engine
            .catalog
            .bind_ownership(engine.source_ownership.clone());
        engine.types = self.types.clone();
        engine.memo = self.memo.clone();
        engine.builtins = self.builtins.clone();
        engine.registry = self.registry.clone();
        engine.package_manifests = self.package_manifests.clone();
        engine.published_manifest = self.published_manifest;
        engine.result_store = self.store.clone();
        engine.store_identity = self.identity;
        engine.verify_store = self.verify_store;
        engine.comptime_limits = self.limits;
        engine.staged_constants = self.staged.clone();
        engine.header_products = Some(self.clone());
        engine
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    pub(crate) fn source_facts(&self, id: DefId) -> Option<catalog::SourceFacts> {
        self.syntax.facts(id)
    }
    pub(crate) fn captured_text(&self, id: DefId) -> Option<sources::CapturedText> {
        self.syntax.text(id)
    }
    pub(crate) fn source_fact_index_bytes(engine: &QueryEngine, private: bool) -> u64 {
        HeaderSources::facts_index_bytes(engine.source_paths().len()).saturating_add(if private {
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
    /// Freeze input transport under the front host's existing source grant.
    /// Legacy scalar views keep their written immutable inputs; a disposable
    /// result provider never becomes their required source-input owner.
    pub(crate) fn archive_sources(
        &self,
        store: Arc<dyn crate::store::ResultStore>,
    ) -> Result<(), QueryError> {
        self.syntax
            .archive(self.types.clone(), self.owners.clone(), store)
    }
    pub(super) fn summary(&self, id: DefId) -> Option<hirsel_types::UniqueSummary> {
        if self.private.is_some() {
            self.demanded_summary(id)
        } else {
            self.summaries.get(&id).cloned()
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
    pub(crate) fn product(self: &Arc<Self>, id: DefId) -> Option<Arc<HeaderProduct>> {
        let cell = self.rows.get(&id)?;
        if cell.get().is_none() {
            let mut engine = self.execution();
            if let Err(error) = self.demand(&mut engine, id) {
                let _ = cell.set(HeaderProduct {
                    result: Err(error),
                    entry: None,
                    recomputes: 0,
                    graph_work: 0,
                    publication: false,
                    _standalone: None,
                    _lease: None,
                    origin: Weak::new(),
                    accounted: AtomicBool::new(false),
                });
            }
        }
        cell.get()
    }
    fn refuse(&self, error: QueryError) {
        let _ = self.refusal.set(error);
    }
    fn refused(&self) -> Result<(), QueryError> {
        self.refusal
            .get()
            .map_or(Ok(()), |error| Err(error.clone()))
    }
    fn admitted(&self) -> bool {
        self.admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    pub(crate) fn demand(self: &Arc<Self>, engine: &mut QueryEngine, id: DefId) -> SignatureOutput {
        self.refused()?;
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
        // The held proof pins this module while its header is produced.
        let mut proof = self.private_product(id)?;
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
        let result = engine.signature_query_local(id);
        // A proof read inside the producer was refused: its result may carry a
        // conservative summary, so it is never published.
        self.refused()?;
        if let Some(stale) = proof.take().filter(|proof| {
            proof
                .identities
                .get(&id)
                .is_some_and(|expected| *expected != private::identity(&result))
        }) {
            // The recomputed payload disagrees with the proof that named it.
            // Recompute the proof once; a second disagreement is a fault.
            self.invalidate_private(id, &stale);
            let fresh = self.private_product(id)?;
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
        let (standalone, lease) = self.admit(bytes)?;
        if cell
            .set(HeaderProduct {
                result,
                entry,
                recomputes,
                graph_work: if local { 0 } else { recomputes },
                publication: self.provisional,
                _standalone: standalone,
                _lease: lease,
                origin,
                accounted: AtomicBool::new(local),
            })
            .is_ok()
        {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        cell.get()
            .expect("a completed producer publishes its row")
            .result
            .clone()
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
        }
        product.result.clone()
    }
    /// Charge one settled row to the front's header grant, evicting unpinned
    /// rows first. A refusal is a resource failure, never a row verdict.
    fn admit(
        &self,
        bytes: u64,
    ) -> Result<
        (
            Option<Arc<crate::runtime::MemoryReservation>>,
            Option<HeaderLease>,
        ),
        QueryError,
    > {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        while admission.as_ref().is_some_and(|admission| {
            *admission
                .remaining
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                < bytes
        }) {
            if !self.evict() {
                break;
            }
        }
        let standalone = (self.standalone && admission.is_none())
            .then(|| self.reserve_standalone(bytes))
            .transpose()?;
        let mut grant = admission.as_ref().map(|admission| {
            admission
                .remaining
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        });
        let header_allowance = admission
            .as_ref()
            .map(|admission| {
                admission
                    .source
                    .upgrade()
                    .and_then(|source| source.take_header(bytes))
                    .ok_or_else(|| {
                        QueryError::Operational(hirsel_types::OperationalFailure {
                            operation: "header admission".into(),
                            message: format!("header source allowance cannot retain {bytes} bytes"),
                        })
                    })
            })
            .transpose()?;
        if let Some(grant) = &mut grant {
            **grant = grant.checked_sub(bytes).ok_or_else(|| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "header admission".into(),
                    message: format!("demanded header exceeds the existing front partition; required {bytes} bytes, available {} bytes", **grant),
                })
            })?;
        }
        let lease = admission.as_ref().map(|admission| HeaderLease {
            remaining: admission.remaining.clone(),
            bytes,
            _allowance: header_allowance,
        });
        drop(grant);
        Ok((standalone, lease))
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
        let Ok((standalone, lease)) = self.admit(bytes) else {
            return;
        };
        let _ = cell.set(HeaderProduct {
            result: entry.result(),
            entry: Some(entry),
            recomputes: 0,
            graph_work: 0,
            publication: false,
            _standalone: standalone,
            _lease: lease,
            origin: Weak::new(),
            accounted: AtomicBool::new(true),
        });
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
        if let Some(source) = admission
            .as_ref()
            .and_then(|admission| admission.source.upgrade())
        {
            return source;
        }
        *admission = Some(HeaderAdmission {
            remaining,
            source: Arc::downgrade(&backing),
        });
        backing
    }
    /// Evict one unpinned settled entry: a header row, else a module proof.
    pub(crate) fn evict(&self) -> bool {
        self.evict_row() || self.evict_proof()
    }
    fn evict_row(&self) -> bool {
        for cell in self.rows.values() {
            let mut slot = cell.product.lock().unwrap_or_else(|e| e.into_inner());
            let Some(product) = slot.as_ref() else {
                continue;
            };
            if Arc::strong_count(product) != 1 || product._lease.is_none() {
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

impl QueryEngine {
    pub(crate) fn declared_header_inputs(&self) -> Option<Arc<HeaderProducts>> {
        self.header_products
            .as_ref()
            .and_then(|headers| headers.declared_inputs(self))
    }
    pub(crate) fn adopt_completed_headers(&mut self) {
        let Some((stamp, snapshot)) = &self.snapshot else {
            return;
        };
        let current = *stamp == self.snapshot_stamp();
        let Some(headers) = snapshot.header_products().cloned() else {
            return;
        };
        if current {
            let _ = headers.adopt_private_products(self);
            self.adopt_header_products(&headers);
        } else {
            for (id, product) in headers.completed() {
                self.account_header_work(id, &product);
            }
        }
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
            signature_persisted: true,
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

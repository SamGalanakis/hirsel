use super::*;
use crate::queries::private_unique::scheduled::Facts;
use std::sync::{Mutex, atomic::AtomicU64};
type Module = (crate::memo::PackageKey, crate::memo::ModuleKey);
pub(super) struct ModuleProduct {
    facts: Facts,
    complete: bool,
    work: BTreeMap<(DefId, QueryKind), usize>,
    /// The identity of each settled signature this proof covers. Payloads
    /// live only in the admitted header rows; a miss recomputes there.
    pub(super) identities: hirsel_syntax::DefMap<ContentHash>,
    accounted: AtomicBool,
    _standalone: Option<Arc<crate::runtime::MemoryReservation>>,
}
/// One module's proof attempt. A stale proof is replaced, never patched.
type ProofCell = Arc<OnceLock<Result<Arc<ModuleProduct>, QueryError>>>;
struct Candidate {
    module: Module,
    declared: Option<Arc<Entry<hirsel_types::SignatureRef>>>,
    publication: bool,
}
pub(super) struct PrivateProducts {
    candidates: hirsel_syntax::DefMap<Candidate>,
    modules: BTreeMap<Module, Mutex<ProofCell>>,
    working: AtomicU64,
    registrations: OnceLock<Vec<Arc<AcceptedFrontRegistration>>>,
    retention: OnceLock<Arc<Mutex<u64>>>,
}
impl PrivateProducts {
    pub(super) fn index_bytes(engine: &QueryEngine) -> u64 {
        let paths = Self::candidate_paths(engine);
        let modules = paths.iter().map(module_of).collect::<BTreeSet<_>>().len();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + 4 * size_of::<usize>());
        weight.tree::<DefId, Candidate>(paths.len());
        weight.tree::<Module, Mutex<ProofCell>>(modules);
        weight.add(
            modules
                * (size_of::<OnceLock<Result<Arc<ModuleProduct>, QueryError>>>()
                    + 2 * size_of::<usize>()),
        );
        weight.bytes()
    }
    fn candidate_paths(engine: &QueryEngine) -> Vec<DefPath> {
        let declarations = engine.declarations.frozen();
        engine
            .source_paths()
            .into_iter()
            .filter(|path| {
                declarations
                    .metadata()
                    .definitions
                    .get(path)
                    .is_some_and(|m| !m.public)
                    && engine.source_facts(path).is_some_and(|f| {
                        (f.body || f.generator) && !f.constant && !f.instance_bodied
                    })
            })
            .collect()
    }
    pub(super) fn capture(engine: &QueryEngine) -> Option<Self> {
        if engine.private_summary_active
            || engine.private_summary_revision == Some(engine.graph.revision())
            || engine
                .adapters
                .modes
                .as_ref()
                .is_none_or(|m| !m.infers_private_unique())
        {
            return None;
        }
        let candidates = Self::candidate_paths(engine)
            .into_iter()
            .map(|path| {
                let module = module_of(&path);
                let id = DefId::intern(&path);
                let declared = engine
                    .unmarked_signature(id)
                    .and_then(|_| engine.signatures.get(&id))
                    .map(project);
                (
                    id,
                    Candidate {
                        module,
                        declared,
                        publication: engine.private_signature_publications.contains(&id),
                    },
                )
            })
            .collect::<hirsel_syntax::DefMap<_>>();
        let modules = candidates
            .values()
            .map(|candidate| (candidate.module, Mutex::default()))
            .collect();
        Some(Self {
            candidates,
            modules,
            working: AtomicU64::new(0),
            registrations: OnceLock::new(),
            retention: OnceLock::new(),
        })
    }
    pub(super) fn requires_proof(&self, id: DefId) -> bool {
        self.candidates.contains_key(&id)
    }
    fn product(&self, id: DefId) -> Option<Arc<ModuleProduct>> {
        proof(self.modules.get(&self.candidates.get(&id)?.module)?)
    }
    fn products(
        &self,
    ) -> impl Iterator<Item = (&Module, Option<Result<Arc<ModuleProduct>, QueryError>>)> {
        self.modules.iter().map(|(module, slot)| {
            let cell = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
            (module, cell.get().cloned())
        })
    }
}
fn module_of(path: &DefPath) -> Module {
    (
        crate::memo::PackageKey::of(&path.0.package),
        crate::memo::ModuleKey::of(&path.0.modules),
    )
}
fn proof(slot: &Mutex<ProofCell>) -> Option<Arc<ModuleProduct>> {
    let cell = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    cell.get()?.as_ref().ok().cloned()
}
/// The value a signature read observes: what equal-output cutoff and
/// invalidation compare.
pub(super) fn identity(result: &SignatureOutput) -> ContentHash {
    memos::signature_value(result.as_ref().ok().map(|output| output.value.fingerprint))
}
impl HeaderProducts {
    pub(crate) fn configure_private(
        &self,
        working: u64,
        registrations: &[Arc<AcceptedFrontRegistration>],
        retention: Arc<Mutex<u64>>,
    ) {
        if let Some(private) = &self.private {
            private.working.store(working, Ordering::Relaxed);
            let _ = private.registrations.set(registrations.to_vec());
            let _ = private.retention.set(retention);
        }
    }
    pub(super) fn private_product(
        self: &Arc<Self>,
        id: DefId,
    ) -> Result<Option<Arc<ModuleProduct>>, QueryError> {
        let Some(private) = &self.private else {
            return Ok(None);
        };
        let Some(candidate) = private.candidates.get(&id) else {
            return Ok(None);
        };
        let module = candidate.module;
        let cell = private.modules[&module]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut settled = Vec::new();
        let product = cell
            .get_or_init(|| {
                if let Some(error) = &self.failure {
                    return Err(error.clone());
                }
                let working = private.working.load(Ordering::Relaxed);
                if working == 0 {
                    return Err(private_refusal());
                }
                // This fixed point reads declared headers only. It cannot demand
                // another settled module while its own publication is in progress.
                let declared = self.declared();
                let mut engine = declared.execution();
                // The module's previous proof, with its reads and failures, is
                // revalidated by the ordinary previous-facts check.
                for (id, summary) in self.summaries.iter().filter(|(id, _)| {
                    private
                        .candidates
                        .get(id)
                        .is_some_and(|candidate| candidate.module == module)
                }) {
                    engine.private_summaries.insert(*id, summary.clone());
                    if let Some(reads) = self.summary_reads.get(id) {
                        engine.private_summary_reads.insert(*id, reads.clone());
                    }
                    if let Some(failures) = self.failures.get(id) {
                        engine.private_unique_failures.insert(*id, failures.clone());
                    }
                }
                let (facts, complete) = engine.infer_private_module(
                    working,
                    private.registrations.get().map_or(&[], Vec::as_slice),
                    id,
                )?;
                let mut weight = crate::NativeMetadataWeight::default();
                weight.add(
                    size_of::<ModuleProduct>()
                        + facts.proofs.capacity()
                            * size_of::<(DefId, hirsel_types::UniqueInference)>(),
                );
                for (_, bytes) in facts.reads.allocations() {
                    weight.add(bytes);
                }
                for (_, proof) in &facts.proofs {
                    weight.flat_vec(&proof.summary.preserves_unique);
                    weight.flat_vec(&proof.summary.results);
                    weight.flat_vec(&proof.summary.nonretaining);
                    weight.vec(&proof.failures, |weight, failure| {
                        weight.add(failure.message.capacity())
                    });
                }
                let signatures: Vec<_> = engine
                    .signatures
                    .iter()
                    .filter(|(definition, entry)| {
                        complete
                            && self.store.as_ref().is_some_and(|store| store.enabled())
                            && private
                                .candidates
                                .get(definition)
                                .is_some_and(|candidate| candidate.module == module)
                            && engine.unmarked_signature(**definition).is_some()
                            && entry.reads().iter().all(|(read, _)| match read {
                                ReadKey::PrivateSummary(dependency)
                                | ReadKey::Signature(dependency)
                                | ReadKey::ExecutedSignature(dependency)
                                | ReadKey::FailedSignature(dependency) => private
                                    .candidates
                                    .get(dependency)
                                    .is_none_or(|candidate| candidate.module == module),
                                _ => true,
                            })
                    })
                    .map(|(definition, entry)| (*definition, project(entry)))
                    .collect();
                let identities: hirsel_syntax::DefMap<_> = signatures
                    .iter()
                    .map(|(definition, entry)| (*definition, identity(&entry.result())))
                    .collect();
                weight.tree::<DefId, ContentHash>(identities.len());
                let work: BTreeMap<_, _> = engine
                    .recomputes
                    .iter()
                    .map(|((path, kind), count)| ((DefId::intern(path), *kind), *count))
                    .collect();
                weight.tree::<(DefId, QueryKind), usize>(work.len());
                let standalone = if self.standalone && private.retention.get().is_none() {
                    Some(self.reserve_standalone(weight.bytes())?)
                } else if let Some(retention) = private.retention.get() {
                    // Settled header rows share this partition; evict unpinned
                    // rows before refusing, never while holding the grant.
                    while *retention.lock().unwrap_or_else(|e| e.into_inner()) < weight.bytes()
                        && self.evict()
                    {}
                    let mut remaining = retention.lock().unwrap_or_else(|e| e.into_inner());
                    *remaining = remaining.checked_sub(weight.bytes()).ok_or_else(|| {
                        QueryError::Operational(hirsel_types::OperationalFailure {
                            operation: "private inference admission".into(),
                            message: format!("demanded module proof exceeds its existing front partition; owner {}; required {} bytes, available {} bytes", id.path(), weight.bytes(), *remaining),
                        })
                    })?;
                    None
                } else {
                    return Err(private_refusal());
                };
                self.generation.fetch_add(1, Ordering::Relaxed);
                settled = signatures;
                Ok(Arc::new(ModuleProduct {
                    facts,
                    complete,
                    work,
                    identities,
                    accounted: AtomicBool::new(false),
                    _standalone: standalone,
                }))
            })
            .clone()?;
        // Offer the payloads to the admitted header rows outside the proof's
        // initialization. A row that is not admitted recomputes on demand.
        for (definition, entry) in settled {
            self.seed(definition, entry);
        }
        Ok(Some(product))
    }
    /// Replace a proof whose identity disagrees with its recomputed payload.
    pub(super) fn invalidate_private(&self, id: DefId, stale: &Arc<ModuleProduct>) {
        let Some(private) = &self.private else {
            return;
        };
        let Some(candidate) = private.candidates.get(&id) else {
            return;
        };
        let mut slot = private.modules[&candidate.module]
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot
            .get()
            .and_then(|result| result.as_ref().ok())
            .is_some_and(|product| Arc::ptr_eq(product, stale))
        {
            *slot = ProofCell::default();
        }
    }
    pub(super) fn demanded_summary(&self, id: DefId) -> Option<hirsel_types::UniqueSummary> {
        self.private
            .as_ref()?
            .product(id)?
            .facts
            .proofs
            .iter()
            .find(|(definition, _)| *definition == id)
            .map(|(_, proof)| proof.summary.clone())
    }
    pub(crate) fn unique_failure(
        &self,
        id: DefId,
        claim: hirsel_types::UniqueClaim,
    ) -> Option<hirsel_types::UniqueFailure> {
        let product = self.private.as_ref()?.product(id)?;
        let proof = product
            .facts
            .proofs
            .iter()
            .find(|(definition, _)| *definition == id)?;
        proof.1.failures.iter().find(|f| f.claim == claim).cloned()
    }
    pub(crate) fn adopt_private_products(
        &self,
        engine: &mut QueryEngine,
    ) -> Result<(), QueryError> {
        let Some(private) = &self.private else {
            return Ok(());
        };
        let mut inferred = engine
            .private_summaries
            .iter()
            .map(|(id, summary)| {
                (
                    *id,
                    hirsel_types::UniqueInference {
                        summary: summary.clone(),
                        failures: engine
                            .private_unique_failures
                            .get(id)
                            .cloned()
                            .unwrap_or_default(),
                    },
                )
            })
            .collect::<hirsel_syntax::DefMap<_>>();
        inferred.retain(|id, _| private.candidates.contains_key(id));
        engine
            .private_summary_reads
            .retain(|id, _| private.candidates.contains_key(id));
        let mut changed = inferred.len() != engine.private_summaries.len();
        let products: Vec<_> = private.products().collect();
        for (module, product) in &products {
            let Some(Ok(product)) = product else {
                continue;
            };
            for (id, candidate) in &private.candidates {
                if candidate.module == **module {
                    inferred.remove(id);
                    engine.private_summary_reads.remove(id);
                }
            }
            let reads = Arc::new(product.facts.reads.clone());
            for (id, proof) in &product.facts.proofs {
                engine.private_summary_reads.insert(*id, reads.clone());
                inferred.insert(*id, proof.clone());
            }
            if !product.accounted.swap(true, Ordering::Relaxed) {
                for ((id, kind), count) in &product.work {
                    *engine
                        .recomputes
                        .entry((id.path().clone(), *kind))
                        .or_default() += count;
                }
            }
            changed = true;
        }
        if changed {
            engine.publish_private_summaries(inferred)?;
            if products.iter().all(|(_, result)| {
                result
                    .as_ref()
                    .is_some_and(|result| result.as_ref().is_ok_and(|product| product.complete))
            }) {
                engine.private_summary_revision = Some(engine.graph.revision());
            }
        }
        Ok(())
    }
    pub(in crate::queries) fn private_work(&self, work: &mut Vec<(DefPath, QueryKind, u64)>) {
        if let Some(private) = &self.private {
            for (_, product) in private.products() {
                let Some(Ok(product)) = product else {
                    continue;
                };
                work.extend(
                    product
                        .work
                        .iter()
                        .map(|((id, kind), count)| (id.path().clone(), *kind, *count as u64)),
                );
            }
        }
    }
    pub(super) fn reserve_standalone(
        &self,
        bytes: u64,
    ) -> Result<Arc<crate::runtime::MemoryReservation>, QueryError> {
        // Legacy scalar inputs already use this shared source-inference partition.
        // The admitted front grant covers its task/output rows, not those inputs.
        let bytes = bytes
            .checked_add(
                (size_of::<crate::runtime::MemoryReservation>() + 2 * size_of::<usize>()) as u64,
            )
            .ok_or_else(private_refusal)?;
        crate::queries::private_unique::scheduled::standalone_budget()
            .reserve(bytes)
            .map(Arc::new)
            .map_err(|error| QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "private inference admission".into(),
                message: format!("demanded module proof exceeds its existing front partition; standalone header owner: {error}"),
            }))
    }
    fn declared(&self) -> Arc<Self> {
        Arc::new(Self {
            rows: self
                .rows
                .iter()
                .map(|(id, final_cell)| {
                    let cell = HeaderSlot::default();
                    let entry = final_cell
                        .get()
                        .and_then(|product| {
                            product
                                .entry
                                .as_ref()
                                .map(|entry| (entry.clone(), product.publication))
                        })
                        .or_else(|| {
                            let candidate = self.private.as_ref()?.candidates.get(id)?;
                            Some((candidate.declared.as_ref()?.clone(), candidate.publication))
                        });
                    if let Some((entry, publication)) = entry {
                        let _ = cell.set(HeaderProduct {
                            result: entry.result(),
                            entry: Some(entry.clone()),
                            recomputes: 0,
                            graph_work: 0,
                            publication,
                            _standalone: None,
                            _lease: None,
                            origin: Weak::new(),
                            accounted: AtomicBool::new(true),
                        });
                    }
                    (*id, cell)
                })
                .collect(),
            generation: AtomicU64::new(0),
            admission: Mutex::new(None),
            environment: self.environment.clone(),
            adapters: self.adapters.clone(),
            sources: self.sources.clone(),
            owners: self.owners.clone(),
            provisional: true,
            standalone: false,
            failure: None,
            syntax: self.syntax.clone(),
            declarations: self.declarations.clone(),
            catalog: self.catalog.clone(),
            types: self.types.clone(),
            memo: self.memo.clone(),
            builtins: self.builtins.clone(),
            registry: self.registry.clone(),
            package_manifests: self.package_manifests.clone(),
            published_manifest: self.published_manifest,
            store: self.store.clone(),
            identity: self.identity,
            verify_store: self.verify_store,
            limits: self.limits,
            staged: self.staged.clone(),
            summaries: self.summaries.clone(),
            summary_reads: self.summary_reads.clone(),
            failures: Default::default(),
            private: None,
        })
    }
}
fn private_refusal() -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "private inference admission".into(),
        message: "demanded module proof exceeds its existing front partition".into(),
    })
}

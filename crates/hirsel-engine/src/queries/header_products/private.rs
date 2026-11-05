use super::*;
use crate::queries::private_unique::scheduled::Facts;
use std::sync::{Mutex, atomic::AtomicU64};
type Module = (crate::memo::PackageKey, crate::memo::ModuleKey);
/// The immutable proof owner and loader inputs whose facts are already adopted.
pub(crate) struct AdoptedPrivate {
    owner: std::sync::Weak<HeaderProducts>,
    revision: u64,
    inputs: u64,
    graph: crate::memo::Revision,
}
pub(super) struct ModuleProduct {
    facts: Facts,
    complete: bool,
    work: BTreeMap<(DefId, QueryKind), usize>,
    /// The identity of each settled signature this proof covers. Payloads
    /// live only in the admitted header rows; a miss recomputes there.
    pub(super) identities: hirsel_syntax::DefMap<ContentHash>,
    accounted: AtomicBool,
    /// Charged like a header row; evicting the proof returns it.
    charge: Charge,
}
/// One module's proof. A settled proof is an evictable entry of the admitted
/// front cache: it is pinned only while a consumer holds its Arc, and an
/// evicted proof recomputes through the ordinary producer on its next demand.
/// A resource refusal is never recorded here.
#[derive(Default)]
enum ProofState {
    #[default]
    Unproven,
    Settled(Arc<ModuleProduct>),
    Evicted,
    Failed(QueryError),
}
#[derive(Default)]
struct ModuleSlot {
    state: Mutex<ProofState>,
    /// Serializes one module's producer; never held by a reader or evictor.
    producing: Mutex<()>,
    /// How many times this module's proof was inferred, evictions included.
    inferences: AtomicU64,
}
impl ModuleSlot {
    fn current(&self) -> Option<Result<Arc<ModuleProduct>, QueryError>> {
        match &*self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            ProofState::Settled(product) => Some(Ok(product.clone())),
            ProofState::Failed(error) => Some(Err(error.clone())),
            ProofState::Unproven | ProofState::Evicted => None,
        }
    }
    fn settled_once(&self) -> bool {
        !matches!(
            *self.state.lock().unwrap_or_else(|e| e.into_inner()),
            ProofState::Unproven
        )
    }
}
/// A scoped view of one settled module proof. Holding it pins the proof.
pub struct ModuleProof(Arc<ModuleProduct>);
impl ModuleProof {
    /// The proved definitions with their inferred summaries and failures,
    /// and the identity of each settled signature the proof covers.
    pub fn settled(
        &self,
    ) -> (
        Vec<(DefPath, hirsel_types::UniqueInference)>,
        Vec<(DefPath, ContentHash)>,
    ) {
        let mut proofs: Vec<_> = self
            .0
            .facts
            .proofs
            .iter()
            .map(|(id, proof)| (id.path().clone(), proof.clone()))
            .collect();
        proofs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut identities: Vec<_> = self
            .0
            .identities
            .iter()
            .map(|(id, identity)| (id.path().clone(), *identity))
            .collect();
        identities.sort();
        (proofs, identities)
    }
}
struct Candidate {
    module: Module,
    declared: Option<Arc<Entry<hirsel_types::SignatureRef>>>,
    publication: bool,
}
pub(super) struct PrivateProducts {
    revision: AtomicU64,
    candidates: hirsel_syntax::DefMap<Candidate>,
    modules: BTreeMap<Module, ModuleSlot>,
}
impl PrivateProducts {
    pub(super) fn trace(&self) -> (usize, u64, usize) {
        let mut totals = (0, 0, 0);
        for slot in self.modules.values() {
            if let Some(Ok(product)) = slot.current()
                && let Some(cell) = &product.charge.0
            {
                let charge = cell.lock().unwrap_or_else(|e| e.into_inner());
                if let HeaderCharge::Front(charge) = &*charge {
                    totals.0 += 1;
                    totals.1 += charge.bytes;
                    totals.2 += usize::from(Arc::strong_count(&product) > 2);
                }
            }
        }
        totals
    }
    fn publish(&self, slot: &ModuleSlot, next: ProofState) {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = next;
        self.revision.fetch_add(1, Ordering::Release);
    }
    pub(super) fn index_bytes(engine: &QueryEngine) -> u64 {
        let paths = Self::candidate_paths(engine);
        let modules = paths.iter().map(module_of).collect::<BTreeSet<_>>().len();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + 4 * size_of::<usize>());
        weight.tree::<DefId, Candidate>(paths.len());
        weight.tree::<Module, ModuleSlot>(modules);
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
            .map(|candidate| (candidate.module, ModuleSlot::default()))
            .collect();
        Some(Self {
            candidates,
            modules,
            revision: AtomicU64::new(0),
        })
    }
    pub(super) fn requires_proof(&self, id: DefId) -> bool {
        self.candidates.contains_key(&id)
    }
    fn module(&self, id: DefId) -> Option<(Module, &ModuleSlot)> {
        let module = self.candidates.get(&id)?.module;
        Some((module, &self.modules[&module]))
    }
    /// A representative candidate whose demand produces this module's proof.
    fn demand_of(&self, module: &Module) -> Option<DefId> {
        self.candidates
            .iter()
            .find(|(_, candidate)| candidate.module == *module)
            .map(|(id, _)| *id)
    }
    /// Evict one settled proof no consumer holds.
    fn evict(&self) -> bool {
        for slot in self.modules.values() {
            let evicted = {
                let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                match &*state {
                    ProofState::Settled(product)
                        if Arc::strong_count(product) == 1 && product.charge.evictable() =>
                    {
                        let evicted = std::mem::replace(&mut *state, ProofState::Evicted);
                        self.revision.fetch_add(1, Ordering::Release);
                        evicted
                    }
                    _ => continue,
                }
            };
            // Release the charge outside the slot lock.
            drop(evicted);
            return true;
        }
        false
    }
}
fn module_of(path: &DefPath) -> Module {
    (
        crate::memo::PackageKey::of(&path.0.package),
        crate::memo::ModuleKey::of(&path.0.modules),
    )
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
    ) {
        let inputs = private_unique::registration_inputs(registrations);
        let previous = self.scope();
        let scope = if previous.inputs == inputs
            && previous.working.load(Ordering::Relaxed) == working
            && previous.registrations.len() == registrations.len()
            && previous
                .registrations
                .iter()
                .zip(registrations)
                .all(|(old, current)| old.ptr_eq(&Arc::downgrade(current)))
        {
            previous
        } else {
            Arc::new(PrivateInferenceScope {
                inputs,
                working: AtomicU64::new(working),
                registrations: registrations.iter().map(Arc::downgrade).collect(),
                admission: Mutex::new(
                    previous
                        .admission
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone(),
                ),
            })
        };
        *self.scope.lock().unwrap_or_else(|e| e.into_inner()) = scope.clone();
        let mut scopes = self
            .operation
            .inference
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !scopes.iter().any(|old| Arc::ptr_eq(old, &scope)) {
            scopes.push(scope);
        }
    }
    pub(super) fn inherit_private_snapshot(&self, front: &Self, inputs: &[[ContentHash; 3]]) {
        let previous = front.scope();
        if previous.inputs.as_ref() == inputs {
            *self.scope.lock().unwrap_or_else(|e| e.into_inner()) = previous;
        }
    }
    pub(super) fn inherit_private_scope(
        &self,
        previous: &Self,
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) {
        self.configure_private(
            previous.scope().working.load(Ordering::Relaxed),
            registrations,
        );
    }
    /// The proof of the module declaring `id`, demanded again if evicted.
    pub(crate) fn module_proof(&self, id: DefId) -> Result<Option<ModuleProof>, QueryError> {
        Ok(self.private_product(id)?.map(ModuleProof))
    }
    pub(crate) fn module_proof_resident(&self, id: DefId) -> bool {
        self.private
            .as_ref()
            .and_then(|private| private.module(id))
            .is_some_and(|(_, slot)| matches!(slot.current(), Some(Ok(_))))
    }
    /// How many times each module's proof was inferred, evictions included,
    /// by a representative candidate of the module.
    pub(crate) fn module_inferences(&self) -> Vec<(DefPath, u64)> {
        let Some(private) = &self.private else {
            return vec![];
        };
        private
            .modules
            .iter()
            .filter_map(|(module, slot)| {
                Some((
                    private.demand_of(module)?.path().clone(),
                    slot.inferences.load(Ordering::Relaxed),
                ))
            })
            .collect()
    }
    /// Evict one unpinned settled module proof.
    pub(super) fn evict_proof(&self) -> bool {
        self.private.as_ref().is_some_and(PrivateProducts::evict)
    }
    pub(super) fn private_product(
        &self,
        id: DefId,
    ) -> Result<Option<Arc<ModuleProduct>>, QueryError> {
        let Some(private) = &self.private else {
            return Ok(None);
        };
        let Some((module, slot)) = private.module(id) else {
            return Ok(None);
        };
        if let Some(product) = slot.current() {
            return product.map(Some);
        }
        let producing = slot.producing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(product) = slot.current() {
            return product.map(Some);
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let refusal = |reason: &str| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "private inference admission".into(),
                message: format!(
                    "demanded module proof exceeds its existing front partition; owner {}; {reason}",
                    id.path()
                ),
            })
        };
        let scope = self.scope();
        let working = scope.working.load(Ordering::Relaxed);
        if working == 0 {
            return Err(refusal("private working partition is not configured"));
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
        // Counted before inference: a refused proof was still inferred.
        slot.inferences.fetch_add(1, Ordering::Relaxed);
        let registrations = scope
            .registrations
            .iter()
            .map(Weak::upgrade)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| refusal("accepted inference input lease has ended"))?;
        let (facts, complete) = match engine.infer_private_module(working, &registrations, id) {
            Ok(result) => result,
            Err(error @ QueryError::Operational(_)) => return Err(error),
            Err(error) => {
                private.publish(slot, ProofState::Failed(error.clone()));
                return Err(error);
            }
        };
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            size_of::<ModuleProduct>()
                + facts.proofs.capacity() * size_of::<(DefId, hirsel_types::UniqueInference)>(),
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
                    && self.store.is_some()
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
        drop(engine);
        drop(declared);
        // The proof is charged like any settled header row: unpinned rows and
        // proofs are evicted first, and a refusal stays a resource failure.
        if !self.standalone && !self.admitted() {
            return Err(refusal("module proof has no admitted source owner"));
        }
        let charge = self.admit(weight.bytes()).map_err(|error| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "private inference admission".into(),
                message: format!(
                    "demanded module proof exceeds its existing front partition; owner {}; required {} bytes; {}",
                    id.path(),
                    weight.bytes(),
                    match error {
                        QueryError::Operational(failure) => failure.message,
                        other => format!("{other:?}"),
                    }
                ),
            })
        })?;
        let product = Arc::new(ModuleProduct {
            facts,
            complete,
            work,
            identities,
            accounted: AtomicBool::new(false),
            charge,
        });
        private.publish(slot, ProofState::Settled(product.clone()));
        drop(producing);
        self.generation.fetch_add(1, Ordering::Relaxed);
        // Offer the payloads to the admitted header rows outside the proof's
        // production. A row that is not admitted recomputes on demand.
        for (definition, entry) in signatures {
            self.seed(definition, entry);
        }
        Ok(Some(product))
    }
    /// The settled proof of a module that has settled before, recomputed if
    /// it was evicted. `None` only for a module no demand has proved yet.
    fn settled_product(&self, id: DefId) -> Option<Result<Arc<ModuleProduct>, QueryError>> {
        let (_, slot) = self.private.as_ref()?.module(id)?;
        if !slot.settled_once() {
            return None;
        }
        self.private_product(id).transpose()
    }
    /// Replace a proof whose identity disagrees with its recomputed payload.
    pub(super) fn invalidate_private(&self, id: DefId, stale: &Arc<ModuleProduct>) {
        let Some(private) = &self.private else {
            return;
        };
        let Some((_, slot)) = private.module(id) else {
            return;
        };
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state, ProofState::Settled(product) if Arc::ptr_eq(product, stale)) {
            *state = ProofState::Evicted;
            private.revision.fetch_add(1, Ordering::Release);
        }
    }
    /// A proof reader. An evicted proof is demanded again. A failed proof is
    /// its module's verdict and has no summary; a resource refusal is Err,
    /// never a conservative verdict.
    fn proof_for_reader(&self, id: DefId) -> Result<Option<Arc<ModuleProduct>>, QueryError> {
        match self.settled_product(id) {
            None => Ok(None),
            Some(Ok(product)) => Ok(Some(product)),
            Some(Err(error @ QueryError::Operational(_))) => Err(error),
            Some(Err(_)) => Ok(None),
        }
    }
    pub(super) fn demanded_summary(
        &self,
        id: DefId,
    ) -> Result<Option<hirsel_types::UniqueSummary>, QueryError> {
        Ok(self.proof_for_reader(id)?.and_then(|product| {
            product
                .facts
                .proofs
                .iter()
                .find(|(definition, _)| *definition == id)
                .map(|(_, proof)| proof.summary.clone())
        }))
    }
    /// The cited failure of `id`'s proof for `claim`. A refused proof read is
    /// Err: the citing body must not be kept, never a body that cites nothing.
    pub(crate) fn unique_failure(
        &self,
        id: DefId,
        claim: hirsel_types::UniqueClaim,
    ) -> Result<Option<hirsel_types::UniqueFailure>, QueryError> {
        let Some(product) = self.proof_for_reader(id)? else {
            return Ok(None);
        };
        Ok(product
            .facts
            .proofs
            .iter()
            .find(|(definition, _)| *definition == id)
            .and_then(|proof| proof.1.failures.iter().find(|f| f.claim == claim).cloned()))
    }
    pub(crate) fn adopt_private_products(
        self: &Arc<Self>,
        engine: &mut QueryEngine,
    ) -> Result<(), QueryError> {
        let Some(private) = &self.private else {
            return Ok(());
        };
        let revision = private.revision.load(Ordering::Acquire);
        if engine.adopted_private.as_ref().is_some_and(|adopted| {
            adopted.owner.ptr_eq(&Arc::downgrade(self))
                && adopted.revision == revision
                && adopted.inputs == engine.snapshot_inputs_revision
                && adopted.graph == engine.graph.revision()
        }) {
            return Ok(());
        }
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
        let mut complete = true;
        // One proof is pinned at a time: an evicted proof recomputes here
        // and may evict the proof adopted before it.
        for module in private.modules.keys() {
            let Some(product) = private
                .demand_of(module)
                .and_then(|id| self.settled_product(id))
            else {
                complete = false;
                continue;
            };
            let product = match product {
                Ok(product) => product,
                Err(error @ QueryError::Operational(_)) => return Err(error),
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            complete &= product.complete;
            for (id, candidate) in &private.candidates {
                if candidate.module == *module {
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
            if complete {
                engine.private_summary_revision = Some(engine.graph.revision());
            }
        }
        engine.adopted_private = Some(AdoptedPrivate {
            owner: Arc::downgrade(self),
            revision,
            inputs: engine.snapshot_inputs_revision,
            graph: engine.graph.revision(),
        });
        Ok(())
    }
    pub(in crate::queries) fn private_work(&self, work: &mut Vec<(DefPath, QueryKind, u64)>) {
        if let Some(private) = &self.private {
            for slot in private.modules.values() {
                let Some(Ok(product)) = slot.current() else {
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
    ) -> Result<crate::runtime::MemoryReservation, QueryError> {
        // Legacy scalar inputs already use this shared source-inference partition.
        // The admitted front grant covers its task/output rows, not those inputs.
        let bytes = bytes
            .checked_add(
                (size_of::<crate::runtime::MemoryReservation>() + 2 * size_of::<usize>()) as u64,
            )
            .ok_or_else(private_refusal)?;
        crate::queries::private_unique::scheduled::standalone_budget()
            .reserve(bytes)
            .map_err(|error| QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "private inference admission".into(),
                message: format!("demanded module proof exceeds its existing front partition; standalone header owner: {error}"),
            }))
    }
    fn declared(&self) -> Arc<Self> {
        let rows: hirsel_syntax::DefMap<HeaderSlot> = self
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
                        charge: Charge::engine(),
                        _row_charge: Charge::engine(),
                        origin: Weak::new(),
                        accounted: AtomicBool::new(true),
                    });
                }
                (*id, cell)
            })
            .collect();
        Arc::new(Self {
            operation: self.operation.clone(),
            rows,
            generation: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            scope: Mutex::new(self.scope()),
            validation_caches: Default::default(),
            environment: self.environment.clone(),
            adapters: self.adapters.clone(),
            sources: self.sources.clone(),
            owners: self.owners.clone(),
            provisional: true,
            standalone: false,
            failure: None,
            syntax: self.syntax.clone(),
            inputs: self.inputs.clone(),
            declarations: self.declarations.clone(),
            catalog: self.catalog.clone(),
            installed_sources: self.installed_sources.clone(),
            types: self.types.clone(),
            memo: self.memo.clone(),
            builtins: self.builtins.clone(),
            registry: self.registry.clone(),
            package_manifests: self.package_manifests.clone(),
            package_sources: self.package_sources.clone(),
            published_manifest: self.published_manifest,
            store: self.store.clone(),
            identity: self.identity,
            verify_store: self.verify_store,
            verify: self.verify,
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

#[cfg(test)]
mod adoption_tests {
    use super::*;
    fn fixture() -> (Arc<HeaderProducts>, QueryEngine, DefId) {
        let engine = QueryEngine::new(default_environment(), PhaseAdapters::default());
        let mut headers = HeaderProducts::capture(&engine);
        let id = DefId::intern(&crate::tools::entry_path(&crate::tools::default_context()));
        let module = module_of(id.path());
        let slot = ModuleSlot::default();
        *slot.state.lock().unwrap() = ProofState::Settled(Arc::new(ModuleProduct {
            facts: Facts {
                proofs: vec![(
                    id,
                    hirsel_types::UniqueInference {
                        summary: Default::default(),
                        failures: vec![],
                    },
                )],
                reads: Default::default(),
            },
            complete: true,
            work: Default::default(),
            identities: Default::default(),
            accounted: AtomicBool::new(false),
            charge: Charge::engine(),
        }));
        Arc::get_mut(&mut headers).unwrap().private = Some(PrivateProducts {
            candidates: [(
                id,
                Candidate {
                    module,
                    declared: None,
                    publication: false,
                },
            )]
            .into_iter()
            .collect(),
            modules: [(module, slot)].into(),
            revision: AtomicU64::new(0),
        });
        (headers, engine, id)
    }
    #[test]
    fn unchanged_private_adoption_keeps_its_accepted_read_owner() {
        let (headers, mut engine, id) = fixture();
        headers.adopt_private_products(&mut engine).unwrap();
        let accepted = engine.private_summary_reads[&id].clone();
        for _ in 0..32 {
            headers.adopt_private_products(&mut engine).unwrap();
        }
        assert!(
            Arc::ptr_eq(&accepted, &engine.private_summary_reads[&id]),
            "unchanged proof reads must be shared, not rebuilt on every header lookup"
        );
        assert!(engine.private_summaries.contains_key(&id));
    }
    #[test]
    fn a_new_proof_revision_refreshes_the_same_engines_reads() {
        let (headers, mut engine, id) = fixture();
        headers.adopt_private_products(&mut engine).unwrap();
        let accepted = engine.private_summary_reads[&id].clone();
        let private = headers.private.as_ref().unwrap();
        let (_, slot) = private.module(id).unwrap();
        let previous = slot.current().unwrap().unwrap();
        let mut facts = previous.facts.clone();
        let mut reads = ReadsBuilder::new();
        let value = ContentHash::of_bytes(b"changed private inputs");
        reads.record(ReadKey::Environment, value);
        facts.reads = reads.finish().unwrap();
        private.publish(
            slot,
            ProofState::Settled(Arc::new(ModuleProduct {
                facts,
                complete: true,
                work: Default::default(),
                identities: Default::default(),
                accounted: AtomicBool::new(false),
                charge: Charge::engine(),
            })),
        );
        headers.adopt_private_products(&mut engine).unwrap();
        assert!(!Arc::ptr_eq(&accepted, &engine.private_summary_reads[&id]));
        assert!(
            engine.private_summary_reads[&id]
                .iter()
                .any(|&(key, observed)| key == ReadKey::Environment && observed == value)
        );
    }
    #[test]
    fn another_header_owner_never_reuses_a_private_adoption_stamp() {
        let (first, mut engine, id) = fixture();
        first.adopt_private_products(&mut engine).unwrap();
        let accepted = engine.private_summary_reads[&id].clone();
        let (second, _, _) = fixture();
        second.adopt_private_products(&mut engine).unwrap();
        assert!(!Arc::ptr_eq(&accepted, &engine.private_summary_reads[&id]));
        assert!(engine.private_summaries.contains_key(&id));
    }
    #[test]
    fn replacing_engine_facts_requires_adopting_the_proof_again() {
        let (headers, mut engine, id) = fixture();
        headers.adopt_private_products(&mut engine).unwrap();
        engine
            .publish_private_summaries(Default::default())
            .unwrap();
        assert!(!engine.private_summaries.contains_key(&id));
        headers.adopt_private_products(&mut engine).unwrap();
        assert!(engine.private_summaries.contains_key(&id));
    }
    #[test]
    fn a_changed_graph_revision_refreshes_private_completion() {
        let (headers, mut engine, _) = fixture();
        headers.adopt_private_products(&mut engine).unwrap();
        let previous = engine.private_summary_revision;
        let mut changes = crate::memo::ChangeSet::default();
        changes.read(ReadKey::Environment, crate::memo::Durability::High);
        engine.apply_changes(&changes);
        assert_ne!(previous, Some(engine.graph.revision()));
        headers.adopt_private_products(&mut engine).unwrap();
        assert_eq!(
            engine.private_summary_revision,
            Some(engine.graph.revision())
        );
    }
}

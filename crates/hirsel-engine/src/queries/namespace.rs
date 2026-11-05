//! Immutable inputs of a source transaction. Consumers retain this capture;
//! they never refresh its declarations, instance bindings or source owners.
use super::*;
use crate::store::{Domain, EntryKey, Reference};
use crate::{NativeRecordContext, NativeRecords};
mod reopen;

const GENERATION: Domain = Domain {
    kind: 0x4601_0008,
    schema: 1,
};
const MANIFEST: Domain = Domain {
    kind: 0x4601_0009,
    schema: 1,
};

#[derive(PartialEq)]
pub(crate) struct BodyKeys {
    pub implementation: ContentHash,
    pub site: Span,
    pub modes: EntryKey,
    pub lower: EntryKey,
    pub evaluate: EntryKey,
}
#[derive(PartialEq)]
pub(crate) struct BodyInput {
    pub syntax: ContentHash,
    pub implementation: ContentHash,
    pub executable: bool,
    pub generated: bool,
    pub queries: Option<Arc<BodyKeys>>,
}
pub(crate) struct HeaderInput {
    pub output: Arc<QueryOutput<hirsel_types::SignatureRef>>,
    pub reads: Option<Arc<Reads>>,
    pub semantic: Vec<DefId>,
}

/// A capture owns observations and portable bindings, never verifier authority.
/// Successor captures are distinct values, including when they share maps.
pub struct NamespaceCapture {
    pub(crate) snapshot: Arc<SignatureSnapshot>,
    inputs: Arc<header_products::SourceInputs>,
    pub(crate) declarations: Arc<crate::snapshot::Declarations>,
    pub(crate) source_paths: Box<[DefId]>,
    pub(crate) roots: Box<[DefId]>,
    pub(crate) sources: SourceMap,
    pub(crate) anchors: Arc<NativeRecordContext>,
    store: Arc<dyn crate::store::ResultStore>,
    pub(crate) instances: Arc<hirsel_tir::InstanceBodies>,
    pub(crate) bodies: operation_inputs::InputIndex<DefId, Arc<BodyInput>>,
    pub(crate) registrations: BTreeMap<DefId, Arc<AcceptedFrontRegistration>>,
    private_sources: BTreeMap<(DefId, u32), hirsel_span::Anchor>,
    pub(crate) records: crate::NativeRecords,
    pub(crate) limits: crate::ComptimeLimits,
    environment: ContentHash,
    verify: Verify,
    pub(crate) manifest: Reference,
    manifest_inputs: Manifest,
    identity: ContentHash,
    retained: u64,
    admission: std::sync::OnceLock<NamespaceAdmission>,
    capture_inputs: CaptureInputs,
    pub(crate) operation: Arc<operation_inputs::OperationOwner>,
}

/// A retained holder's lease over exact operation inputs and header bindings.
/// It has no reader-completion action. Dropping the final lease releases the
/// captured owners only when their other payload leases have also ended.
#[derive(Clone)]
pub struct OperationLease {
    capture: Arc<NamespaceCapture>,
}
impl OperationLease {
    pub fn capture(&self) -> &NamespaceCapture {
        &self.capture
    }
}

/// Exact immutable source owners and complete mutable selection inputs of a
/// capture request. Weak source-table owners never retain expression arenas.
struct CaptureInputs {
    declarations: crate::DeclarationIndex,
    catalog: catalog::CatalogIdentity,
    items: std::sync::Weak<BTreeMap<DefPath, Vec<Arc<ItemSyntax>>>>,
    indexed: BTreeMap<FileId, (FileContext, QueryEnvironment)>,
    generated: std::sync::Weak<BTreeMap<DefPath, GeneratedSource>>,
    definitions: Vec<DefId>,
    registrations: Vec<std::sync::Weak<AcceptedFrontRegistration>>,
    staged: BTreeMap<DefPath, crate::comptime::StagedConstant>,
    instances: hirsel_tir::InstanceBodies,
    selected: BTreeMap<DefId, BTreeSet<hirsel_types::GeneratedActualKey>>,
    store: Option<std::sync::Weak<dyn crate::store::ResultStore>>,
    limit: u64,
}
impl CaptureInputs {
    fn new(
        engine: &QueryEngine,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
        limit: u64,
    ) -> Self {
        Self {
            declarations: engine.declarations.observations(),
            catalog: engine.catalog.identity(),
            items: engine.items.owner(),
            indexed: engine.indexed_sources.clone(),
            generated: engine.generated.owner(),
            definitions: definitions.iter().map(DefId::intern).collect(),
            registrations: registrations.iter().map(Arc::downgrade).collect(),
            staged: engine.staged_constants.clone(),
            instances: engine.instance_bodies.clone(),
            selected: engine.selected_instances.clone(),
            store: engine.result_store.as_ref().map(Arc::downgrade),
            limit,
        }
    }
    fn matches(
        &self,
        engine: &QueryEngine,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
        limit: u64,
    ) -> bool {
        self.limit == limit
            && self.declarations.shares_inputs(&engine.declarations)
            && self.catalog.matches(&engine.catalog)
            && self.items.ptr_eq(&engine.items.owner())
            && self.indexed == engine.indexed_sources
            && self.generated.ptr_eq(&engine.generated.owner())
            && self
                .definitions
                .iter()
                .copied()
                .eq(definitions.iter().map(DefId::intern))
            && self.registrations.len() == registrations.len()
            && self
                .registrations
                .iter()
                .zip(registrations)
                .all(|(previous, current)| previous.ptr_eq(&Arc::downgrade(current)))
            && self.instances == engine.instance_bodies
            && self.selected == engine.selected_instances
            && self.staged.len() == engine.staged_constants.len()
            && self.staged.iter().zip(&engine.staged_constants).all(
                |((path, previous), (current_path, current))| {
                    path == current_path
                        && previous.owner == current.owner
                        && previous.expression == current.expression
                        && previous.inputs == current.inputs
                        && Arc::ptr_eq(&previous.signature, &current.signature)
                },
            )
            && match (&self.store, &engine.result_store) {
                (None, None) => true,
                (Some(previous), Some(current)) => previous.ptr_eq(&Arc::downgrade(current)),
                _ => false,
            }
    }
    fn retained_bytes(&mut self) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + 256);
        weight.flat_vec(&self.definitions);
        weight.flat_vec(&self.registrations);
        weight.tree::<FileId, (FileContext, QueryEnvironment)>(self.indexed.len());
        for (context, environment) in self.indexed.values() {
            weight.add(
                context.package.name.capacity()
                    + context.package.version.capacity()
                    + environment.compiler_version.capacity(),
            );
            weight.vec(&context.modules, |weight, module| {
                weight.add(module.capacity())
            });
        }

        weight.instance_bodies(&mut self.instances);
        weight.tree::<DefPath, crate::comptime::StagedConstant>(self.staged.len());
        for (path, staged) in &self.staged {
            weight.path(path);
            weight.path(&staged.owner);
            weight.flat_vec(&staged.inputs);
        }
        weight.tree::<DefId, BTreeSet<hirsel_types::GeneratedActualKey>>(self.selected.len());
        for keys in self.selected.values() {
            weight.tree::<hirsel_types::GeneratedActualKey, ()>(keys.len());
        }
        weight.bytes().checked_add(self.catalog.retained_bytes())
    }
}

pub(crate) struct DiagnosticSources {
    pub(crate) definition: DefId,
    pub(crate) sources: SourceMap,
    pub(crate) _allowance: Arc<crate::runtime::MemoryReservation>,
}
impl DiagnosticSources {
    pub(crate) fn owner(&self, anchor: hirsel_span::Anchor) -> Option<(DefPath, u32)> {
        let (file, base) = self.sources.anchor(anchor)?;
        let source = self.sources.file(file)?;
        Some((
            persistent::generated_codec_owner(self.definition.path(), source.text()),
            base,
        ))
    }
}

pub(crate) struct DiagnosticRecordSource<'a> {
    pub(crate) base: &'a NativeRecordContext,
    pub(crate) diagnostics: Option<&'a DiagnosticSources>,
}
impl crate::NativeRecordSource for DiagnosticRecordSource<'_> {
    fn record_types(&self) -> &Arc<hirsel_types::TypeInterner> {
        crate::NativeRecordSource::record_types(self.base)
    }
    fn record_owner(&self, anchor: hirsel_span::Anchor) -> Option<(DefPath, u32)> {
        crate::NativeRecordSource::record_owner(self.base, anchor)
            .or_else(|| self.diagnostics?.owner(anchor))
    }
    fn record_source_anchor(
        &self,
        owner: &DefPath,
        occurrence: u32,
    ) -> Option<hirsel_span::Anchor> {
        crate::NativeRecordSource::record_source_anchor(self.base, owner, occurrence).or_else(
            || {
                let diagnostics = self.diagnostics?;
                diagnostics.sources.anchors().find_map(|(anchor, _, _)| {
                    (diagnostics.owner(anchor).as_ref() == Some(&(owner.clone(), occurrence)))
                        .then_some(anchor)
                })
            },
        )
    }
}

struct NamespaceAdmission {
    _reservation: crate::runtime::MemoryReservation,
    _sources: SourceMap,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Manifest {
    base: ContentHash,
    dependencies: Vec<(DefId, Reference)>,
    trace: Vec<DefId>,
    instances: Arc<hirsel_tir::InstanceBodies>,
    budget: u64,
    depth: usize,
    charging_version: u32,
}

impl NamespaceCapture {
    fn same_sources(&self, sources: &SourceMap) -> bool {
        if !self.sources.anchors().eq(sources.anchors()) {
            return false;
        }
        for n in 0..=u32::MAX {
            match (self.sources.file(FileId(n)), sources.file(FileId(n))) {
                (None, None) => return true,
                (Some(previous), Some(current)) if std::ptr::eq(previous, current) => {}
                _ => return false,
            }
        }
        true
    }
    pub fn validation_input_digest(
        &self,
        package: &hirsel_syntax::PackageId,
    ) -> Result<ContentHash, hirsel_validate::ViewRefusal> {
        self.snapshot.validation_input_digest(package)
    }
    pub(crate) fn revalidate_execution(
        &self,
        receipt: &hirsel_validate::ExecutionReceipt,
    ) -> Option<hirsel_validate::ExecutionReceipt> {
        self.snapshot
            .revalidate_execution(receipt, |path, occurrence| {
                self.private_sources
                    .get(&(DefId::intern(path), occurrence))
                    .copied()
                    .or_else(|| {
                        crate::NativeRecordSource::record_source_anchor(
                            &*self.anchors,
                            path,
                            occurrence,
                        )
                    })
            })
    }
    pub(crate) fn verification(&self) -> Verify {
        self.verify
    }
    pub(crate) fn admits_successor(&self, successor: &Self) -> bool {
        successor.admission.get().is_some()
            && self.environment == successor.environment
            && self.verify == successor.verify
            && self.anchors.bindings_hold(&successor.anchors)
            && self.bodies.iter().all(|(id, previous)| {
                successor.bodies.get(id).is_some_and(|current| {
                    previous.syntax == current.syntax
                        && previous.implementation == current.implementation
                })
            })
            && self.registrations.iter().all(|(id, previous)| {
                successor
                    .registrations
                    .get(id)
                    .is_some_and(|current| previous.text() == current.text())
            })
    }
    /// Reconstruct metadata from this capture for a registration transaction.
    /// This assembler has no session, active S1 stack or ambient body cache.
    pub(crate) fn discovery_assembly(
        &self,
        adapters: &SourcePhases,
    ) -> Result<QueryEngine, QueryError> {
        self.assembly(adapters)
    }
    pub(crate) fn assembly(&self, adapters: &SourcePhases) -> Result<QueryEngine, QueryError> {
        let headers = self
            .snapshot
            .header_products()
            .ok_or_else(|| refused("missing captured header inputs"))?;
        let mut engine = headers.execution();
        self.inputs.install(&mut engine);
        engine.operation = self.operation.clone();
        engine.adapters = adapters.adapters();
        engine.verify = self.verify;
        engine.sources = self.sources.clone();
        engine.result_store = Some(self.store.clone());
        engine.instance_bodies = (*self.instances).clone();
        engine.generated.bind(engine.source_ownership.clone());
        // Metadata assembly must never invoke the standalone execution driver.
        engine.private_bootstrap_active = true;
        for (id, registration) in &self.registrations {
            engine
                .generated
                .insert(id.path().clone(), registration.source());
        }
        engine.private_summary_inputs = private_unique::registration_inputs(
            &self.registrations.values().cloned().collect::<Vec<_>>(),
        );
        engine.snapshot = Some((engine.snapshot_stamp(), self.snapshot.clone()));
        engine.inherit_source_records((*self.anchors).clone());
        Ok(engine)
    }
    pub(crate) fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            size_of::<NamespaceCapture>()
                + size_of::<crate::snapshot::Declarations>()
                + 2 * size_of::<usize>(),
        );
        weight.tree::<DefId, Arc<BodyInput>>(definitions.len());
        weight.add(definitions.len().checked_mul(
            size_of::<BodyInput>() + size_of::<BodyKeys>() + 4 * size_of::<usize>(),
        )?);
        weight.tree::<DefId, Arc<AcceptedFrontRegistration>>(definitions.len());
        weight.tree::<(DefId, u32), hirsel_span::Anchor>(definitions.len());
        Some(weight.bytes())
    }

    pub(crate) fn view_bytes(registrations: usize, private_sources: usize) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            size_of::<NamespaceCapture>()
                + size_of::<crate::snapshot::Declarations>()
                + 2 * size_of::<usize>(),
        );
        weight.tree::<DefId, Arc<AcceptedFrontRegistration>>(registrations);
        weight.tree::<(DefId, u32), hirsel_span::Anchor>(private_sources);
        Some(weight.bytes())
    }

    pub(crate) fn admit(
        &self,
        allowance: &mut crate::runtime::MemoryReservation,
        sources: SourceMap,
    ) -> Result<(), QueryError> {
        if self.admission.get().is_none() {
            let reservation = allowance
                .split(self.retained)
                .ok_or_else(|| refused("capture reservation"))?;
            if let Err(admission) = self.admission.set(NamespaceAdmission {
                _reservation: reservation,
                _sources: sources,
            }) {
                allowance
                    .join(admission._reservation)
                    .map_err(|_| refused("capture reservation partition"))?;
            }
        }
        self.inputs.admit(allowance)?;
        self.bodies.admit(allowance)?;
        Ok(())
    }
    pub(crate) fn require_environment(&self, engine: &QueryEngine) -> Result<(), QueryError> {
        if self.environment != engine.environment_value() || self.verify != engine.verify {
            return Err(refused(
                "consumer environment differs from namespace capture",
            ));
        }
        Ok(())
    }
    pub(crate) fn file_holds(&self, sources: &SourceMap, file: FileId) -> bool {
        self.sources
            .file(file)
            .zip(sources.file(file))
            .is_some_and(|(captured, current)| {
                captured.logical_path() == current.logical_path()
                    && captured.text() == current.text()
            })
    }
    pub(crate) fn query_id(&self, kind: u32, key: ContentHash) -> crate::runtime::QueryId {
        crate::runtime::QueryId {
            kind,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.namespace.query.v1",
                [
                    self.identity.0.as_slice(),
                    key.0.as_slice(),
                    kind.to_le_bytes().as_slice(),
                ],
            )),
        }
    }
    pub fn identity(&self) -> ContentHash {
        self.identity
    }
    pub fn lease(self: &Arc<Self>) -> OperationLease {
        OperationLease {
            capture: self.clone(),
        }
    }
    pub fn generation_manifest(&self) -> Reference {
        self.manifest
    }
    pub(crate) fn syntax(&self, path: &DefPath) -> Result<Arc<ItemSyntax>, QueryError> {
        let input = self
            .bodies
            .get(&DefId::intern(path))
            .ok_or_else(|| refused("body outside capture"))?;
        let syntax = match self.inputs.syntax(path)? {
            Some(syntax) => syntax,
            None => self.source_engine(path)?.snapshot(path)?,
        };
        if syntax.text_hash != input.implementation {
            return Err(refused("captured source binding changed"));
        }
        Ok(syntax)
    }
    pub(crate) fn source_engine(&self, path: &DefPath) -> Result<QueryEngine, QueryError> {
        #[cfg(test)]
        SOURCE_ENGINES.set(SOURCE_ENGINES.get() + 1);
        let headers = self
            .snapshot
            .header_products()
            .ok_or_else(|| refused("missing captured source owner"))?;
        let mut engine = headers.execution();
        self.inputs.install(&mut engine);
        engine.sources = self.sources.clone();
        engine.materialize(path)?;
        Ok(engine)
    }
    pub(crate) fn header(&self, path: &DefPath) -> Result<HeaderInput, QueryError> {
        let id = DefId::intern(path);
        if let Some(product) = self
            .snapshot
            .header_products()
            .and_then(|headers| headers.product(id))
        {
            let product = product?;
            return Ok(HeaderInput {
                output: product.result.clone()?,
                reads: product.entry.as_ref().map(|entry| entry.memo.reads.clone()),
                semantic: product.entry.as_ref().map_or_else(Vec::new, |entry| {
                    entry.detail.semantic_definitions().collect()
                }),
            });
        }
        let signature = self
            .snapshot
            .signature(id)
            .ok_or(QueryError::NotImplemented)??;
        Ok(HeaderInput {
            output: Arc::new(QueryOutput {
                value: signature,
                diagnostics: vec![],
                dependencies: vec![],
            }),
            reads: None,
            semantic: vec![],
        })
    }
    pub(crate) fn representation(
        &self,
        path: &DefPath,
    ) -> Option<crate::representation::OwnerRecord> {
        self.snapshot
            .header_products()?
            .product(DefId::intern(path))?
            .ok()?
            .entry
            .as_ref()?
            .products
            .representation
            .clone()
    }
    pub(crate) fn anchors(&self) -> &Arc<NativeRecordContext> {
        &self.anchors
    }
    pub(crate) fn retains_written(&self, syntax: &Arc<ItemSyntax>) -> bool {
        self.admission.get().is_some()
            && (self.inputs.owns_written(syntax)
                || self
                    .snapshot
                    .header_products()
                    .is_some_and(|headers| headers.owns_written(syntax)))
    }
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.retained
            .saturating_add(self.inputs.unadmitted_bytes())
            .saturating_add(self.bodies.unadmitted_bytes())
    }
    pub(crate) fn consistent(&self, other: &Self) -> bool {
        self.identity == other.identity
            && Arc::ptr_eq(&self.snapshot, &other.snapshot)
            && self.anchors().bindings_hold(other.anchors())
            && other.anchors().bindings_hold(self.anchors())
    }
    pub(crate) fn require_consistent(&self, other: &Self) -> Result<(), QueryError> {
        if self.consistent(other) {
            Ok(())
        } else {
            Err(refused("consumer captures differ"))
        }
    }
    /// Byte equality includes the complete independent base and each accepted
    /// generation. A missing manifest is a miss, never ambient source authority.
    pub(crate) fn manifest_holds(&self, reference: Reference, limit: u64) -> bool {
        if reference != self.manifest || reference.domain != MANIFEST || reference.len > limit {
            return false;
        }
        let store = &self.store;
        let Some(bytes) = store.get_content(reference) else {
            return false;
        };
        if MANIFEST.reference(&bytes) != Some(reference) {
            return false;
        }
        let Some(manifest): Option<Manifest> = self.records.decode(&**self.anchors(), &bytes)
        else {
            return false;
        };
        manifest.charging_version == hirsel_tir::CHARGING_VERSION
            && manifest.dependencies.iter().all(|(_, reference)| {
                reference.domain == GENERATION
                    && reference.len <= limit
                    && store
                        .get_content(*reference)
                        .is_some_and(|bytes| GENERATION.reference(&bytes) == Some(*reference))
            })
    }
}

#[cfg(test)]
thread_local! {
    static SOURCE_ENGINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn refused(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "namespace capture".into(),
        message: message.into(),
    })
}
fn generation_bytes(
    records: &NativeRecords,
    anchors: &NativeRecordContext,
    registration: &AcceptedFrontRegistration,
) -> Option<Vec<u8>> {
    records.encode(
        anchors,
        &(
            DefId::intern(registration.definition()),
            registration.text(),
            &registration.syntax,
            registration.reads(),
            registration.checked_prefix(),
            registration.depth(),
        ),
    )
}
impl QueryEngine {
    pub(crate) fn prepare_namespace_registrations(
        &mut self,
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) -> Result<(), QueryError> {
        self.prepare_namespace_scope(registrations);
        for registration in registrations {
            registration.validate(self)?;
        }
        Ok(())
    }
    pub(super) fn prepare_namespace_scope(
        &mut self,
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) {
        if !private_unique::registrations_covered(&self.private_summary_inputs, registrations) {
            self.private_summary_inputs = private_unique::registration_inputs(registrations);
            self.private_summary_revision = None;
            self.retire_snapshot();
        }
        let previous = self
            .snapshot
            .as_ref()
            .and_then(|(_, snapshot)| snapshot.header_products().cloned());
        let snapshot = self.signature_inputs();
        if let (Some(previous), Some(current)) = (previous, snapshot.header_products())
            && !Arc::ptr_eq(&previous, current)
        {
            current.inherit_registration_scope(&previous, registrations);
        }
    }
    pub fn capture_namespace(
        &mut self,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
        limit: u64,
    ) -> Result<Arc<NamespaceCapture>, QueryError> {
        self.prepare_namespace_scope(registrations);
        let snapshot = self.signature_inputs();
        if let Some(previous) = self
            .operation.captures.lock().unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .find(|previous| {
                previous.limits == self.comptime_limits
                    && previous.verify == self.verify
                    && previous.environment == self.environment_value()
                    && Arc::ptr_eq(&previous.snapshot, &snapshot)
                    && previous.same_sources(&self.sources)
                    && previous.anchors.bindings_hold(&self.source_records())
                    && previous
                        .capture_inputs
                        .matches(self, definitions, registrations, limit)
            })
        {
            return Ok(previous);
        }
        hirsel_intern::work::count(hirsel_intern::work::Counter::NamespaceConstructions);
        hirsel_intern::timings::count(hirsel_intern::timings::Count::NamespaceConstructions);
        let declarations = self.frozen_declarations().headers_only();
        let anchors = Arc::new(self.source_records());
        let store = self
            .result_store
            .clone()
            .unwrap_or_else(|| Arc::new(crate::store::NoneStore));
        let records = NativeRecords::new(self.types.clone(), store.clone());
        let mut bodies = BTreeMap::new();
        let mut private_sources = BTreeMap::new();
        let base = self
            .source_paths()
            .iter()
            .map(|path| {
                ContentHash::of_parts(
                    "hirsel.namespace.declaration.v1",
                    [
                        path.fingerprint().0.as_slice(),
                        self.candidates_value(path).0.as_slice(),
                    ],
                )
            })
            .collect::<Vec<_>>();
        let base = ContentHash::of_parts(
            "hirsel.namespace.base.v2",
            std::iter::once(self.environment_value().0.as_slice())
                .chain(base.iter().map(|hash| hash.0.as_slice())),
        );
        let environment = ContentHash::of_parts(
            "hirsel.persisted-products.v1",
            [
                self.environment_value().0.as_slice(),
                self.store_identity.0.as_slice(),
            ],
        );
        for path in definitions {
            let facts = self
                .source_facts(path)
                .ok_or_else(|| refused("missing declaration facts"))?;
            let implementation = self
                .implementation_hash(path)
                .ok_or_else(|| refused("missing declaration identity"))?;
            let site = self
                .items
                .get(path)
                .and_then(|items| items.first())
                .map(|syntax| syntax.item.name_span)
                .or_else(|| self.catalog.site(path))
                .or_else(|| self.captured_text(path).map(|text| text.site))
                .ok_or_else(|| refused(&format!("missing declaration site for {path}")))?;
            let registration = registrations
                .iter()
                .find(|accepted| accepted.definition() == path);
            let generated = facts.generator && !facts.instance_bodied;
            if generated {
                private_sources.insert(
                    (
                        DefId::intern(&generation::task::GeneratorInput::private_path(path)),
                        0,
                    ),
                    site.anchor,
                );
            }
            let queries = {
                let [modes, lower, evaluate] = match registration {
                    Some(accepted) => self.accepted_front_store_keys(accepted)?,
                    None => {
                        let mut key = |kind| {
                            if facts.generator && self.items.contains_key(path) {
                                self.front_declaration_store_key(path, kind)
                            } else {
                                Ok(EntryKey::new(
                                    environment,
                                    path,
                                    implementation,
                                    kind,
                                    LoweringPolicy::Reachable,
                                ))
                            }
                        };
                        [
                            key(QueryKind::Modes)?,
                            key(QueryKind::Lower)?,
                            key(QueryKind::Evaluate)?,
                        ]
                    }
                };
                Some(Arc::new(BodyKeys {
                    implementation,
                    site,
                    modes,
                    lower,
                    evaluate,
                }))
            };
            bodies.insert(
                DefId::intern(path),
                Arc::new(BodyInput {
                    syntax: ContentHash::of_parts(
                        "hirsel.namespace.syntax-input.v1",
                        [
                            implementation.0.as_slice(),
                            self.cited_value(path).0.as_slice(),
                        ],
                    ),
                    implementation,
                    executable: facts.executable,
                    generated,
                    queries,
                }),
            );
        }
        self.prepare_namespace_registrations(registrations)?;
        let snapshot = self.signature_inputs();
        let inputs = header_products::SourceInputs::capture(self);
        let selected = definitions.iter().cloned().collect::<BTreeSet<_>>();
        let mut instances = self.instance_bodies(&selected);
        for path in definitions {
            if self
                .source_facts(path)
                .is_some_and(|facts| facts.instance_bodied)
            {
                let id = DefId::intern(path);
                instances.declarations.insert(id);
                if let Some(bodies) = self.instance_bodies.instances.get(&id) {
                    let bodies = bodies
                        .iter()
                        .filter(|(_, body)| selected.contains(body.definition.path()))
                        .map(|(key, body)| (*key, body.clone()))
                        .collect::<std::collections::HashMap<_, _>>();
                    if !bodies.is_empty() {
                        instances.instances.insert(id, bodies);
                    }
                }
            }
        }
        let mut dependencies = BTreeMap::new();
        for registration in registrations {
            let id = DefId::intern(registration.definition());
            let bytes = generation_bytes(&records, &anchors, registration)
                .ok_or_else(|| refused("generation encoding"))?;
            if bytes.len() as u64 > limit {
                return Err(refused("generation exceeds admission"));
            }
            let reference = GENERATION
                .reference(&bytes)
                .ok_or_else(|| refused("generation reference"))?;
            if store.persistence().retains()
                && store.put_content(GENERATION, &bytes) != Some(reference)
            {
                return Err(refused("generation publication"));
            }
            if dependencies.insert(id, reference).is_some() {
                return Err(refused("duplicate accepted registration"));
            }
        }
        // Portable generation dependencies name only their generating
        // declarations. The capture identity retains the complete
        // written base; unrelated headers must not change ordinary records.
        let generation_headers = registrations
            .iter()
            .map(|registration| {
                let id = DefId::intern(registration.definition());
                (id, self.candidates_value(id.path()))
            })
            .chain(
                instances
                    .declarations
                    .iter()
                    .map(|id| (*id, self.candidates_value(id.path()))),
            )
            .collect::<BTreeMap<_, _>>();
        let generation_base = records
            .encode(&*anchors, &(self.environment_value(), generation_headers))
            .ok_or_else(|| refused("generation header encoding"))?;
        if generation_base.len() as u64 > limit {
            return Err(refused("generation headers exceed admission"));
        }
        let mut instance_weight = crate::NativeMetadataWeight::default();
        instance_weight.instance_bodies(&mut instances);
        let instances = Arc::new(instances);
        let manifest_inputs = Manifest {
            base: ContentHash::of_parts("hirsel.generation.base.v1", [generation_base.as_slice()]),
            dependencies: dependencies.into_iter().collect(),
            trace: registrations
                .iter()
                .map(|r| DefId::intern(r.definition()))
                .collect(),
            instances: instances.clone(),
            budget: self.comptime_limits.budget,
            depth: self.comptime_limits.call_depth,
            charging_version: hirsel_tir::CHARGING_VERSION,
        };
        let bytes = records
            .encode(&*anchors, &manifest_inputs)
            .ok_or_else(|| refused("manifest encoding"))?;
        if bytes.len() as u64 > limit {
            return Err(refused("manifest exceeds admission"));
        }
        let manifest = MANIFEST
            .reference(&bytes)
            .ok_or_else(|| refused("manifest reference"))?;
        if store.persistence().retains() && store.put_content(MANIFEST, &bytes) != Some(manifest) {
            return Err(refused("manifest publication"));
        }
        let mut previous = self.operation.bodies.lock().unwrap_or_else(|e| e.into_inner());
        let bodies = operation_inputs::InputIndex::capture(
            bodies,
            previous.as_ref(),
            |len| {
                let mut weight = crate::NativeMetadataWeight::default();
                weight.tree::<DefId, Arc<BodyInput>>(len);
                weight.add(len.checked_mul(
                    size_of::<BodyInput>() + size_of::<BodyKeys>() + 4 * size_of::<usize>(),
                )?);
                Some(weight.bytes())
            },
        )?;
        *previous = Some(bodies.clone());
        drop(previous);
        let body_identities = bodies
            .iter()
            .map(|(id, body)| {
                (
                    *id,
                    body.implementation,
                    body.queries.as_ref().map(|keys| {
                        (
                            keys.modes.0,
                            keys.modes.1,
                            keys.lower.0,
                            keys.lower.1,
                            keys.evaluate.0,
                            keys.evaluate.1,
                        )
                    }),
                )
            })
            .collect::<Vec<_>>();
        let body_bytes = records
            .encode(&*anchors, &body_identities)
            .ok_or_else(|| refused("body identity encoding"))?;
        let staged = self
            .staged_constants
            .iter()
            .map(|(path, constant)| (DefId::intern(path), constant.signature.fingerprint))
            .collect::<Vec<_>>();
        let staged = records
            .encode(&*anchors, &staged)
            .ok_or_else(|| refused("staged header identity encoding"))?;
        let identity = ContentHash::of_parts(
            "hirsel.namespace.capture.v1",
            [
                base.0.as_slice(),
                manifest.output.0.0.as_slice(),
                body_bytes.as_slice(),
                staged.as_slice(),
            ],
        );
        let registrations: BTreeMap<_, _> = registrations
            .iter()
            .map(|r| (DefId::intern(r.definition()), r.clone()))
            .collect();
        let source_paths = self
            .source_paths()
            .iter()
            .map(DefId::intern)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let roots = self
            .front_source_roots()
            .iter()
            .map(DefId::intern)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of_val(&*source_paths) + size_of_val(&*roots));
        weight.flat_vec(&manifest_inputs.dependencies);
        weight.flat_vec(&manifest_inputs.trace);
        let mut capture_inputs = CaptureInputs::new(
            self,
            definitions,
            &registrations.values().cloned().collect::<Vec<_>>(),
            limit,
        );
        // The caller's order is semantic, independently of the keyed registry.
        capture_inputs.registrations = manifest_inputs
            .trace
            .iter()
            .map(|id| Arc::downgrade(&registrations[id]))
            .collect();
        let capture_bytes = capture_inputs
            .retained_bytes()
            .ok_or_else(|| refused("capture input size overflow"))?;
        let retained = NamespaceCapture::view_bytes(registrations.len(), private_sources.len())
            .and_then(|bytes| bytes.checked_add(capture_bytes))
            .and_then(|bytes| bytes.checked_add(weight.bytes()))
            .and_then(|bytes| bytes.checked_add(instance_weight.bytes()))
            .ok_or_else(|| refused("capture size overflow"))?;
        if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
            eprintln!(
                "OWNER CAPTURE fixed={:p} body={:p} view={} selection={} paths={} instances={} source_total={} source_pending={} bodies_pending={} definitions={} registrations={} limit={}",
                Arc::as_ptr(&inputs),
                &bodies,
                retained,
                capture_bytes,
                weight.bytes(),
                instance_weight.bytes(),
                inputs.retained_bytes(),
                inputs.unadmitted_bytes(),
                bodies.unadmitted_bytes(),
                definitions.len(),
                registrations.len(),
                limit
            );
        }
        if retained
            .checked_add(inputs.unadmitted_bytes())
            .and_then(|bytes| bytes.checked_add(bodies.unadmitted_bytes()))
            .is_none_or(|bytes| bytes > limit)
        {
            return Err(refused("capture exceeds admission"));
        }
        let capture = Arc::new(NamespaceCapture {
            snapshot,
            inputs,
            declarations,
            source_paths,
            roots,
            sources: self.sources.clone(),
            anchors,
            store,
            instances,
            bodies,
            registrations,
            private_sources,
            records,
            limits: self.comptime_limits,
            environment: self.environment_value(),
            verify: self.verify,
            manifest,
            manifest_inputs,
            identity,
            retained,
            admission: std::sync::OnceLock::new(),
            capture_inputs,
            operation: self.operation.clone(),
        });
        let mut captures = self.operation.captures.lock().unwrap_or_else(|e| e.into_inner());
        captures.retain(|capture| capture.strong_count() != 0);
        captures.push(Arc::downgrade(&capture));
        Ok(capture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemStore, Persistence, ResultStore};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct StoreOff {
        installed: Arc<MemStore>,
        writes: AtomicUsize,
    }
    impl ResultStore for StoreOff {
        fn name(&self) -> &'static str {
            "capture-store-off"
        }
        fn persistence(&self) -> Persistence {
            Persistence::Disabled
        }
        fn get(&self, key: &EntryKey) -> Option<crate::store::Bytes> {
            self.installed.get(key)
        }
        fn put(&self, _: &EntryKey, _: &[u8]) {
            self.writes.fetch_add(1, Ordering::Relaxed);
        }
        fn put_content(&self, _: Domain, _: &[u8]) -> Option<Reference> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
    fn indexed(store: Arc<dyn ResultStore>) -> (QueryEngine, Vec<DefPath>) {
        let mut engine =
            QueryEngine::new(default_environment(), crate::phases::engine_only_adapters())
                .with_result_store(store);
        let context = default_context();
        for (file, source) in [
            ("main.hir", "pub fn main()->u8{1u8}\n"),
            ("unrelated.hir", "pub fn unrelated()->u8{2u8}\n"),
        ] {
            let file = engine.sources.add(file, source.into()).unwrap();
            engine.index_source(file, &context).unwrap();
        }
        let definitions = engine.source_paths();
        (engine, definitions)
    }
    #[test]
    fn namespace_installation_keeps_indexed_bodies_undemanded() {
        crate::with_compiler_stack(|| {
            let store = Arc::new(MemStore::default());
            drop(indexed(store.clone()));
            let (mut engine, definitions) = indexed(store);
            assert!(
                engine.items.is_empty(),
                "warm declaration indexing must be body free"
            );
            let capture = engine
                .capture_namespace(&definitions, &[], 8 << 20)
                .unwrap();
            assert!(
                engine.items.is_empty(),
                "installing a capture must not demand any body"
            );
            assert!(engine.signatures.is_empty());
            let syntax = capture.syntax(&definitions[0]).unwrap();
            assert_eq!(syntax.item.definition, definitions[0]);
            let demanded = capture.source_engine(&definitions[0]).unwrap();
            assert!(demanded.items.contains_key(&definitions[0]));
            assert!(!demanded.items.contains_key(&definitions[1]));
            assert!(
                engine.items.is_empty(),
                "demand uses the immutable capture's owner"
            );
        });
    }
    #[test]
    fn store_off_capture_needs_no_syntax_or_manifest_archive() {
        crate::with_compiler_stack(|| {
            let store = Arc::new(StoreOff::default());
            let (mut engine, definitions) = indexed(store.clone());
            let before =
                hirsel_intern::work::read(hirsel_intern::work::Counter::NamespaceConstructions);
            let capture = engine
                .capture_namespace(&definitions, &[], 8 << 20)
                .unwrap();
            for _ in 0..3 {
                let same = engine
                    .capture_namespace(&definitions, &[], 8 << 20)
                    .unwrap();
                assert!(Arc::ptr_eq(&capture, &same));
            }
            let subset = engine
                .capture_namespace(&definitions[..1], &[], 8 << 20)
                .unwrap();
            assert_eq!(subset.bodies.len(), 1);
            assert!(!Arc::ptr_eq(&capture, &subset));
            let again = engine
                .capture_namespace(&definitions, &[], 8 << 20)
                .unwrap();
            assert!(Arc::ptr_eq(&capture, &again));
            assert_eq!(
                hirsel_intern::work::read(hirsel_intern::work::Counter::NamespaceConstructions)
                    - before,
                2,
                "two independently selected input sets build twice, regardless of consumer order/count"
            );
            engine.comptime_limits.budget -= 1;
            let changed = engine
                .capture_namespace(&definitions, &[], 8 << 20)
                .unwrap();
            assert_ne!(capture.manifest, changed.manifest);
            assert!(!Arc::ptr_eq(&capture, &changed));
            drop(engine);
            for definition in &definitions {
                assert_eq!(
                    capture.syntax(definition).unwrap().item.definition,
                    *definition
                );
            }
            assert_eq!(store.writes.load(Ordering::Relaxed), 0);
        });
    }
    struct Header;
    impl SignaturePhase for Header {
        fn signature(
            &self,
            types: &hirsel_types::TypeInterner,
            _: &hirsel_types::TypeMemo,
            syntax: Arc<ItemSyntax>,
            _: &Declarations,
            products: &mut PhaseProducts,
            _: &mut dyn SignatureProvider,
        ) -> Result<QueryOutput<hirsel_types::Signature>, QueryError> {
            products.resolution = Some(crate::ResolverSnapshot {
                syntax: syntax.clone(),
                output: output(Default::default()),
            });
            Ok(output(hirsel_types::Signature {
                metadata: Default::default(),
                definition: DefId::intern(&syntax.item.definition),
                kind: hirsel_types::SignatureKind::Const {
                    ty: types.intern(hirsel_types::Type::unit()),
                    generics: Default::default(),
                },
                fingerprint: syntax.text_hash,
            }))
        }
    }
    #[test]
    fn request_demanders_parse_and_project_one_live_source_once() {
        struct Parser {
            inner: Arc<dyn crate::ParserPhase>,
            calls: Arc<AtomicUsize>,
        }
        impl crate::ParserPhase for Parser {
            fn parse(
                &self,
                file: FileId,
                tokens: Vec<hirsel_syntax::Token>,
                context: &FileContext,
                sources: &mut SourceMap,
            ) -> Result<QueryOutput<SyntaxFile>, QueryError> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                self.inner.parse(file, tokens, context, sources)
            }
        }
        crate::with_compiler_stack(|| {
            for cold in [false, true] {
                let calls = Arc::new(AtomicUsize::new(0));
                let mut adapters = crate::phases::engine_only_adapters();
                adapters.parser = Some(Arc::new(Parser {
                    inner: adapters.parser.take().unwrap(),
                    calls: calls.clone(),
                }));
                adapters.signature = Some(Arc::new(Header));
                let store = Arc::new(StoreOff::default());
                if cold {
                    let mut seed = QueryEngine::new(
                        default_environment(),
                        crate::phases::engine_only_adapters(),
                    )
                    .with_result_store(store.installed.clone());
                    let file = seed
                        .sources
                        .add("shared.hir", "pub fn main(){()}\n".into())
                        .unwrap();
                    seed.index_source(file, &default_context()).unwrap();
                }
                let provider: Arc<dyn ResultStore> = if cold {
                    store.installed.clone()
                } else {
                    store.clone()
                };
                let mut engine =
                    QueryEngine::new(default_environment(), adapters).with_result_store(provider);
                let file = engine
                    .sources
                    .add("shared.hir", "pub fn main(){()}\n".into())
                    .unwrap();
                engine.index_source(file, &default_context()).unwrap();
                if cold {
                    assert!(
                        engine.items.is_empty(),
                        "warm indexing must leave syntax undemanded"
                    );
                    engine.verify_store = crate::store::VerifyStore::Recompute;
                    engine = engine.with_result_store(store.clone());
                    engine.signature_inputs();
                    engine.sources = engine.sources.clone().retain_owner(Arc::new(()));
                }
                let definitions = engine.source_paths();
                let capture = engine
                    .capture_namespace(&definitions, &[], 8 << 20)
                    .unwrap();
                drop(engine);
                header_products::PROJECTIONS.set(0);
                if cold {
                    capture.header(&definitions[0]).unwrap();
                    let barrier = std::sync::Barrier::new(8);
                    std::thread::scope(|scope| {
                        let workers: Vec<_> = (0..8)
                            .map(|_| {
                                scope.spawn(|| {
                                    barrier.wait();
                                    capture.syntax(&definitions[0]).unwrap()
                                })
                            })
                            .collect();
                        let syntax: Vec<_> = workers
                            .into_iter()
                            .map(|worker| worker.join().unwrap())
                            .collect();
                        assert!(syntax.iter().all(|item| Arc::ptr_eq(item, &syntax[0])));
                    });
                }
                let syntax = capture.syntax(&definitions[0]).unwrap();
                let assemblies = SOURCE_ENGINES.get();
                for _ in 0..8 {
                    assert!(Arc::ptr_eq(
                        &syntax,
                        &capture.syntax(&definitions[0]).unwrap()
                    ));
                }
                assert_eq!(
                    SOURCE_ENGINES.get(),
                    assemblies,
                    "read-only syntax must reuse its captured owner"
                );
                for _ in 0..8 {
                    let mut demander = capture.source_engine(&definitions[0]).unwrap();
                    demander.item_signature(&definitions[0]).unwrap();
                    demander.signature_inputs();
                }
                // One immutable source and one declared header, shared by eight
                // demanders of the same request: each producer runs exactly once.
                assert_eq!(
                    (
                        calls.load(Ordering::Relaxed),
                        header_products::PROJECTIONS.get()
                    ),
                    (1, 1),
                    "source parses and header projections",
                );
                assert_eq!(store.writes.load(Ordering::Relaxed), 0);
            }
        });
    }
    #[test]
    fn successor_views_charge_fixed_operation_inputs_once_until_the_last_lease() {
        crate::with_compiler_stack(|| {
            let (mut engine, definitions) = indexed(Arc::new(MemStore::default()));
            let budget = crate::runtime::ProcessBudget::new(8 << 20);
            let pool = crate::runtime::MemoryReservationPool::new(budget.reserve(8 << 20).unwrap())
                .ok()
                .unwrap();
            let before = pool.available_bytes();
            let mut views = Vec::new();
            let mut view_bytes = 0;
            let mut fixed = None;
            for successor in 0..8 {
                let capture = engine
                    .capture_namespace(&definitions, &[], (8 << 20) - successor)
                    .unwrap();
                let required = capture.retained_bytes();
                if let Some((inputs, bodies)) = fixed {
                    assert_eq!(capture.inputs.unadmitted_bytes(), 0);
                    assert_eq!(capture.bodies.unadmitted_bytes(), 0);
                    assert_eq!(required, capture.retained);
                    assert_eq!(
                        before - pool.available_bytes(),
                        inputs + bodies + view_bytes
                    );
                } else {
                    fixed = Some((
                        capture.inputs.unadmitted_bytes(),
                        capture.bodies.unadmitted_bytes(),
                    ));
                }
                let mut allowance = pool.take(required).unwrap();
                capture
                    .admit(&mut allowance, engine.sources.clone())
                    .unwrap();
                assert_eq!(allowance.bytes(), 0);
                view_bytes += capture.retained;
                for definition in &definitions {
                    assert_eq!(
                        capture.bodies[&DefId::intern(definition)].implementation,
                        engine.implementation_hash(definition).unwrap()
                    );
                }
                let reconstructed = capture.assembly(&engine.adapters.source_phases()).unwrap();
                views.push(capture);
                engine = reconstructed;
                // Distinct admission selections retain the same exact written
                // inputs and query keys, just as a registration successor does.
            }
            let last = views.pop().unwrap();
            drop(views);
            drop(engine);
            assert!(
                pool.available_bytes() < before,
                "the last reader keeps fixed inputs admitted"
            );
            assert_eq!(last.inputs.unadmitted_bytes(), 0);
            assert_eq!(last.bodies.unadmitted_bytes(), 0);
            let retained_slot = last.lease();
            drop(last);
            assert!(
                pool.available_bytes() < before,
                "a retained holder keeps the payload admitted after all readers finish"
            );
            assert_eq!(retained_slot.capture().inputs.unadmitted_bytes(), 0);
            drop(retained_slot);
            assert_eq!(
                pool.available_bytes(),
                before,
                "the last lease returns all input bytes"
            );
        });
    }
    #[test]
    fn demanded_syntax_keeps_its_charge_after_the_front_partition_ends() {
        use hirsel_validate::SignatureLookup;
        struct Allowance(Arc<crate::runtime::MemoryReservationPool>);
        impl header_products::HeaderAllowance for Allowance {
            fn take_header(&self, bytes: u64) -> Option<crate::runtime::MemoryReservation> {
                self.0.take(bytes)
            }
        }
        crate::with_compiler_stack(|| {
            let store = Arc::new(MemStore::default());
            drop(indexed(store.clone()));
            let (mut engine, definitions) = indexed(store);
            engine.adapters.signature = Some(Arc::new(Header));
            assert!(engine.items.is_empty());
            let capture = engine
                .capture_namespace(&definitions, &[], 8 << 20)
                .unwrap();
            let types = engine.types.clone();
            drop(engine);
            let headers = capture.snapshot.header_products().unwrap().clone();
            let definition = DefId::intern(&definitions[0]);
            let package = definitions[0].0.package.clone();
            let budget = crate::runtime::ProcessBudget::new(8 << 20);
            let pool = Arc::new(
                crate::runtime::MemoryReservationPool::new(budget.reserve(8 << 20).unwrap())
                    .ok()
                    .unwrap(),
            );
            let before = pool.available_bytes();
            let partition = headers.configure_source_backing(
                Arc::new(Allowance(pool.clone())),
                Arc::new(std::sync::Mutex::new(before)),
            );
            for definition in &definitions {
                drop(capture.syntax(definition).unwrap());
            }
            let retained = pool.available_bytes();
            assert!(retained < before, "the live source owns its syntax charge");
            let signature = capture.snapshot.signature(definition).unwrap().unwrap();
            let expected = hirsel_validate::View::new(&types, signature.clone(), None).digest();
            capture.snapshot.package_universe(&package).unwrap();
            {
                let _pinned = definitions
                    .iter()
                    .map(|definition| headers.product(DefId::intern(definition)).unwrap().unwrap())
                    .collect::<Vec<_>>();
                let held = pool.take(pool.available_bytes()).unwrap();
                let before = hirsel_intern::work::read(
                    hirsel_intern::work::Counter::ValidationRetentionRefusals,
                );
                for _ in 0..2 {
                    let views = capture
                        .snapshot
                        .validation_with(&package, signature.clone())
                        .unwrap();
                    assert_eq!(views.view(definition).unwrap().digest(), expected);
                    assert!(views.universe_errors().1.is_empty());
                }
                assert_eq!(
                    hirsel_intern::work::read(
                        hirsel_intern::work::Counter::ValidationRetentionRefusals
                    ) - before,
                    2
                );
                assert_eq!(
                    pool.available_bytes(),
                    0,
                    "transient validation publishes no retained charge"
                );
                drop(held);
            }
            drop(partition);
            assert!(
                pool.available_bytes() < retained,
                "live syntax and signature leases keep their reservations"
            );
            drop(capture);
            drop(headers);
            assert!(
                pool.available_bytes() < before,
                "the raw signature retains its payload lease"
            );
            drop(signature);
            assert_eq!(
                pool.available_bytes(),
                before,
                "the final payload lease releases its reservation"
            );
        });
    }
}

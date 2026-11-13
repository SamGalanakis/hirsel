//! Native root selection and static initialization over settled ordinary
//! products. Source selection keeps the existing whole-package default and
//! semantic closure; host readiness only inspects this completed handoff.
use super::*;
use hirsel_engine_admission::{MemoryReservation, ProcessBudget};
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, Reference, ResultStore};
use hirsel_registry::{OperationKey, OperationParameter, OperationRecord};
use hirsel_types::SignatureKind;
use std::sync::Mutex;
mod acceptance;

pub const FRONT_NATIVE_ROOT_KIND: u32 = 0x4600_001a;
const OUTPUT: Domain = Domain {
    kind: 0x4600_001b,
    schema: 2,
};
const FAILURE: Domain = Domain {
    kind: 0x4600_001c,
    schema: 1,
};
/// Install source selection before exposing either factory. Both views are
/// consumed unshared, so no running request can observe a changed namespace.
pub struct FrontNativePlan {
    pub entry: DefPath,
    /// Empty selects every nondependency source root, as ordinary native export.
    pub roots: Vec<DefPath>,
    pub run: Arc<FrontRunQueries>,
    pub definitions: Arc<FrontNativeDefinitionQueries>,
}
pub struct FrontNativeProducts {
    pub roots: crate::NativeSourceRoots,
    pub statics: crate::NativeStaticInputs,
    pub catalogue: Vec<Arc<crate::NativeDiscoveryProduct>>,
    pub extra: InstanceGraph,
    pub originals: Vec<Arc<crate::NativeOriginalProduct>>,
    pub original_context: Arc<crate::NativeOriginalContext>,
    pub signatures: Arc<SignatureSnapshot>,
    pub run: Arc<FrontRunOutput>,
    pub semantic: Arc<crate::NativeSemanticProgram>,
    _charge: MemoryReservation,
    _owner: Arc<Ownership>,
}
pub struct FrontNativeOutput {
    pub products: Arc<FrontNativeProducts>,
    _owner: Arc<FrontNativeQueries>,
}
pub struct FrontNativeQueries {
    query: QueryId,
    entry: DefPath,
    template: crate::NativeSourceRoots,
    constants: Vec<DefPath>,
    registry: hirsel_registry::BuiltinRegistry,
    memo: Arc<hirsel_types::TypeMemo>,
    run: Arc<FrontRunQueries>,
    definitions: Arc<FrontNativeDefinitionQueries>,
    records: crate::NativeRecords,
    anchors: crate::NativeRecordContext,
    store: Arc<dyn ResultStore>,
    completed: Mutex<Option<(ProductRef, Arc<FrontNativeProducts>)>>,
    accepted: Mutex<Option<ProductRef>>,
    failure: Mutex<Option<(Reference, QueryError)>>,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    ownership: Arc<Ownership>,
}
struct Ownership {
    _run: Arc<FrontRunQueries>,
    _definitions: Arc<FrontNativeDefinitionQueries>,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
}
fn constant_bytes<'a>(mut paths: impl Iterator<Item = &'a DefPath>) -> Option<u64> {
    paths.try_fold(0u64, |total, path| {
        total.checked_add(
            bincode::serialized_size(path)
                .ok()?
                .checked_add(std::mem::size_of::<DefPath>() as u64)?,
        )
    })
}
fn metadata_bytes<'a>(
    operations: &BTreeMap<OperationKey, OperationRecord>,
    mut builtin_paths: impl Iterator<Item = &'a DefPath>,
    constant_paths: impl Iterator<Item = &'a DefPath>,
) -> Option<u64> {
    let operations = operations
        .iter()
        .try_fold(0u64, |total, (key, operation)| {
            total
                .checked_add(
                    bincode::serialized_size(&key.definition)
                        .ok()?
                        .checked_mul(2)?,
                )?
                .checked_add(
                    (std::mem::size_of::<(OperationKey, OperationRecord)>()
                        + operation.parameters.len() * std::mem::size_of::<OperationParameter>()
                        + 128) as u64,
                )
        })?;
    builtin_paths
        .try_fold(operations, |total, path| {
            total.checked_add(bincode::serialized_size(path).ok()?.checked_add(128)?)
        })?
        .checked_add(constant_bytes(constant_paths)?)
}

impl FrontNativeQueries {
    fn request(&self) -> &Arc<ProcessBudget> {
        self.ownership._allowance.budget()
    }
    pub fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        // Core operation/builtin inputs are mandatory even for a source with
        // no calls. Cache their immutable inventory cost, then bound source
        // constants by the supplied complete definition namespace. The actual
        // constructor still charges only its selected constant paths.
        static CORE_BYTES: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        let core = *CORE_BYTES.get_or_init(|| {
            let registry = hirsel_registry::BuiltinRegistry::shared();
            let operations = registry.operation_inventory();
            metadata_bytes(
                &operations,
                registry.signatures().map(|s| s.definition.path()),
                registry
                    .signatures()
                    .filter(|s| matches!(s.kind, SignatureKind::Const { .. }))
                    .map(|s| s.definition.path()),
            )
        });
        Self::header_bytes(definitions)?
            .checked_add(core?)?
            .checked_add(constant_bytes(definitions.iter())?)
    }
    fn header_bytes(definitions: &[DefPath]) -> Option<u64> {
        definitions.iter().try_fold(
            (std::mem::size_of::<Self>()
                + std::mem::size_of::<Ownership>()
                + std::mem::size_of::<Mutex<u64>>()
                + std::mem::size_of::<hirsel_engine_admission::MemoryReservation>()
                + 6 * std::mem::size_of::<usize>()) as u64,
            |total, path| {
                total
                    .checked_add(bincode::serialized_size(path).ok()?.checked_mul(4)?)?
                    .checked_add(
                        // Only immutable source/header slots live here.
                        // Demanded catalogue outputs are charged to the root
                        // continuation when it actually reads them below.
                        (4 * std::mem::size_of::<DefPath>() + 256) as u64,
                    )
            },
        )
    }
    pub fn query(&self) -> QueryId {
        self.query
    }
    /// Native source data can feed independently validated native producers
    /// before complete source acceptance. Executable consumers join `query()`.
    pub fn preparation_query(&self) -> QueryId {
        QueryId {
            kind: FRONT_NATIVE_ROOT_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.native-preparation.address.v1",
                [self.query.address.0.0.as_slice()],
            )),
        }
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    pub(crate) fn export_run(&self, entry: &DefPath) -> Option<Arc<FrontRunQueries>> {
        (entry == &self.entry).then(|| self.run.clone())
    }
    /// The exact metadata Run selected by this native preparation or acceptance.
    /// No source-root selection or new query is performed here.
    pub fn completed_run_product(&self, product: ProductRef) -> Option<ProductRef> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, _) = completed.as_ref()?;
        if *selected != product && !self.is_accepted(&product) {
            return None;
        }
        self.run
            .completed_product(self.run.preparation_query(&self.entry)?)
    }
    /// Actual IDs from the Run view after native source-root selection.
    pub fn run_query(&self, path: &DefPath) -> Option<QueryId> {
        self.run.preparation_query(path)
    }
    /// Every admitted semantic address, including core, and the one Run root.
    pub fn run_query_inventory(&self) -> impl Iterator<Item = QueryId> + '_ {
        self.run.preparation_inventory()
    }

    /// Actual IDs from the selected source catalogue captured by this root.
    pub fn definition_query(&self, path: &DefPath) -> Option<QueryId> {
        self.definitions.query(path)
    }
    pub fn run_factory(&self) -> Arc<dyn QueryFactory> {
        self.run.factory()
    }
    pub fn definition_factory(&self) -> Arc<dyn QueryFactory> {
        self.definitions.factory()
    }
    pub fn outcome(self: &Arc<Self>, product: ProductRef) -> Option<FrontNativeOutput> {
        let value = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = value.as_ref()?;
        (*selected == product || self.is_accepted(&product)).then(|| FrontNativeOutput {
            products: data.clone(),
            _owner: self.clone(),
        })
    }
    fn is_accepted(&self, product: &ProductRef) -> bool {
        self.accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|selected| selected == product)
    }
    pub fn failure(&self, reference: Reference) -> Option<QueryError> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|(selected, error)| (*selected == reference).then(|| error.clone()))
            .or_else(|| self.definitions.failure(reference))
            .or_else(|| self.run.failure(reference))
            .or_else(|| {
                self.run
                    .source_check()?
                    .failure(reference)
                    .map(|(error, _)| error)
            })
            .or_else(|| self.run.source_check()?.rejection(reference))
    }
    fn fail(&self, error: QueryError) -> Reference {
        let Some(bytes) = self.records.encode(&self.anchors, &error) else {
            return self.errors.resource;
        };
        if bytes.len() as u64 > self.request().free() {
            return self.errors.resource;
        }
        let Some(reference) = self.store.put_content(FAILURE, &bytes) else {
            return self.errors.resource;
        };
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some((reference, error));
        reference
    }
}
struct Factory(Arc<FrontNativeQueries>);
impl QueryFactory for Factory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if id == self.0.query {
            return Ok(Box::new(acceptance::Task::new(self.0.clone())));
        }
        if id != self.0.preparation_query() {
            return Err(self.0.errors.protocol);
        }
        let terminal = self
            .0
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(p, _)| Ok(p.clone()));
        Ok(Box::new(Task {
            owner: self.0.clone(),
            id,
            terminal,
        }))
    }
}
struct Task {
    owner: Arc<FrontNativeQueries>,
    id: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
struct DefinitionsState {
    selected: Vec<DefPath>,
    run_product: ProductRef,
    next: usize,
    definitions: Vec<FrontNativeDefinitionOutput>,
    evaluated: Vec<QueryId>,
    charge: MemoryReservation,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        self.owner.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = &self.terminal {
            return result.clone();
        }
        let result = prepare(&self.owner, self.id, cx);
        self.terminal = Some(result.clone());
        result
    }
}
fn prepare(
    owner: &Arc<FrontNativeQueries>,
    id: QueryId,
    cx: &mut dyn QueryContext,
) -> QueryOutcome {
    for path in &owner.template.definitions {
        let definition = owner.definitions.query(path).ok_or(owner.errors.protocol)?;
        cx.read(definition)?;
    }
    let run_id = owner
        .run
        .preparation_query(&owner.entry)
        .ok_or(owner.errors.protocol)?;
    let product = cx.read(run_id)?;
    let output = owner
        .run
        .outcome(product.clone())
        .ok_or(owner.errors.protocol)?;
    let charge = owner
        .request()
        .reserve(0)
        .map_err(|_| owner.errors.resource)?;
    let mut state = DefinitionsState::prepared(product, &output, charge);
    while state.next < state.selected.len() {
        let path = &state.selected[state.next];
        let Some(definition) = owner.definitions.query(path) else {
            return Err(owner.fail(invalid(format!(
                "missing native source product input {path}"
            ))));
        };
        let product = cx.read(definition)?;
        let output = owner
            .definitions
            .outcome(product)
            .ok_or(owner.errors.protocol)?;
        state.definition_done(output, owner)?;
    }
    // Native preparation can precede the ordered Check root. Select its
    // runtime comptime values explicitly rather than relying on Check work
    // having happened while a dependency was suspended.
    let staged = owner
        .definitions
        .lower()
        .runtime_staged_queries(
            state.selected.iter().cloned(),
            cx.scratch_bytes(),
            cx.scratch_bytes(),
            cx,
        )
        .map_err(|error| owner.fail(error))?;
    for (query, site) in staged {
        if let Err(reference) = cx.read(query) {
            return Err(owner
                .definitions
                .lower()
                .staged_failure(query, reference, site)
                .map(|error| owner.fail(error))
                .unwrap_or(reference));
        }
        state
            .charge
            .grow(4 * std::mem::size_of::<QueryId>() as u64)
            .map_err(|_| owner.errors.resource)?;
        state.evaluated.push(query);
    }
    while let Some(query) = owner
        .definitions
        .staged_static_query(&state.selected, &state.evaluated)
    {
        cx.read(query)?;
        // Vec's first allocation holds four query slots; this
        // bound also covers every later geometric growth step.
        state
            .charge
            .grow(4 * std::mem::size_of::<QueryId>() as u64)
            .map_err(|_| owner.errors.resource)?;
        state.evaluated.push(query);
    }
    state
        .complete(owner, id, cx.scratch_bytes())
        .map_err(|error| owner.fail(error))
}
impl DefinitionsState {
    fn prepared(
        run_product: ProductRef,
        output: &FrontRunOutput,
        charge: MemoryReservation,
    ) -> Self {
        // Bootstrap binds every checked semantic declaration, including
        // compile-time-only functions. Runtime registration is a separate
        // subset; omitting these headers breaks the exact source union.
        Self {
            selected: output
                .semantic
                .source_bindings()
                .map(|(path, _, _)| path.clone())
                .collect(),
            run_product,
            next: 0,
            definitions: Vec::new(),
            evaluated: Vec::new(),
            charge,
        }
    }
    fn definition_done(
        &mut self,
        output: FrontNativeDefinitionOutput,
        owner: &FrontNativeQueries,
    ) -> Result<(), Reference> {
        // Each handoff row is a handle to a product its definition owner
        // has already charged to the request, so the root holds only the row
        // and its vector slot.
        self.charge
            .grow(2 * std::mem::size_of::<FrontNativeDefinitionOutput>() as u64)
            .map_err(|_| owner.errors.resource)?;
        self.definitions.push(output);
        self.next += 1;
        Ok(())
    }
    fn complete(
        self,
        owner: &Arc<FrontNativeQueries>,
        id: QueryId,
        scratch: u64,
    ) -> Result<ProductRef, QueryError> {
        let run_product = self.run_product;
        let mut charge = self.charge;
        let run = owner
            .run
            .outcome(run_product.clone())
            .ok_or(QueryError::NotImplemented)?;
        let current = owner.definitions.lower();
        let signatures = current.signatures();
        let anchors = current.namespace().anchors.clone();
        let original_context = owner.definitions.original_context();
        let native_selected = self.selected.iter().cloned().collect::<Vec<_>>();
        let selected_set = native_selected.iter().cloned().collect::<BTreeSet<_>>();
        // Decorating the external map copies its file/path metadata. Shared
        // text, lines and anchors keep their original producer reservations.
        // Refuse before allocating the copy, within the existing output grant.
        let source_metadata = owner.template.sources.retained_snapshot_metadata_bytes();
        charge
            .grow(source_metadata)
            .map_err(|_| invalid("native source metadata exceeds admission"))?;
        let mut roots = owner.template.clone();
        roots.definitions = native_selected;
        roots.foreign_unreplayable = run.semantic.foreign_unreplayable();
        let source_constants = self
            .definitions
            .iter()
            .filter(|output| output.product.source.constant.is_some())
            .map(|output| output.product.source.definition.clone())
            .collect::<BTreeSet<_>>();
        let mut constants = BTreeMap::new();
        let mut constant_paths = Vec::new();
        for path in &owner.constants {
            if selected_set.contains(path) {
                let source = self
                    .definitions
                    .iter()
                    .find(|d| d.product.source.definition == *path)
                    .ok_or(QueryError::NotImplemented)?;
                constants.insert(
                    path.clone(),
                    source.product.source.constant.clone().ok_or_else(|| {
                        invalid(format!("constant {path} needs accepted closed arguments"))
                    })?,
                );
                constant_paths.push(path.clone());
            } else if !owner.definitions.inputs_contains(path) {
                let value = owner
                    .registry
                    .dictionary_value(signatures.types(), &owner.memo, path)?
                    .ok_or_else(|| invalid(format!("missing registry constant producer {path}")))?;
                constants.insert(path.clone(), value);
                constant_paths.push(path.clone());
            }
        }
        let definitions = order_constants(constant_paths, |path| {
            if !selected_set.contains(path) {
                return Ok(Vec::new());
            }
            let reads = owner
                .definitions
                .constant_reads(path)
                .ok_or_else(|| invalid(format!("missing actual Evaluate reads for {path}")))?;
            Ok(reads
                .iter()
                .filter_map(|(key, _)| match key {
                    ReadKey::ExecutedBody(id) => Some(id.path().clone()),
                    _ => None,
                })
                .collect())
        })?;
        let (staged, staged_bytes) = owner
            .definitions
            .staged_static_inputs(&selected_set, owner.request().free())?;
        charge
            .grow(staged_bytes)
            .map_err(|_| invalid("native staged output admission"))?;
        constants.extend(staged.constants);
        let extra = crate::generic_graph::native_materialized(
            signatures.types(),
            constants
                .iter()
                .filter(|(path, _)| !source_constants.contains(*path))
                .map(|(path, value)| (path.clone(), value.clone())),
            &mut |path| {
                if !selected_set.contains(path) {
                    return Ok(None);
                }
                let product = self
                    .definitions
                    .iter()
                    .find(|d| d.product.source.definition == *path)
                    .ok_or(QueryError::NotImplemented)?;
                Ok(Some(
                    product
                        .product
                        .source
                        .source
                        .syntax
                        .owner()
                        .site(hirsel_types::NameSite::Item),
                ))
            },
        )?;
        let statics = crate::NativeStaticInputs {
            constants,
            definitions,
            generated_signatures: staged.generated_signatures,
        };
        let catalogue = self
            .definitions
            .iter()
            .map(|o| o.product.clone())
            .collect::<Vec<_>>();
        let originals = self
            .definitions
            .iter()
            .filter_map(|o| o.original.clone())
            .collect::<Vec<_>>();
        let bytes = owner
            .records
            .encode(
                &*anchors,
                &(
                    &roots.definitions,
                    roots.registry_revision,
                    &statics.constants,
                    &statics.definitions,
                    &statics.generated_signatures,
                    &extra,
                    (
                        run_product.address.0,
                        run_product.action.0,
                        run_product
                            .archive(ArchiveAdmission::Retained)
                            .map_err(|error| invalid(format!("native Run archive: {error}")))?
                            .reference(),
                    ),
                    catalogue
                        .iter()
                        .map(|d| {
                            d.product
                                .as_ref()
                                .map(|p| {
                                    owner.definitions.export_archive(p, scratch).map(|archive| {
                                        (p.address.0, p.action.0, archive.reference())
                                    })
                                })
                                .transpose()
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            )
            .ok_or_else(|| invalid("native root archive encoding"))?;
        if bytes.len() as u64 > owner.request().free() {
            return Err(invalid("native root outputs exceed admission"));
        }
        let record = owner
            .store
            .put_content(OUTPUT, &bytes)
            .ok_or(QueryError::NotImplemented)?;
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.native-root-action.v1",
            [id.address.0.0.as_slice(), record.output.0.0.as_slice()],
        ));
        let product = ProductRef::import_archive(
            id.address,
            action,
            ProductArchive::from_retained_bytes(
                record,
                bytes.into(),
                owner.ownership._allowance.budget(),
            )
            .map_err(|_| QueryError::NotImplemented)?,
        );
        // Keep the producer template undecorated: an external map or file
        // can outlive this root, without the owner retaining itself in a cycle.
        roots.sources = roots.sources.retain_owner(owner.ownership.clone());
        let products = FrontNativeProducts {
            roots,
            statics,
            catalogue,
            extra,
            originals,
            original_context,
            signatures,
            semantic: run.semantic.clone(),
            run,
            _charge: charge,
            _owner: owner.ownership.clone(),
        };
        *owner.completed.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((product.clone(), Arc::new(products)));
        owner
            .store
            .associate_product(product.address, product.action, record);
        Ok(product)
    }
}
pub(crate) fn order_constants(
    paths: Vec<DefPath>,
    mut dependencies: impl FnMut(&DefPath) -> Result<Vec<DefPath>, QueryError>,
) -> Result<Vec<DefPath>, QueryError> {
    let ranks = paths
        .iter()
        .enumerate()
        .map(|(i, path)| (path.clone(), i))
        .collect::<BTreeMap<_, _>>();
    let mut users = vec![Vec::new(); paths.len()];
    let mut counts = vec![0; paths.len()];
    for (i, path) in paths.iter().enumerate() {
        for dependency in dependencies(path)? {
            if let Some(&dependency) = ranks.get(&dependency).filter(|&&d| d != i) {
                users[dependency].push(i);
                counts[i] += 1;
            }
        }
    }
    let mut ready = counts
        .iter()
        .enumerate()
        .filter_map(|(i, &n)| (n == 0).then_some(i))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(paths.len());
    while let Some(i) = ready.pop_first() {
        ordered.push(paths[i].clone());
        for &user in &users[i] {
            counts[user] -= 1;
            if counts[user] == 0 {
                ready.insert(user);
            }
        }
    }
    if ordered.len() != paths.len() {
        return Err(QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "constant initialization".into(),
            message: "cyclic materialized constant dependencies".into(),
        }));
    }
    Ok(ordered)
}
fn invalid(message: impl Into<String>) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "admitted native source root".into(),
        message: message.into(),
    })
}
impl QueryEngine {
    /// Written roots selected by a native export with no explicit root list.
    pub fn front_native_source_roots(&self) -> Vec<DefPath> {
        self.source_paths()
            .into_iter()
            .filter(|path| !self.package_manifests.contains_key(&path.0.package))
            .collect()
    }
    pub fn front_native_queries(
        &mut self,
        plan: FrontNativePlan,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<FrontNativeQueries>, QueryError> {
        if !self.staged_values.is_empty() {
            return Err(invalid(
                "native staged products need their accepted immutable registration view",
            ));
        }
        let all = self.source_paths();
        let mut seeds = if plan.roots.is_empty() {
            self.front_native_source_roots()
        } else {
            plan.roots
        };
        seeds.sort();
        seeds.dedup();
        if seeds.is_empty() {
            return Err(invalid("native source root set is empty"));
        }
        let required = FrontNativeQueries::header_bytes(&all)
            .ok_or_else(|| invalid("native root input overflow"))?;
        let run = plan
            .run
            .with_source_roots(seeds.clone())?
            .with_selected_run_root(&plan.entry)?;
        let definitions = plan
            .definitions
            .with_selection(run.clone(), plan.entry.clone())?;
        if !Arc::ptr_eq(&definitions.signatures(), &self.signature_inputs()) {
            return Err(invalid(
                "native roots differ from the pinned signature snapshot",
            ));
        }
        let registry = hirsel_registry::BuiltinRegistry::shared();
        let template = crate::NativeSourceRoots {
            definitions: seeds.clone(),
            environment: self.environment.clone(),
            registry_revision: registry.revision(),
            operations: registry.operation_inventory(),
            builtins: self.native_builtin_signatures(),
            sources: self.sources.clone(),
            foreign_links: self.foreign_links.clone(),
            // The Run decides PROG-4 from the entry's foreign use.
            foreign_unreplayable: false,
            foreign_threads: self.foreign_threads.clone(),
        };
        let mut declarations = Vec::new();
        for path in &all {
            let settled = definitions
                .signatures()
                .header_products()
                .and_then(|headers| headers.settled_signature(DefId::intern(path)));
            let constant = match settled {
                Some(signature) => matches!(signature.kind, SignatureKind::Const { .. }),
                None => {
                    self.source_facts(path)
                        .ok_or_else(|| invalid(format!("native declaration facts: {path}")))?
                        .body_kind
                        == BodyKind::Constant
                }
            };
            if !constant {
                continue;
            }
            let span = self
                .items
                .get(path)
                .and_then(|items| items.first())
                .map(|syntax| syntax.item.name_span)
                .or_else(|| self.catalog.site(path))
                .or_else(|| self.captured_text(path).map(|text| text.site))
                .ok_or_else(|| invalid(format!("native constant declaration site: {path}")))?;
            let site = self.sources.resolve(span);
            let file = self
                .sources
                .file(site.file)
                .ok_or(QueryError::NotImplemented)?;
            declarations.push((file.path.clone(), site.start, path.clone()));
        }
        declarations.sort();
        let mut constants = self
            .registry
            .signatures()
            .filter(|s| matches!(s.kind, SignatureKind::Const { .. }))
            .map(|s| s.definition.path().clone())
            .collect::<Vec<_>>();
        constants.extend(declarations.into_iter().map(|(_, _, path)| path));
        let required = required
            .checked_add(
                metadata_bytes(
                    &template.operations,
                    template.builtins.keys(),
                    constants.iter(),
                )
                .ok_or_else(|| invalid("native root metadata overflow"))?,
            )
            .ok_or_else(|| invalid("native root input overflow"))?;
        if required > allowance.bytes() {
            return Err(invalid("native root inputs exceed admission"));
        }
        let store = self
            .result_store
            .clone()
            .ok_or(QueryError::NotImplemented)?;
        let query = QueryId {
            kind: FRONT_NATIVE_ROOT_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.native-root-address.v1",
                [
                    self.environment_value().0.as_slice(),
                    serde_json::to_vec(&seeds)
                        .map_err(|_| invalid("root selection encoding"))?
                        .as_slice(),
                    plan.entry.fingerprint().0.as_slice(),
                ],
            )),
        };
        Ok(Arc::new(FrontNativeQueries {
            query,
            entry: plan.entry,
            template,
            constants,
            registry: self.registry.clone(),
            memo: self.memo.clone(),
            run: run.clone(),
            definitions: definitions.clone(),
            records: crate::NativeRecords::new(self.types.clone(), store.clone()),
            anchors: crate::NativeRecordContext::capture(self),
            store,
            completed: Mutex::new(None),
            accepted: Mutex::new(None),
            failure: Mutex::new(None),
            weight,
            errors,
            ownership: Arc::new(Ownership {
                _run: run.clone(),
                _definitions: definitions.clone(),
                _allowance: Arc::new(allowance),
            }),
        }))
    }
}

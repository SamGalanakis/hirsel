//! Cold native instance continuations. The shared registry stores only request
//! and completed descriptor metadata; each poll owns its importer and source
//! reader. No source/native work runs under its lock or in factory creation.
use super::{
    demand::{DemandInstances, DiscoveredInstance, Request, StoredInstance},
    instance::InstanceQueries,
    source_inputs::{NativeFrontLower, NativeInstanceInputs, NativeSourceProvider},
    *,
};
use hirsel_engine_runtime::*;
use hirsel_engine_store::ResultStore;
use hirsel_engine_store::{Action, Address, Domain};

use std::{
    collections::BTreeMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub const INSTANCE_INPUT_KIND: u32 = 0x4e00_0020;
pub const DISCOVERY_KIND: u32 = 0x4e00_0021;
const FAILURE: Domain = Domain {
    kind: 0x4e00_001e,
    schema: 1,
};
type Index = BTreeMap<ContentHash, Arc<Registered>>;
type ProductIdentity = (Address, Action, Reference);
fn product_identity(product: &ProductRef) -> Result<ProductIdentity, String> {
    Ok((
        product.address,
        product.action,
        product
            .archive(ArchiveAdmission::Retained)
            .map_err(|e| e.to_string())?
            .reference(),
    ))
}
struct Registered {
    ordinal: usize,
    address: ContentHash,
    key: Reference,
    request: StoredRequest,
    input: OnceLock<ProductIdentity>,
    output: OriginalOutput,
}
impl Registered {
    fn key(&self, store: &dyn ResultStore) -> Result<InstanceAddress, String> {
        let record: super::dispatch::AddressRecord = super::dispatch::read(store, self.key)?;
        let key = record.bind()?;
        if key.address(INSTANCE_KIND).0 != self.address {
            return Err("registered owner identity differs".into());
        }
        Ok(key)
    }
}
const REQUEST_ARCHIVE: Domain = Domain {
    kind: INSTANCE_INPUT_KIND,
    schema: 2,
};
#[derive(Debug)]
struct StoredRequest {
    record: Reference,
    source: bool,
    receipts: bool,
}
impl StoredRequest {
    fn capture(store: &dyn ResultStore, request: Request) -> Result<Self, String> {
        let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        let record = store
            .put_content(REQUEST_ARCHIVE, &bytes)
            .ok_or("native request archive publication failed")?;
        if REQUEST_ARCHIVE.reference(&bytes) != Some(record) {
            return Err("native request archive identity differs".into());
        }
        Ok(Self {
            record,
            source: request.source_definition().is_some(),
            receipts: request.has_receipt_roots(),
        })
    }
    fn open(&self, store: &dyn ResultStore) -> Result<Request, String> {
        let bytes = store
            .get_content(self.record)
            .ok_or("missing native request archive")?;
        if REQUEST_ARCHIVE.reference(&bytes) != Some(self.record) {
            return Err("native request archive identity differs".into());
        }
        let request: Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if request.source_definition().is_some() != self.source
            || request.has_receipt_roots() != self.receipts
        {
            return Err("native request archive flags differ".into());
        }
        Ok(request)
    }
}
#[derive(Default)]
struct OriginalOutput {
    selected: OnceLock<OriginalSelection>,
}
struct OriginalSelection {
    product: ProductIdentity,
    record: Reference,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct OriginalRecord {
    output: super::demand::window::StoredRecord,
    dependencies: Box<[ContentHash]>,
    callable: bool,
}
impl OriginalOutput {
    fn read(&self, store: &dyn ResultStore) -> Result<Option<OriginalRecord>, String> {
        self.selected
            .get()
            .map(|s| super::dispatch::read(store, s.record))
            .transpose()
    }
    fn get(&self, store: &dyn ResultStore) -> Result<Option<StoredInstance>, String> {
        self.read(store)?
            .map(|s| StoredInstance::from_record(store, s.output))
            .transpose()
    }
    fn holds(&self, product: &ProductRef) -> bool {
        self.selected.get().is_some_and(|selected| {
            selected.product.0 == product.address
                && selected.product.1 == product.action
                && product
                    .archive(ArchiveAdmission::Retained)
                    .is_ok_and(|archive| archive.reference() == selected.product.2)
        })
    }
    fn set(&self, output: &DiscoveredInstance, store: &dyn ResultStore) -> Result<(), String> {
        let row = OriginalRecord {
            output: StoredInstance::capture(store, output)?.record()?,
            dependencies: output
                .facts
                .body
                .dependencies
                .iter()
                .map(|key| key.address(INSTANCE_KIND).0)
                .collect(),
            callable: output.facts.body.templates.execution,
        };
        let record = super::dispatch::put(store, &row)?;
        let product = &output.product.product;
        let selected = OriginalSelection {
            product: (
                product.address,
                product.action,
                product
                    .archive(ArchiveAdmission::Retained)
                    .map_err(|e| e.to_string())?
                    .reference(),
            ),
            record,
        };
        if let Err(other) = self.selected.set(selected) {
            let previous = self.selected.get().unwrap();
            let old: OriginalRecord = super::dispatch::read(store, previous.record)?;
            if previous.product != other.product
                || old.dependencies != row.dependencies
                || old.callable != row.callable
            {
                return Err("native instance completion changed".into());
            }
        }
        Ok(())
    }
}
struct SourceBinding {
    selected: (Address, Action),
    original: hirsel_engine::NativeOriginalProduct,
}
struct Registry {
    sources: BTreeMap<hirsel_syntax::DefPath, Arc<SourceBinding>>,
    entries: Index,
    inputs: Index,
    charged: u64,
}
/// Source production must already have selected actual current V envelopes.
/// Cold source production under VerifyOff/recomputation remains a separate
/// source task; these native tasks never infer a missing source observation.
pub struct InstanceTasks {
    source: Arc<NativeInstanceInputs>,
    front: Option<NativeFrontLower>,
    store: Arc<dyn ResultStore>,
    options: hirsel_native::Options,
    verify_store: hirsel_engine_store::VerifyStore,
    policy: ContentHash,
    native: Arc<hirsel_lir::validate::NativeCache>,
    registry: Mutex<Registry>,
    roots: Vec<InstanceAddress>,
    discovery: QueryId,
    completed: OnceLock<ProductRef>,
    original_discovery: OnceLock<(ProductRef, bool)>,
    callables: OnceLock<(ProductRef, super::demand::callable::PublishedCallables)>,
    window: Arc<super::demand::window::WindowBudget>,
    weight: MemoryWeight,
    allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    fallback: Reference,
    pub imports: AtomicU64,
    pub recipe_decodes: AtomicU64,
    pub source_body_demands: AtomicU64,
}
fn instance_id(key: &InstanceAddress) -> QueryId {
    QueryId {
        kind: INSTANCE_KIND,
        address: key.address(INSTANCE_KIND),
    }
}
fn input_id(key: &InstanceAddress) -> QueryId {
    QueryId {
        kind: INSTANCE_INPUT_KIND,
        address: key.address(INSTANCE_INPUT_KIND),
    }
}
impl InstanceTasks {
    pub fn new(
        source: Arc<NativeInstanceInputs>,
        store: Arc<dyn ResultStore>,
        options: hirsel_native::Options,
        policy: ContentHash,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Arc<Self>, String> {
        Self::with_library_roots(source, store, options, policy, weight, allowance, &[])
    }
    pub fn with_library_roots(
        source: Arc<NativeInstanceInputs>,
        store: Arc<dyn ResultStore>,
        options: hirsel_native::Options,
        policy: ContentHash,
        weight: MemoryWeight,
        mut allowance: hirsel_engine_admission::MemoryReservation,
        library: &[hirsel_rc::library::Request],
    ) -> Result<Arc<Self>, String> {
        source.retain_metadata(&mut allowance)?;
        let provider = source.provider();
        let native = InstanceQueries::validator(&store);
        let mut initial = DemandInstances::with_validator(
            &provider,
            store.clone(),
            options.target.clone(),
            policy,
            native.clone(),
        );
        let roots = initial.roots_with_library(&provider, library)?;
        let selection = serde_json::to_vec(
            &roots
                .iter()
                .map(|key| key.address(INSTANCE_KIND).0)
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?;
        let discovery = QueryId {
            kind: DISCOVERY_KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.native.discovery-address.v1",
                [selection.as_slice()],
            )),
        };
        let fallback = store
            .put_content(
                FAILURE,
                b"native instance task resource or protocol refusal",
            )
            .ok_or("native task failure publication failed")?;
        let root_bytes = roots.len().saturating_mul(size_of::<Arc<Registered>>());
        if weight.retained < root_bytes as u64 + std::mem::size_of::<DiscoveryTask>() as u64
            || weight.retained < std::mem::size_of::<InstanceTask>() as u64
            || weight.retained < std::mem::size_of::<DiscoveryTask>() as u64
        {
            return Err("native task frame exceeds retained allowance".into());
        }
        let mut metadata = hirsel_query_products::retained::MetadataWeight::default();
        metadata.add(size_of::<Self>() + 2 * size_of::<usize>());
        metadata.add(super::demand::window::WindowBudget::retained_bytes() as usize);
        metadata
            .add(size_of::<hirsel_engine_admission::MemoryReservation>() + 2 * size_of::<usize>());
        metadata.vec(&roots, |w, key| {
            w.path(key.instance.definition());
            w.add(key.instance.retained_encoding_bytes());
        });
        if metadata.bytes() > allowance.bytes() {
            return Err("native task metadata exceeds retained allowance".into());
        }
        let allowance = Arc::new(allowance);
        let window = super::demand::window::WindowBudget::new(allowance.clone());
        window.set_base(metadata.bytes())?;
        let tasks = Arc::new(Self {
            window,
            source,
            front: None,
            store,
            options,
            verify_store: hirsel_engine_store::VerifyStore::Off,
            policy,
            native,
            roots,
            discovery,
            registry: Mutex::new(Registry {
                sources: BTreeMap::new(),
                entries: BTreeMap::new(),
                inputs: BTreeMap::new(),
                charged: metadata.bytes(),
            }),
            completed: OnceLock::new(),
            original_discovery: OnceLock::new(),
            callables: OnceLock::new(),
            weight,
            allowance,
            fallback,
            imports: AtomicU64::new(0),
            recipe_decodes: AtomicU64::new(0),
            source_body_demands: AtomicU64::new(0),
        });
        tasks.register(initial.registered_requests()?)?;
        Ok(tasks)
    }
    /// Bind explicit host cache verification before this registry is shared
    /// with factories. Every worker-local Instance producer receives the policy.
    pub fn with_store_verification(
        mut self: Arc<Self>,
        policy: hirsel_engine_store::VerifyStore,
    ) -> Result<Arc<Self>, String> {
        let tasks = Arc::get_mut(&mut self)
            .ok_or("native tasks were shared before store verification binding")?;
        tasks.verify_store = policy;
        Ok(self)
    }
    /// Attach immutable front dispatch before any factory shares this request.
    /// The explicit Lower read replaces eager completion for this request.
    pub fn with_front_lower(
        mut self: Arc<Self>,
        front: NativeFrontLower,
    ) -> Result<Arc<Self>, String> {
        for path in front.bindings.keys() {
            if !self.source.declarations.sources.contains_key(path) {
                return Err("front Lower binding has no native source declaration".into());
            }
        }
        let tasks =
            Arc::get_mut(&mut self).ok_or("native tasks were shared before front binding")?;
        if tasks.front.is_some() {
            return Err("native front Lower dispatch is already bound".into());
        }
        let mut metadata = hirsel_query_products::retained::MetadataWeight::default();
        metadata.map(&front.bindings, |w, path, _| w.path(path));
        let mut registry = tasks.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.charged = registry
            .charged
            .checked_add(metadata.bytes())
            .filter(|total| *total <= tasks.allowance.bytes())
            .ok_or("native front dispatch metadata exceeds retained allowance")?;
        tasks.window.set_base(registry.charged)?;
        drop(registry);
        tasks.front = Some(front);
        Ok(self)
    }
    /// Completed original read caches, separate from the live dispatch/source
    /// identities. Callable handoff releases these bytes at their last use.
    pub fn completed_read_cache_bytes(&self) -> u64 {
        0
    }
    pub fn metadata_peak(&self) -> (u64, u64) {
        self.window.peak()
    }
    pub fn read_completed(
        &self,
        key: &InstanceAddress,
        stored: &StoredInstance,
    ) -> Result<Arc<DiscoveredInstance>, String> {
        stored.read(&self.producer(), key)
    }
    pub fn retained_allowance(&self) -> Arc<hirsel_engine_admission::MemoryReservation> {
        self.allowance.clone()
    }
    /// Keep copied source declarations charged when a settled native snapshot
    /// outlives this task registry. The registry allowance is a separate slice.
    pub fn retained_source_allowance(&self) -> Arc<hirsel_engine_admission::MemoryReservation> {
        self.source
            .metadata_allowance()
            .expect("instance source metadata admitted at construction")
    }
    fn producer(&self) -> DemandInstances {
        DemandInstances::with_validator(
            &self.source.provider(),
            self.store.clone(),
            self.options.target.clone(),
            self.policy,
            self.native.clone(),
        )
        .with_store_verification(self.verify_store)
        .with_retained_owner(self.allowance.clone())
        .with_window(self.window.clone())
    }
    fn register(&self, requests: Vec<(InstanceAddress, Request)>) -> Result<(), String> {
        // Encoding/validation runs outside the index lock. admit() has already
        // checked recipe domains and presence through the sole demand core.
        let entries = requests
            .into_iter()
            .map(|(key, request)| {
                let mut weight = hirsel_query_products::retained::MetadataWeight::default();
                weight.add(size_of::<Registered>() + 2 * size_of::<usize>());
                let charge = weight.bytes();
                let request = StoredRequest::capture(self.store.as_ref(), request)?;
                Ok((
                    key.address(INSTANCE_KIND).0,
                    key.address(INSTANCE_INPUT_KIND).0,
                    charge,
                    Arc::new(Registered {
                        ordinal: 0,
                        address: key.address(INSTANCE_KIND).0,
                        key: super::dispatch::put(
                            self.store.as_ref(),
                            &super::dispatch::AddressRecord::capture(&key),
                        )?,
                        request,
                        input: OnceLock::new(),
                        output: OriginalOutput::default(),
                    }),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let index_bytes = |len| {
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.tree::<ContentHash, Arc<Registered>>(len);
            weight.bytes().saturating_mul(2)
        };
        for (id, input, mut charge, mut entry) in entries {
            if let Some(previous) = registry.entries.get(&id) {
                if previous.key != entry.key || previous.request.record != entry.request.record {
                    return Err(format!(
                        "inconsistent native request registration for {:?}: previous={:?}, requested={:?}",
                        entry.address, previous.request, entry.request,
                    ));
                }
                continue;
            }
            charge = charge.saturating_add(index_bytes(registry.entries.len() + 1).saturating_sub(
                if registry.entries.is_empty() {
                    0
                } else {
                    index_bytes(registry.entries.len())
                },
            ));
            if registry.entries.len() >= hirsel_rc::instance::MAX_INSTANCES
                || charge > self.allowance.bytes().saturating_sub(registry.charged)
            {
                return Err("native discovery metadata allowance exhausted".into());
            }
            Arc::get_mut(&mut entry)
                .expect("unpublished native registration")
                .ordinal = registry.entries.len();
            registry.charged += charge;
            self.window.set_base(registry.charged)?;
            registry.inputs.insert(input, entry.clone());
            registry.entries.insert(id, entry);
        }
        Ok(())
    }
    fn registered(&self, key: &InstanceAddress) -> Result<Arc<Registered>, String> {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(&key.address(INSTANCE_KIND).0)
            .cloned()
            .ok_or("unregistered native instance".into())
    }
    fn entries(&self) -> Vec<Arc<Registered>> {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .values()
            .cloned()
            .collect()
    }
    fn binding_refusal(
        &self,
        registry: &Registry,
        requested: u64,
        owner: &hirsel_syntax::DefPath,
    ) -> String {
        format!(
            "native source binding admission refused for {owner}: requested {requested}, retained {}, quota {}",
            registry.charged,
            self.allowance.bytes()
        )
    }
    fn bind_source(
        &self,
        selected: (Address, Action),
        original: hirsel_engine::NativeOriginalProduct,
    ) -> Result<(), String> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = registry.sources.get(&original.definition) {
            if previous.selected != selected || !original.same_binding(&previous.original) {
                return Err("front Lower changed within a native input".into());
            }
            return Ok(());
        }
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.path(&original.definition);
        weight.path(&original.definition);
        weight.add(size_of::<SourceBinding>() + 2 * size_of::<usize>());
        weight.add(
            original
                .body
                .retained_metadata_bytes()
                .and_then(|bytes| usize::try_from(bytes).ok())
                .ok_or("native source binding metadata overflow")?,
        );
        let nodes = |len| {
            let mut w = hirsel_query_products::retained::MetadataWeight::default();
            w.tree::<hirsel_syntax::DefPath, Arc<SourceBinding>>(len);
            w.bytes()
        };
        let charge =
            weight
                .bytes()
                .saturating_add(nodes(registry.sources.len() + 1).saturating_sub(
                    if registry.sources.is_empty() {
                        0
                    } else {
                        nodes(registry.sources.len())
                    },
                ));
        registry.charged = registry
            .charged
            .checked_add(charge)
            .filter(|bytes| *bytes <= self.allowance.bytes())
            .ok_or_else(|| self.binding_refusal(&registry, charge, &original.definition))?;
        self.window.set_base(registry.charged)?;
        registry.sources.insert(
            original.definition.clone(),
            Arc::new(SourceBinding { selected, original }),
        );
        Ok(())
    }
    fn put(&self, key: QueryId, bytes: Vec<u8>) -> Result<ProductRef, String> {
        let domain = Domain {
            kind: key.kind,
            schema: 1,
        };
        let record = self
            .store
            .put_content(domain, &bytes)
            .ok_or("native task output transport failed")?;
        if domain.reference(&bytes) != Some(record) {
            return Err("native task output content identity differs".into());
        }
        let action = Action(ContentHash::of_parts(
            "hirsel.native.task-output-action.v1",
            [key.address.0.0.as_slice(), record.output.0.0.as_slice()],
        ));
        self.store.associate_product(key.address, action, record);
        Ok(ProductRef::import_archive(
            key.address,
            action,
            self.store
                .admit_product_archive(record, bytes.into())
                .ok_or("native product archive is unavailable or exceeds admission")?,
        ))
    }
    pub fn failure(&self, message: &str) -> Reference {
        self.store
            .put_content(FAILURE, message.as_bytes())
            .unwrap_or(self.fallback)
    }
    /// Reopen a bounded native diagnostic. This conveys no product or verifier
    /// authority and never reruns source, instance, C or Ready work.
    pub fn failure_message(&self, reference: Reference) -> Option<String> {
        if reference.domain != FAILURE || reference.len > self.weight.retained {
            return None;
        }
        let bytes = self.store.get_content(reference)?;
        if FAILURE.reference(&bytes) != Some(reference) {
            return None;
        }
        std::str::from_utf8(&bytes).ok().map(str::to_owned)
    }
    pub fn source_inputs(&self) -> &NativeInstanceInputs {
        &self.source
    }
    pub fn options(&self) -> &hirsel_native::Options {
        &self.options
    }
    pub fn policy(&self) -> ContentHash {
        self.policy
    }
    pub fn selected_store(&self) -> &Arc<dyn ResultStore> {
        &self.store
    }
    /// Current independent source V rows for all actual completed source
    /// selections, assembled without source or candidate native body reads.
    fn current_source(&self) -> Result<super::source_inputs::FrozenNativeSource, String> {
        let source = if let Some(front) = &self.front {
            let products = self
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .sources
                .values()
                .map(|binding| binding.original.clone())
                .collect::<Vec<_>>();
            self.source
                .provider_from_products(&front.context, products)?
        } else {
            self.source.provider()
        };
        Ok(source)
    }
    pub fn source_operations(
        &self,
        settled: ProductRef,
    ) -> Result<hirsel_layout::operation::OperationRegistry, String> {
        if self.completed.get() != Some(&settled) {
            return Err("source operation inputs require settled native discovery".into());
        }
        let mut operations = self.current_operations()?;
        if let Some((_, graph)) = self.callables.get() {
            let mut source = self.current_source()?;
            for (key, _) in graph.outputs(self.store.as_ref())? {
                if !self
                    .source
                    .declarations
                    .sources
                    .contains_key(key.instance.definition())
                {
                    continue;
                }
                // A charged instance (STORE-11 energy) is selected through its base.
                let base = key
                    .instance
                    .uncharged()
                    .unwrap_or_else(|| key.instance.clone());
                let symbol = hirsel_lir::ir::Symbol {
                    kind: hirsel_lir::ir::SymbolKind::Function,
                    definition: key.instance.definition().clone(),
                    instance: Some(base.digest()),
                    charged: false,
                };
                let (evidence, _) = source.source_invocations(
                    key.instance.definition(),
                    &symbol,
                    hirsel_tir::BodyId(base.body()),
                )?;
                operations
                    .invocations
                    .insert(evidence.body.definition.clone(), evidence.invocations);
                operations.invocation_bodies.retain(|row| {
                    row.definition != evidence.body.definition
                        || row.instance != evidence.body.instance
                });
                operations.invocation_bodies.push(evidence.body);
            }
        }
        Ok(operations)
    }
    fn current_operations(&self) -> Result<hirsel_layout::operation::OperationRegistry, String> {
        let entries = self.entries();
        let mut source = self.current_source()?;
        let mut registry = self.source.declarations.program.operation_registry();
        let requests = entries
            .iter()
            .map(|entry| entry.request.open(self.store.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        self.producer()
            .bind_planned_adapters(&source, requests.iter(), &mut registry)?;
        for entry in entries {
            let key = entry.key(self.store.as_ref())?;
            let Some(path) = (entry.request.source).then(|| key.instance.definition()) else {
                continue;
            };
            // A charged instance (STORE-11 energy) is selected through its base.
            let base = key
                .instance
                .uncharged()
                .unwrap_or_else(|| key.instance.clone());
            let selected = hirsel_lir::ir::Symbol {
                kind: hirsel_lir::ir::SymbolKind::Function,
                definition: key.instance.definition().clone(),
                instance: Some(base.digest()),
                charged: false,
            };
            let (evidence, _) =
                source.source_invocations(path, &selected, hirsel_tir::BodyId(base.body()))?;
            if let Some(previous) = registry.invocations.insert(
                evidence.body.definition.clone(),
                evidence.invocations.clone(),
            ) && previous != evidence.invocations
            {
                return Err("source invocation inventories differ within the request".into());
            }
            if !registry.invocation_bodies.contains(&evidence.body) {
                registry.invocation_bodies.push(evidence.body);
            }
        }
        registry
            .invocation_bodies
            .sort_by(|a, b| (&a.definition, a.instance).cmp(&(&b.definition, b.instance)));
        Ok(registry)
    }
    pub fn discovery_id(&self) -> QueryId {
        self.discovery
    }
    pub fn roots(&self) -> &[InstanceAddress] {
        &self.roots
    }
    /// Snapshot assembly reads only the actual settled discovery output. No
    /// dependency/recipe/Value work is hidden here; consumers independently
    /// re-admit C using the shared snapshot-private native cache.
    pub fn products(&self, settled: ProductRef) -> Result<DemandInstances, String> {
        if self.completed.get() != Some(&settled) {
            return Err("native discovery is not settled at this output".into());
        }
        if let Some((_, scope)) = self.callables.get() {
            let mut products = self.producer();
            scope.apply(&mut products)?;
            products.recheck_receipts(&self.current_source()?)?;
            products.bind_original_roots(&self.roots)?;
            return Ok(products);
        }
        self.original_products()
    }
    fn original_products(&self) -> Result<DemandInstances, String> {
        let mut products = self.producer();
        let entries = self.entries();
        for entry in &entries {
            products.admit(entry.request.open(self.store.as_ref())?)?;
        }
        for entry in entries {
            let output = entry
                .output
                .get(self.store.as_ref())?
                .ok_or("native discovery has an unfinished instance")?;
            products.attach_stored(&entry.key(self.store.as_ref())?, output)?;
        }
        products.recheck_receipts(&self.current_source()?)?;
        products.bind_original_roots(&self.roots)?;
        Ok(products)
    }
    pub fn completed_instances(
        &self,
        settled: ProductRef,
    ) -> Result<Vec<(InstanceAddress, StoredInstance)>, String> {
        if self.completed.get() != Some(&settled) {
            return Err("native discovery has not completed".into());
        }
        if let Some((_, scope)) = self.callables.get() {
            return scope.outputs(self.store.as_ref());
        }
        let mut outputs = self
            .entries()
            .into_iter()
            .map(|entry| {
                let output = entry
                    .output
                    .get(self.store.as_ref())?
                    .ok_or("native instance has not completed")?
                    .clone();
                Ok((entry.key(self.store.as_ref())?, output))
            })
            .collect::<Result<Vec<_>, String>>()?;
        outputs.sort_by(|a, b| a.0.canonical_cmp(&b.0));
        Ok(outputs)
    }
    /// Actual independently compared check removals for this complete native
    /// discovery. Ordinary per-instance facts cannot certify a partial graph.
    pub fn discharged(
        &self,
        settled: ProductRef,
    ) -> Result<hirsel_lir::discharge::Discharge, String> {
        let completed = self.completed_instances(settled)?;
        let facts = completed
            .iter()
            .map(|(key, stored)| {
                self.read_completed(key, stored).map(|output| {
                    (
                        key.instance.definition().clone(),
                        output.facts.source.discharge.clone(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        super::discharge::complete(
            &self.options.discharge,
            facts.iter().map(|(key, facts)| (key, facts)),
        )
    }
}

pub struct InstanceTaskFactory {
    tasks: Arc<InstanceTasks>,
}
impl InstanceTaskFactory {
    pub fn new(tasks: Arc<InstanceTasks>) -> Self {
        Self { tasks }
    }
}
impl QueryFactory for InstanceTaskFactory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if id == self.tasks.callable_id()
            || id.kind == hirsel_native::query_model::CALLABLE_VALUE_KIND
        {
            return Ok(Box::new(CallableTask {
                tasks: self.tasks.clone(),
                id,
                terminal: None,
            }));
        }
        if id == self.tasks.discovery {
            return Ok(Box::new(DiscoveryTask {
                tasks: self.tasks.clone(),
                frontier: self
                    .tasks
                    .roots
                    .iter()
                    .map(|key| {
                        self.tasks
                            .registered(key)
                            .map_err(|e| self.tasks.failure(&e))
                    })
                    .collect::<Result<_, _>>()?,
                seen: Vec::new(),
                terminal: None,
            }));
        }
        let entry = {
            let registry = self
                .tasks
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let index = match id.kind {
                INSTANCE_KIND => &registry.entries,
                INSTANCE_INPUT_KIND => &registry.inputs,
                _ => return Err(self.tasks.fallback),
            };
            index
                .get(&id.address.0)
                .cloned()
                .ok_or(self.tasks.fallback)?
        };
        Ok(Box::new(InstanceTask {
            tasks: self.tasks.clone(),
            entry,
            id,
            terminal: None,
        }))
    }
}
struct InstanceTask {
    tasks: Arc<InstanceTasks>,
    entry: Arc<Registered>,
    id: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for InstanceTask {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        // Registry descriptors have their own reservation. This task retains
        // only Arc handles, its identity and a terminal result across polls.
        MemoryWeight {
            retained: std::mem::size_of::<Self>() as u64,
            ..self.tasks.weight
        }
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = self.terminal.clone() {
            return result;
        }
        let key = match self.entry.key(self.tasks.store.as_ref()) {
            Ok(key) => key,
            Err(error) => return Err(self.tasks.failure(&error)),
        };
        let mut key_weight = hirsel_query_products::retained::MetadataWeight::default();
        key_weight.path(key.instance.definition());
        key_weight.add(key.instance.retained_encoding_bytes());
        let _key_read = match self.tasks.window.read_for("owner-key", key_weight.bytes()) {
            Ok(lease) => lease,
            Err(error) => return Err(self.tasks.failure(&error)),
        };
        let mut receipt_products = Vec::new();
        let source_owner = self.entry.request.source.then(|| key.instance.definition());
        if let Some(front) = &self.tasks.front
            && ((source_owner.is_some() && self.id.kind == INSTANCE_INPUT_KIND)
                || self.entry.request.receipts)
        {
            let paths = if self.entry.request.receipts {
                self.tasks
                    .source
                    .declarations
                    .context
                    .sources
                    .keys()
                    .collect::<Vec<_>>()
            } else {
                source_owner.into_iter().collect()
            };
            for path in paths {
                let Some(query) = front.bindings.get(path).copied() else {
                    let error = self
                        .tasks
                        .failure("native source has no front Lower dependency");
                    self.terminal = Some(Err(error));
                    return Err(error);
                };
                let selected = match cx.read(query) {
                    Err(error) => {
                        self.terminal = Some(Err(error));
                        return Err(error);
                    }
                    Ok(product) if product.address == query.address => product,
                    _ => {
                        let error = self
                            .tasks
                            .failure("front Lower returned another query output");
                        self.terminal = Some(Err(error));
                        return Err(error);
                    }
                };
                let result = front
                    .products
                    .lower_product(path, selected.clone())
                    .and_then(|product| {
                        if &product.definition != path {
                            return Err("front Lower names another source definition".into());
                        }
                        if Some(path) == source_owner {
                            self.tasks.bind_source(
                                (selected.address, selected.action),
                                product.clone(),
                            )?;
                        }
                        receipt_products.push(product);
                        Ok(())
                    });
                if let Err(message) = result {
                    let error = self.tasks.failure(&message);
                    self.terminal = Some(Err(error));
                    return Err(error);
                }
            }
        }
        if self.id.kind == INSTANCE_KIND {
            match cx.read(input_id(&key)) {
                Err(error) => {
                    self.terminal = Some(Err(error));
                    return Err(error);
                }
                Ok(product)
                    if product_identity(&product)
                        .is_ok_and(|identity| self.entry.input.get() == Some(&identity)) => {}
                _ => {
                    let error = self
                        .tasks
                        .failure("native instance input differs from selected snapshot");
                    self.terminal = Some(Err(error));
                    return Err(error);
                }
            }
        }
        let result = (|| {
            let mut producer = self.tasks.producer();
            producer.admit(self.entry.request.open(self.tasks.store.as_ref())?)?;
            let mut source = if let Some(front) = &self.tasks.front
                && (source_owner.is_some() || self.entry.request.receipts)
            {
                let products = if self.entry.request.receipts {
                    receipt_products
                } else {
                    vec![
                        self.tasks
                            .registry
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .sources
                            .get(source_owner.unwrap())
                            .ok_or("source instance lost its settled Lower selection")?
                            .original
                            .clone(),
                    ]
                };
                self.tasks
                    .source
                    .provider_from_products(&front.context, products)?
            } else {
                self.tasks.source.provider()
            };
            if self.id.kind == INSTANCE_INPUT_KIND {
                let bytes = producer.producer_inputs(&source, &key, &self.tasks.options)?;
                let product = self.tasks.put(self.id, bytes)?;
                let identity = product_identity(&product)?;
                if self.entry.input.set(identity).is_err()
                    && self.entry.input.get() != Some(&identity)
                {
                    return Err("native selected input completion changed".into());
                }
                return Ok(product);
            }
            let output = producer.instance_from(&mut source, &key, &self.tasks.options)?;
            self.tasks
                .imports
                .fetch_add(producer.imports, Ordering::Relaxed);
            self.tasks
                .recipe_decodes
                .fetch_add(producer.recipe_decodes, Ordering::Relaxed);
            self.tasks
                .source_body_demands
                .fetch_add(source.body_demands, Ordering::Relaxed);
            self.tasks.register(producer.registered_requests()?)?;
            let product = output.product.product.clone();
            if self
                .entry
                .output
                .set(&output, self.tasks.store.as_ref())
                .is_err()
            {
                return Err("native instance completion changed".into());
            }
            Ok(product)
        })()
        .map_err(|error: String| self.tasks.failure(&error));
        self.terminal = Some(result.clone());
        result
    }
}
struct DiscoveryTask {
    tasks: Arc<InstanceTasks>,
    frontier: Vec<Arc<Registered>>,
    seen: Vec<u64>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for DiscoveryTask {
    fn id(&self) -> QueryId {
        self.tasks.discovery
    }
    fn weight(&self) -> MemoryWeight {
        self.tasks.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = self.terminal.clone() {
            return result;
        }
        let result = self.advance(cx);
        self.terminal = Some(result.clone());
        result
    }
}
// Ordinals are stable, dense slots in this immutable operation's registry.
// These bits record traversal only; reads still compare the exact ProductRef.
fn marked(bits: &[u64], ordinal: usize) -> bool {
    bits.get(ordinal / 64)
        .is_some_and(|word| word & (1 << (ordinal % 64)) != 0)
}
fn mark(bits: &mut Vec<u64>, ordinal: usize) -> bool {
    let word = ordinal / 64;
    if word >= bits.len() {
        bits.resize(word + 1, 0);
    }
    let mask = 1 << (ordinal % 64);
    let previous = bits[word] & mask != 0;
    bits[word] |= mask;
    previous
}
impl DiscoveryTask {
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let mut frontier = std::mem::take(&mut self.frontier);
        let mut read = Vec::new();
        while let Some(entry) = frontier.pop() {
            if marked(&self.seen, entry.ordinal) || mark(&mut read, entry.ordinal) {
                continue;
            }
            match cx.read(QueryId {
                kind: INSTANCE_KIND,
                address: Address(entry.address),
            }) {
                Err(error) => return Err(error),
                Ok(product) => {
                    let selected = entry
                        .output
                        .read(self.tasks.store.as_ref())
                        .map_err(|e| self.tasks.failure(&e))?
                        .ok_or_else(|| {
                            self.tasks
                                .failure("native task returned no completed descriptor")
                        })?;
                    if !entry.output.holds(&product) {
                        return Err(self.tasks.failure("native discovery read another output"));
                    }
                    mark(&mut self.seen, entry.ordinal);
                    let registry = self
                        .tasks
                        .registry
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    for address in &selected.dependencies {
                        let dependency = registry.entries.get(address).ok_or_else(|| {
                            self.tasks.failure("unregistered native discovery address")
                        })?;
                        frontier.push(dependency.clone());
                    }
                }
            }
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.add(size_of::<Self>());
            weight.flat_vec(&frontier);
            weight.flat_vec(&self.seen);
            weight.flat_vec(&read);
            if weight.bytes() > self.tasks.weight.retained {
                return Err(self
                    .tasks
                    .failure("native discovery frame exceeds retained allowance"));
            }
        }
        let (discovery, needs_callable) = self
            .tasks
            .original_discovery()
            .map_err(|e| self.tasks.failure(&e))?;
        if needs_callable {
            match cx.read(self.tasks.callable_id()) {
                Err(error) => return Err(error),
                Ok(product)
                    if self
                        .tasks
                        .callables
                        .get()
                        .is_some_and(|(selected, _)| *selected == product) =>
                {
                    ()
                }
                _ => {
                    return Err(self
                        .tasks
                        .failure("callable graph returned another product"));
                }
            }
        }
        let result = (|| {
            if self.tasks.completed.set(discovery.clone()).is_err()
                && self.tasks.completed.get() != Some(&discovery)
            {
                return Err("native discovery completion changed".into());
            }
            Ok(discovery)
        })();
        result.map_err(|e: String| self.tasks.failure(&e))
    }
}

pub const CALLABLE_GRAPH_KIND: u32 = 0x4e00_0033;
impl InstanceTasks {
    fn callable_id(&self) -> QueryId {
        QueryId {
            kind: CALLABLE_GRAPH_KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.native.callable-graph-address.v1",
                [self.discovery.address.0.0.as_slice()],
            )),
        }
    }
    fn original_discovery(&self) -> Result<(ProductRef, bool), String> {
        if let Some(selected) = self.original_discovery.get() {
            return Ok(selected.clone());
        }
        let entries = self.entries();
        let mut facts = Vec::new();
        let mut rows = Vec::new();
        let mut needs_callable = false;
        for entry in &entries {
            let key = entry.key(self.store.as_ref())?;
            let stored = entry
                .output
                .get(self.store.as_ref())?
                .ok_or("discovery output is unfinished")?;
            let output = self.read_completed(&key, &stored)?;
            facts.push((
                key.instance.definition().clone(),
                output.facts.source.discharge.clone(),
            ));
            needs_callable |= output.facts.body.templates.execution;
            rows.push((
                key.instance.encoding().to_vec(),
                output.product.product.address.0,
                output
                    .product
                    .product
                    .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                    .map_err(|e| e.to_string())?
                    .reference(),
            ));
        }
        super::discharge::complete(
            &self.options.discharge,
            facts.iter().map(|(key, facts)| (key, facts)),
        )?;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let product = self.put(
            self.discovery,
            serde_json::to_vec(&rows).map_err(|e| e.to_string())?,
        )?;
        self.original_discovery
            .set((product.clone(), needs_callable))
            .map_err(|_| "original discovery published twice")?;
        Ok((product, needs_callable))
    }
    fn callable_scope(&self) -> Result<ProductRef, String> {
        if let Some((product, _)) = self.callables.get() {
            return Ok(product.clone());
        }
        self.original_discovery()?;
        let mut products = self.original_products()?;
        let operations = self.current_operations()?;
        let manifest = self.source.declarations.manifest()?;
        let mut source = self.current_source()?;
        let graph = products.specialize_callables(
            &self.options,
            &manifest,
            operations,
            |key, operations| {
                if source
                    .declarations()
                    .sources
                    .contains_key(key.instance.definition())
                {
                    // A charged instance (STORE-11 energy) is selected through its base.
                    let base = key
                        .instance
                        .uncharged()
                        .unwrap_or_else(|| key.instance.clone());
                    let symbol = hirsel_lir::ir::Symbol {
                        kind: hirsel_lir::ir::SymbolKind::Function,
                        definition: key.instance.definition().clone(),
                        instance: Some(base.digest()),
                        charged: false,
                    };
                    let (evidence, _) = source.source_invocations(
                        key.instance.definition(),
                        &symbol,
                        hirsel_tir::BodyId(base.body()),
                    )?;
                    operations
                        .invocations
                        .insert(evidence.body.definition.clone(), evidence.invocations);
                    operations.invocation_bodies.retain(|row| {
                        row.definition != evidence.body.definition
                            || row.instance != evidence.body.instance
                    });
                    operations.invocation_bodies.push(evidence.body);
                }
                Ok(())
            },
        )?;
        let rows = graph
            .outputs(self.store.as_ref())?
            .into_iter()
            .map(|(key, output)| {
                Ok((
                    key.instance.encoding().to_vec(),
                    output
                        .product
                        .product
                        .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                        .map_err(|e| e.to_string())?
                        .reference(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let product = self.put(
            self.callable_id(),
            serde_json::to_vec(&rows).map_err(|e| e.to_string())?,
        )?;
        self.callables
            .set((product.clone(), graph))
            .map_err(|_| "callable graph published twice")?;
        if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
            let (peak, quota) = self.window.peak();
            eprintln!("NATIVE_WINDOW_PEAK\t{peak}\t{quota}");
            self.window.report("callable");
        }
        Ok(product)
    }
}

struct CallableTask {
    tasks: Arc<InstanceTasks>,
    id: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for CallableTask {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: std::mem::size_of::<Self>() as u64,
            ..self.tasks.weight
        }
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = self.terminal.clone() {
            return result;
        }
        if self.id == self.tasks.callable_id() {
            for entry in self.tasks.entries() {
                let query = QueryId {
                    kind: INSTANCE_KIND,
                    address: Address(entry.address),
                };
                match cx.read(query) {
                    Err(error) => {
                        self.terminal = Some(Err(error));
                        return Err(error);
                    }
                    Ok(product) if entry.output.holds(&product) => (),
                    _ => {
                        let error = self.tasks.failure("callable original output differs");
                        self.terminal = Some(Err(error));
                        return Err(error);
                    }
                }
            }
            let result = self
                .tasks
                .callable_scope()
                .map_err(|e| self.tasks.failure(&e));
            self.terminal = Some(result.clone());
            result
        } else {
            let query = self.tasks.callable_id();
            match cx.read(query) {
                Err(error) => {
                    self.terminal = Some(Err(error));
                    return Err(error);
                }
                Ok(product)
                    if self
                        .tasks
                        .callables
                        .get()
                        .is_some_and(|(p, _)| *p == product) =>
                {
                    ()
                }
                _ => {
                    let error = self.tasks.failure("callable alias has another graph");
                    self.terminal = Some(Err(error));
                    return Err(error);
                }
            }
            let result = (|| {
                self.tasks
                    .callables
                    .get()
                    .unwrap()
                    .1
                    .find(self.tasks.store.as_ref(), self.id.address)
                    .map_err(|error| self.tasks.failure(&error))?
                    .map(|output| output.product.product.clone())
                    .ok_or_else(|| self.tasks.failure("unregistered callable Value alias"))
            })();
            self.terminal = Some(result.clone());
            result
        }
    }
}

#[cfg(test)]
mod retained_tests {
    use super::*;
    use hirsel_engine::DefinitionQueries;

    #[test]
    fn registry_metadata_refuses_one_less_and_releases_its_exact_partition() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            let transport = hirsel_engine_admission::ProcessBudget::new(64 << 20);
            let store: Arc<dyn ResultStore> = Arc::new(
                hirsel_engine_store::RequestStore::with_scratch(
                    Arc::new(hirsel_engine_store::MemStore::default()),
                    transport.reserve(64 << 20).unwrap(),
                    &std::env::temp_dir(),
                    1 << 30,
                )
                .unwrap(),
            );
            let mut engine = hirsel_engine::QueryEngine::new(
                hirsel_engine::default_environment(),
                hirsel_front_phases::compiler_adapters(),
            )
            .with_result_store(store.clone());
            let file = engine
                .sources
                .add("registry-bounds.hir", "pub fn main()->u64{1u64}".into())
                .unwrap();
            engine
                .parse_file(file, &hirsel_engine::default_context())
                .unwrap();
            let mut discovery =
                hirsel_engine::NativeDiscoveryCache::new(engine.types().clone(), store.clone());
            let declarations = hirsel_native_cache::declarations::NativeDeclarations::collect(
                &mut engine,
                &mut discovery,
                &[],
            )
            .unwrap();
            let paths = declarations
                .context
                .sources
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let context = engine.native_original_context(&paths).unwrap();
            let source =
                NativeInstanceInputs::from_products(declarations, context.reader([]).unwrap())
                    .unwrap();
            let budget = hirsel_engine_admission::ProcessBudget::new(16 << 20);
            let make = |bytes| {
                InstanceTasks::new(
                    source.clone(),
                    store.clone(),
                    hirsel_native::Options::new(hirsel_lir::TargetSpec::x86_64_unknown_linux_gnu()),
                    ContentHash::of_bytes(b"registry bounds"),
                    MemoryWeight {
                        scratch: 16 << 20,
                        retained: 1 << 20,
                        stack: 0,
                    },
                    budget.reserve(bytes).unwrap(),
                )
            };
            let tasks = make(16 << 20).unwrap();
            let copied = source.retained_metadata_bytes();
            let required = tasks.registry.lock().unwrap().charged;
            assert!(required > size_of::<InstanceTasks>() as u64);
            drop(tasks);
            assert_eq!(budget.reserved(), copied);
            let refusal = make(required - 1)
                .err()
                .expect("one less cannot retain the registered roots");
            assert_eq!(refusal, "native discovery metadata allowance exhausted");
            assert_eq!(budget.reserved(), copied);
            let exact = make(required).unwrap();
            assert_eq!(exact.allowance.bytes(), required);
            assert_eq!(exact.registry.lock().unwrap().charged, required);
            assert_eq!(budget.reserved(), copied + required);
            drop(source);
            drop(exact);
            assert_eq!(budget.reserved(), 0);
        });
    }
}

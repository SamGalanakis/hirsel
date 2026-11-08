//! One runtime request from cold instance discovery through actual native Link.
//! Dispatch is installed before the request; dependent task metadata is attached
//! only after its actual arena/instance reads settle. Creation never runs work.
use super::{
    arena_tasks::ArenaTasks,
    instance_tasks::InstanceTasks,
    link_tasks::{
        LINK_INPUT_KIND, LINK_MANIFEST_KIND, LinkInputFactory, LinkTasks, NativeLinkExecutor,
        NativeLinkPlan,
    },
    products::{EmissionOptions, NativeSnapshot},
    root_tasks::{NativeRootFactory, NativeRootTasks},
    tasks::{CodegenTasks, NativeTaskFactory, NativeTasks},
    *,
};
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Action, Address, Domain};
use hirsel_native::object_queries::CODEGEN_KIND;
use hirsel_native::object_queries::CodegenKey;
use hirsel_native::object_queries::LINK_KIND;
use hirsel_native::object_queries::LinkKey;
use hirsel_native::object_queries::ObjectTaskFactory;
use hirsel_native::root_source::NativeLinkSource;
use std::{collections::BTreeMap, sync::OnceLock};

pub const COMPILATION_KIND: u32 = 0x4e00_0023;
/// Settled identities and the Ready program assembled over exactly them.
pub struct ReadyView {
    pub root: ProductRef,
    pub ready: Vec<ProductRef>,
    pub program: hirsel_native::ReadyProgram,
    pub observation: hirsel_native::NativeObservation,
    pub profiles: Option<Arc<hirsel_native::ProfileSources>>,
}
/// What one compilation settles: a linked executable, Codegen fragments, or
/// only the root and its Ready products (no Codegen or Link is demanded).
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum CompilationOutput {
    Executable,
    Objects,
    Ready,
}
/// Frozen platform/byte/codegen identities. The discovered instance closure is
/// applied during the compilation poll; no mutable engine or callback builds
/// hidden native dependencies here. Executor is invoked solely by actual Link.
pub struct CompilationBackend {
    pub backend: String,
    pub backend_version: ContentHash,
    pub target_features: ContentHash,
    pub profile_options: ContentHash,
    pub runtime_abi: ContentHash,
    pub linker_options: ContentHash,
    pub runtime_archives: Vec<Reference>,
    pub foreign_inputs: Vec<Reference>,
    pub executor: Arc<dyn NativeLinkExecutor>,
}
struct Dispatch {
    discovery: ProductRef,
    arena: ProductRef,
    native: Arc<NativeTasks>,
    roots: Arc<NativeRootTasks>,
    link: Arc<LinkTasks>,
    factories: BTreeMap<u32, Arc<dyn QueryFactory>>,
    links: Vec<QueryId>,
}
/// Immutable inputs, then compact dispatch and completion metadata. C owns its
/// own private acceptance caches. No Definition or Value collection is retained.
pub struct CompilationTasks {
    instances: Arc<InstanceTasks>,
    arena: Arc<ArenaTasks>,
    source: Arc<NativeLinkSource>,
    root: NativeRootKey,
    emission: EmissionOptions,
    backends: Vec<CompilationBackend>,
    query: QueryId,
    verify_store: hirsel_engine_store::VerifyStore,
    output: CompilationOutput,
    weight: MemoryWeight,
    allowance: Arc<hirsel_engine_admission::MemoryReservation>,
    dispatch: OnceLock<Dispatch>,
    completed: OnceLock<(ProductRef, Vec<ProductRef>)>,
    fallback: Reference,
}
impl CompilationTasks {
    pub fn new(
        instances: Arc<InstanceTasks>,
        arena: Arc<ArenaTasks>,
        source: Arc<NativeLinkSource>,
        root: NativeRootKey,
        emission: EmissionOptions,
        backends: Vec<CompilationBackend>,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Arc<Self>, String> {
        Self::new_with_store_verification(
            instances,
            arena,
            source,
            root,
            emission,
            backends,
            weight,
            allowance,
            hirsel_engine_store::VerifyStore::Off,
        )
    }
    /// Explicit object/Link cache verification, captured before dispatch.
    /// Recompute still requires the same settled native/C dependency scope.
    pub fn new_with_store_verification(
        instances: Arc<InstanceTasks>,
        arena: Arc<ArenaTasks>,
        source: Arc<NativeLinkSource>,
        root: NativeRootKey,
        emission: EmissionOptions,
        backends: Vec<CompilationBackend>,
        weight: MemoryWeight,
        mut allowance: hirsel_engine_admission::MemoryReservation,
        verify_store: hirsel_engine_store::VerifyStore,
    ) -> Result<Arc<Self>, String> {
        if root.target_abi != instances.options().target.digest()
            || root.policy != instances.policy()
            || backends.is_empty()
            || backends
                .iter()
                .any(|backend| !matches!(backend.backend.as_str(), "llvm" | "cranelift"))
        {
            return Err("compilation inputs differ from native request snapshot".into());
        }
        source.retain_metadata(&mut allowance)?;
        let roots = root.query_id()?;
        let selection = serde_json::to_vec(
            &backends
                .iter()
                .map(|backend| {
                    (
                        &backend.backend,
                        backend.backend_version,
                        backend.target_features,
                        backend.profile_options,
                        backend.runtime_abi,
                        backend.linker_options,
                        backend.executor.codegen_partition(),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())?;
        let query = QueryId {
            kind: COMPILATION_KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.native.compilation-address.v1",
                [
                    roots.address.0.0.as_slice(),
                    selection.as_slice(),
                    &[u8::from(
                        verify_store == hirsel_engine_store::VerifyStore::Recompute,
                    )],
                ],
            )),
        };
        if weight.retained < (std::mem::size_of::<CompilationTask>() + backends.len() * 256) as u64
        {
            return Err("compilation continuation exceeds retained allowance".into());
        }
        let fallback = instances.failure("native compilation resource or protocol refusal");
        Ok(Arc::new(Self {
            instances,
            arena,
            source,
            root,
            emission,
            backends,
            query,
            verify_store,
            output: CompilationOutput::Executable,
            weight,
            allowance: Arc::new(allowance),
            dispatch: OnceLock::new(),
            completed: OnceLock::new(),
            fallback,
        }))
    }
    /// Select the settled output before installing any factory. Objects and
    /// Ready use the same discovery, arena, independent C and Ready
    /// dependencies; neither demands Link nor manufactures an executable, and
    /// Ready demands no Codegen either.
    pub fn with_output(
        mut self: Arc<Self>,
        output: CompilationOutput,
    ) -> Result<Arc<Self>, String> {
        if output == self.output {
            return Ok(self);
        }
        let this =
            Arc::get_mut(&mut self).ok_or("compilation output requires an unshared compilation")?;
        if this.dispatch.get().is_some() || this.completed.get().is_some() {
            return Err("compilation output must be selected before dispatch".into());
        }
        let domain = match output {
            CompilationOutput::Executable => {
                return Err("an executable is the default output".into());
            }
            CompilationOutput::Objects => "hirsel.native.compilation-codegen-address.v1",
            CompilationOutput::Ready => "hirsel.native.compilation-ready-address.v1",
        };
        this.query.address = Address(ContentHash::of_parts(
            domain,
            [this.query.address.0.0.as_slice()],
        ));
        this.output = output;
        Ok(self)
    }
    /// The diagnostic produced by an actual native dependency, including a
    /// failure before compilation completes. Reopening it executes no query.
    pub fn failure(&self, reference: Reference) -> Option<String> {
        self.instances.failure_message(reference)
    }
    pub fn query_id(&self) -> QueryId {
        self.query
    }
    /// Installable dispatch for this one immutable compilation request. Creating
    /// factories only describes work; discovery, arena solving, native admission,
    /// emission and linking still run solely through their dependency reads.
    pub fn factories(self: &Arc<Self>) -> Vec<(u32, Arc<dyn QueryFactory>)> {
        let instances: Arc<dyn QueryFactory> = Arc::new(
            super::instance_tasks::InstanceTaskFactory::new(self.instances.clone()),
        );
        let arena: Arc<dyn QueryFactory> = Arc::new(super::arena_tasks::ArenaTaskFactory::new(
            self.arena.clone(),
        ));
        let compilation: Arc<dyn QueryFactory> = Arc::new(CompilationFactory::new(self.clone()));
        let mut factories = Vec::new();
        for kind in [
            INSTANCE_KIND,
            hirsel_native::query_model::CALLABLE_VALUE_KIND,
            super::instance_tasks::CALLABLE_GRAPH_KIND,
            super::instance_tasks::INSTANCE_INPUT_KIND,
            super::instance_tasks::DISCOVERY_KIND,
        ] {
            factories.push((kind, instances.clone()));
        }
        for kind in [
            ARENA_CANDIDATES_KIND,
            ARENA_CHOSEN_KIND,
            super::arena_tasks::ARENA_GRAPH_KIND,
        ] {
            factories.push((kind, arena.clone()));
        }
        for kind in [
            COMPILATION_KIND,
            super::seal_demand::SEAL_DEMAND_KIND,
            READY_KIND,
            ROOT_KIND,
            CODEGEN_KIND,
            LINK_KIND,
            LINK_INPUT_KIND,
            LINK_MANIFEST_KIND,
            super::execution::EXECUTION_KIND,
            super::execution::EXECUTION_OBJECTS_KIND,
        ] {
            factories.push((kind, compilation.clone()));
        }
        factories
    }
    fn initialize(&self, discovery: ProductRef, arena: ProductRef) -> Result<&Dispatch, String> {
        if let Some(dispatch) = self.dispatch.get() {
            return if dispatch.discovery == discovery && dispatch.arena == arena {
                Ok(dispatch)
            } else {
                Err("compilation dependency changed within immutable snapshot".into())
            };
        }
        let completed = self.instances.completed_instances(discovery.clone())?;
        hirsel_intern::memory::checkpoint("global Value complete");
        let mut locals = Vec::new();
        let mut needs_execution = false;
        for (key, stored) in &completed {
            let output = self.instances.read_completed(key, stored)?;
            needs_execution |= output.facts.body.templates.execution;
            locals.push(output.facts.body.seal_demand.clone());
        }
        let guard_bytes = if locals.iter().any(|local| local.has_seal()) {
            super::seal_demand::Plan::stored_admission_bytes(locals.iter())
        } else {
            0
        };
        drop(locals);
        let owners = completed
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let declarations = &self.instances.source_inputs().declarations;
        let mut operations = self.instances.source_operations(discovery.clone())?;
        let operations_bytes = super::ownership_compilation::metadata::operations(&mut operations);
        let manifest = declarations.manifest()?;
        let mut snapshot = NativeSnapshot::new(
            self.instances.selected_store().clone(),
            self.instances.products(discovery.clone())?,
            self.arena.graph(arena.clone())?,
            self.instances.options().clone(),
            self.emission.clone(),
            manifest,
            Arc::new(operations),
        )?
        .with_store_verification(self.verify_store)?;
        // The returned NativeTasks, snapshot and linker views can outlive this
        // controller. Share each existing charge with their actual data owner;
        // never reserve a second copy or retain a controller back-reference.
        let retained_allowances = vec![
            self.allowance.clone(),
            self.instances.retained_allowance(),
            self.instances.retained_source_allowance(),
            self.source
                .metadata_allowance()
                .ok_or("native link source metadata is not admitted")?,
            self.arena.retained_allowance(),
        ];
        Arc::get_mut(&mut snapshot)
            .ok_or("native snapshot was shared before admission completed")?
            .retained_allowances = retained_allowances;
        Arc::get_mut(&mut snapshot).unwrap().guard_admitted_bytes = guard_bytes;
        let snapshot_bytes = Arc::get_mut(&mut snapshot)
            .unwrap()
            .retained_dispatch_bytes();
        let mut native = NativeTasks::for_ready(snapshot, self.weight)?;
        let native_bytes = Arc::get_mut(&mut native).unwrap().retained_dispatch_bytes();
        let roots = NativeRootTasks::new(
            native.clone(),
            self.source.clone(),
            self.root.clone(),
            self.weight,
        )?;
        let mut codegen = Vec::new();
        let mut whole_codegen = Vec::new();
        let mut plans = Vec::new();
        let mut links = Vec::new();
        if self.output == CompilationOutput::Ready {
            links.push(roots.query_id());
            links.extend(owners.iter().map(InstanceAddress::ready_query_id));
        }
        for backend in self
            .backends
            .iter()
            .filter(|_| self.output != CompilationOutput::Ready)
        {
            let fragments = owners
                .iter()
                .map(|owner| CodegenKey {
                    owner: owner.clone(),
                    backend: backend.backend.clone(),
                    backend_version: backend.backend_version,
                    target_features: backend.target_features,
                    profile_options: backend.profile_options,
                    runtime_abi: backend.runtime_abi,
                })
                .collect::<Vec<_>>();
            match backend.executor.codegen_partition() {
                super::link_tasks::CodegenPartition::PerInstance => {
                    if self.output == CompilationOutput::Objects {
                        links.extend(fragments.iter().map(CodegenKey::query_id));
                    }
                    codegen.extend(fragments.iter().cloned());
                }
                partition => {
                    for key in partition.modules(&fragments)? {
                        if self.output == CompilationOutput::Objects {
                            links.push(key.query_id());
                        }
                        whole_codegen.push(key);
                    }
                }
            }
            if self.output == CompilationOutput::Objects {
                continue;
            }
            let key = LinkKey {
                roots: owners.clone().into(),
                target: self.root.target_abi,
                linker_options: backend.linker_options,
            };
            links.push(key.query_id());
            plans.push(NativeLinkPlan {
                key,
                fragments,
                runtime_archives: backend.runtime_archives.clone(),
                foreign_inputs: backend.foreign_inputs.clone(),
                executor: backend.executor.clone(),
                weight: self.weight,
            });
        }
        let execution_keys = self
            .backends
            .iter()
            .filter(|_| needs_execution && self.output != CompilationOutput::Ready)
            .map(|backend| CodegenKey {
                owner: owners[0].clone(),
                backend: backend.backend.clone(),
                backend_version: backend.backend_version,
                target_features: backend.target_features,
                profile_options: backend.profile_options,
                runtime_abi: backend.runtime_abi,
            })
            .collect::<Vec<_>>();
        let mut execution =
            super::execution::ExecutionTasks::new(roots.clone(), &execution_keys, self.weight);
        let execution_bytes = Arc::get_mut(&mut execution)
            .unwrap()
            .retained_dispatch_bytes();
        let mut objects =
            CodegenTasks::with_whole_modules(native.clone(), codegen, whole_codegen, self.weight)?;
        Arc::get_mut(&mut objects).unwrap().execution =
            needs_execution.then(|| execution.query_id());
        let object_bytes = Arc::get_mut(&mut objects)
            .unwrap()
            .retained_dispatch_bytes();
        let mut link = LinkTasks::new(roots.clone(), plans, self.weight)?;
        Arc::get_mut(&mut link).unwrap().execution = needs_execution.then(|| execution.clone());
        let link_bytes = Arc::get_mut(&mut link).unwrap().retained_dispatch_bytes();
        let mut copied = hirsel_query_products::retained::MetadataWeight::default();
        // Fixed controller/factory envelopes plus every owned variable buffer.
        copied.add(16384);
        copied.add(super::demand::window::WindowBudget::retained_bytes() as usize);
        copied.vec(&self.root.roots, |w, path| w.path(path));
        super::ownership_compilation::metadata::emission(&mut copied, &self.emission);
        copied.vec(&self.backends, |w, backend| {
            w.add(backend.backend.capacity());
            w.flat_vec(&backend.runtime_archives);
            w.flat_vec(&backend.foreign_inputs);
        });
        copied.flat_vec(&links);
        copied.add(
            links
                .len()
                .saturating_mul(2)
                .max(4)
                .saturating_mul(size_of::<ProductRef>()),
        );
        let bytes = guard_bytes
            .saturating_add(operations_bytes)
            .saturating_add(snapshot_bytes)
            .saturating_add(native_bytes)
            .saturating_add(roots.retained_dispatch_bytes())
            .saturating_add(execution_bytes)
            .saturating_add(object_bytes)
            .saturating_add(link_bytes)
            .saturating_add(copied.bytes());
        if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
            eprintln!(
                "NATIVE_DISPATCH_OWNERS guard={guard_bytes} operations={operations_bytes} snapshot={snapshot_bytes} native={native_bytes} roots={} execution={execution_bytes} objects={object_bytes} link={link_bytes} copied={} total={bytes} allowance={}",
                roots.retained_dispatch_bytes(),
                copied.bytes(),
                self.allowance.bytes(),
            );
        }
        if bytes > self.allowance.bytes() {
            return Err("native compilation dispatch allowance exhausted".into());
        }
        let window = super::demand::window::WindowBudget::new(self.allowance.clone());
        window.set_base(bytes)?;
        native
            .snapshot
            .dispatch_window
            .set(window)
            .map_err(|_| "native dispatch window installed twice")?;
        let leaf: Arc<dyn QueryFactory> = Arc::new(LinkInputFactory::new(link.clone()));
        let mut factories: BTreeMap<u32, Arc<dyn QueryFactory>> = BTreeMap::from([
            (
                READY_KIND,
                Arc::new(NativeTaskFactory::new(native.clone())) as Arc<dyn QueryFactory>,
            ),
            (
                ROOT_KIND,
                Arc::new(NativeRootFactory::new(roots.clone())) as Arc<dyn QueryFactory>,
            ),
            (
                CODEGEN_KIND,
                Arc::new(ObjectTaskFactory::new(objects)) as Arc<dyn QueryFactory>,
            ),
            (
                LINK_KIND,
                Arc::new(ObjectTaskFactory::new(link.clone())) as Arc<dyn QueryFactory>,
            ),
            (LINK_INPUT_KIND, leaf.clone()),
            (LINK_MANIFEST_KIND, leaf),
        ]);
        let execution_factory: Arc<dyn QueryFactory> =
            Arc::new(super::execution::Factory(execution));
        factories.insert(super::execution::EXECUTION_KIND, execution_factory.clone());
        factories.insert(super::execution::EXECUTION_OBJECTS_KIND, execution_factory);
        if let Some((key, factory)) = native.guard_factory() {
            factories.insert(key.kind, factory);
        }
        self.dispatch
            .set(Dispatch {
                discovery,
                arena,
                native,
                roots,
                link,
                factories,
                links,
            })
            .map_err(|_| "duplicate native compilation dispatch construction")?;
        Ok(self.dispatch.get().unwrap())
    }
    fn selected(&self, compilation: ProductRef) -> Result<&Dispatch, String> {
        let (selected, _) = self
            .completed
            .get()
            .ok_or("native compilation has not completed")?;
        if *selected != compilation {
            return Err("native compilation differs from selected output".into());
        }
        self.dispatch
            .get()
            .ok_or("native compilation dispatch is unavailable".into())
    }
    /// Exact selected outputs, in backend order: Link for executables and
    /// Codegen fragments for emit-only requests.
    pub fn outputs(&self, compilation: ProductRef) -> Result<&[ProductRef], String> {
        self.selected(compilation)?;
        Ok(&self.completed.get().unwrap().1)
    }
    /// Number of independently admitted instances emitted by this completion.
    pub fn emitted_instances(&self, compilation: ProductRef) -> Result<usize, String> {
        let dispatch = self.selected(compilation)?;
        if self.output != CompilationOutput::Objects {
            return Err("compilation did not select object emission".into());
        }
        Ok(self
            .instances
            .completed_instances(dispatch.discovery.clone())?
            .len())
    }
    pub fn native(&self, compilation: ProductRef) -> Result<Arc<NativeTasks>, String> {
        Ok(self.selected(compilation)?.native.clone())
    }
    pub fn roots(&self, compilation: ProductRef) -> Result<Arc<NativeRootTasks>, String> {
        Ok(self.selected(compilation)?.roots.clone())
    }
    /// Actual independently validated removals from the exact discovery read
    /// by this completed compilation. The requested plan supplies no report.
    pub fn discharged(
        &self,
        compilation: ProductRef,
    ) -> Result<hirsel_lir::discharge::Discharge, String> {
        self.instances
            .discharged(self.selected(compilation)?.discovery.clone())
    }
    /// Body-free execution metadata of this exact completed compilation's
    /// settled Link scope. No dependency query or current-association lookup
    /// supplies a missing read, and Link hits expose the same admitted metadata.
    pub fn observation(
        &self,
        compilation: ProductRef,
    ) -> Result<hirsel_native::NativeObservation, String> {
        self.selected(compilation)?.link.observation()
    }

    /// The owned Ready execution view of this exact completed Ready
    /// compilation: its settled root and Ready reads, the Ready program over
    /// them, body-free observation tables and, with source/debug tables, the
    /// root's source profiles. Reopening runs no dependency query.
    pub fn ready_view(&self, compilation: ProductRef) -> Result<ReadyView, String> {
        let dispatch = self.selected(compilation)?;
        if self.output != CompilationOutput::Ready {
            return Err("compilation did not select a Ready execution view".into());
        }
        let (root, ready) = self
            .completed
            .get()
            .unwrap()
            .1
            .split_first()
            .ok_or("Ready compilation has no settled root")?;
        let (mut products, selected) = dispatch.roots.products(root.clone(), ready)?;
        let program = products.ready_program(&selected, ready)?;
        let observation = products.observation_metadata(&selected)?;
        let profiles = self
            .emission
            .debug
            .then(|| products.profile_sources(&selected))
            .transpose()?;
        Ok(ReadyView {
            root: root.clone(),
            ready: ready.to_vec(),
            program,
            observation,
            profiles,
        })
    }

    fn publish(&self, outputs: Vec<ProductRef>) -> Result<ProductRef, String> {
        if let Some(window) = self
            .dispatch
            .get()
            .and_then(|d| d.native.snapshot.dispatch_window.get())
        {
            window.report("dispatch");
        }
        if self.output == CompilationOutput::Executable {
            hirsel_intern::memory::checkpoint("link complete");
        }
        let action = Action(ContentHash::of_parts(
            "hirsel.native.compilation-action.v2",
            std::iter::once(self.query.address.0.0.as_slice()).chain(outputs.iter().flat_map(
                |product| {
                    [
                        product.address.0.0.as_slice(),
                        product.action.0.0.as_slice(),
                    ]
                },
            )),
        ));
        let product = ProductRef::dependencies(
            LiveDependencyProduct::new(
                self.instances.selected_store().as_ref(),
                self.query.address,
                action,
                Domain {
                    kind: COMPILATION_KIND,
                    schema: 2,
                },
                outputs.clone(),
            )
            .map_err(|e| format!("native compilation completion admission: {e}"))?,
        );
        if let Err(other) = self.completed.set((product.clone(), outputs))
            && self.completed.get().unwrap().0 != other.0
        {
            return Err("native compilation completion changed".into());
        }
        Ok(product)
    }
}
pub struct CompilationFactory {
    tasks: Arc<CompilationTasks>,
}
impl CompilationFactory {
    pub fn new(tasks: Arc<CompilationTasks>) -> Self {
        Self { tasks }
    }
}
impl QueryFactory for CompilationFactory {
    fn create(&self, key: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if key == self.tasks.query {
            return Ok(Box::new(CompilationTask {
                tasks: self.tasks.clone(),
                terminal: None,
            }));
        }
        self.tasks
            .dispatch
            .get()
            .and_then(|dispatch| dispatch.factories.get(&key.kind))
            .ok_or(self.tasks.fallback)?
            .create(key)
    }
}
struct CompilationTask {
    tasks: Arc<CompilationTasks>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for CompilationTask {
    fn id(&self) -> QueryId {
        self.tasks.query
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
impl CompilationTask {
    fn advance(&self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let tasks = &self.tasks;
        let mut selected = Vec::new();
        for key in [tasks.instances.discovery_id(), tasks.arena.query_id()] {
            match cx.read(key) {
                Err(error) => return Err(error),
                Ok(product) => selected.push(product),
            }
        }
        let initializing = tasks.dispatch.get().is_none();
        let dispatch = tasks
            .initialize(selected[0].clone(), selected[1].clone())
            .map_err(|e| tasks.instances.failure(&e))?;
        if initializing {
            // Native Value admission is complete. Give the coming emission
            // workspace the heap occupied by recomputable P-body cache entries.
            // The runtime preserves every active or externally pinned value.
            while cx.evict_body() {}
            hirsel_intern::memory::checkpoint("native emission cache pressure");
        }
        let mut outputs = Vec::new();
        for key in &dispatch.links {
            match cx.read(*key) {
                Err(error) => return Err(error),
                Ok(product) if product.address == key.address => outputs.push(product),
                _ => {
                    return Err(tasks
                        .instances
                        .failure("native compilation read another selected output"));
                }
            }
        }
        tasks
            .publish(outputs)
            .map_err(|e| tasks.instances.failure(&e))
    }
}

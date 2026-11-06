//! Original object-control vectors over actual native continuations. The two
//! runtimes settle source discovery and then the immutable native snapshot;
//! no legacy Ready/object controller or test implementation supplies products.
use hirsel_compiler_identity::COMPILER_IDENTITY;
use hirsel_driver::native;
use hirsel_engine::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::ResultStore;

use hirsel_native::object_queries::CodegenKey;
use hirsel_native::object_queries::ObjectTaskFactory;
use hirsel_native::object_queries::fragment_product;
use hirsel_native::object_queries::load_object;
use hirsel_native::query_model::NativeRootKey;
use hirsel_native::query_model::{NativeProducts, NativeRoots};
use hirsel_native::root_source::NativeLinkFacts;
use hirsel_native::root_source::NativeLinkSource;
use hirsel_native_cache::declarations::NativeDeclarations;
use hirsel_native_cache::source_inputs::NativeInstanceInputs;
use hirsel_native_queries as queries;
use hirsel_native_queries::arena_tasks::ARENA_GRAPH_KIND;
use hirsel_native_queries::arena_tasks::ArenaTaskFactory;
use hirsel_native_queries::arena_tasks::ArenaTasks;
use hirsel_native_queries::execution::EXECUTION_KIND;
use hirsel_native_queries::execution::ExecutionTasks;
use hirsel_native_queries::execution::Factory as ExecutionFactory;
use hirsel_native_queries::instance_tasks::DISCOVERY_KIND;
use hirsel_native_queries::instance_tasks::INSTANCE_INPUT_KIND;
use hirsel_native_queries::instance_tasks::InstanceTaskFactory;
use hirsel_native_queries::instance_tasks::InstanceTasks;
use hirsel_native_queries::products::EmissionOptions;
use hirsel_native_queries::products::NativeSnapshot;
use hirsel_native_queries::root_tasks::NativeRootFactory;
use hirsel_native_queries::root_tasks::NativeRootTasks;
use hirsel_native_queries::tasks::CodegenTasks;
use hirsel_native_queries::tasks::NativeTaskFactory;
use hirsel_native_queries::tasks::NativeTasks;
use hirsel_syntax::ContentHash;
use std::{collections::BTreeMap, sync::Arc};

pub(super) struct Objects {
    pub summaries: Vec<hirsel_lir::fragment::UnitSummary>,
    pub objects: Vec<hirsel_lir::stage::ObjectArtifact>,
    pub hits: usize,
    pub products: Vec<(CodegenKey, ProductRef)>,
    pub ready_transformations: u64,
    pub instance_imports: u64,
    pub ready_products: Vec<ProductRef>,
}

pub(super) fn compile(
    engine: &mut QueryEngine,
    store: Arc<dyn ResultStore>,
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
) -> Objects {
    compile_entry(
        engine,
        store,
        &entry_path(&default_context()),
        backend,
        opt,
        options,
        debug,
        jobs,
    )
}

pub(super) fn compile_entry(
    engine: &mut QueryEngine,
    store: Arc<dyn ResultStore>,
    entry: &hirsel_syntax::DefPath,
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
) -> Objects {
    compile_with_context(
        engine,
        store,
        entry,
        backend,
        opt,
        options,
        debug,
        jobs,
        hirsel_engine_store::VerifyStore::Off,
        |_, _| (),
    )
    .0
}

pub(super) fn compile_with_context<R>(
    engine: &mut QueryEngine,
    store: Arc<dyn ResultStore>,
    entry: &hirsel_syntax::DefPath,
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
    verification: hirsel_engine_store::VerifyStore,
    use_context: impl FnOnce(&hirsel_native::ReadyProgram, &[hirsel_lir::stage::ObjectArtifact]) -> R,
) -> (Objects, R) {
    compile_roots(
        engine,
        store,
        std::slice::from_ref(entry),
        &[],
        backend,
        opt,
        options,
        debug,
        jobs,
        verification,
        use_context,
    )
}

pub(super) fn compile_roots<R>(
    engine: &mut QueryEngine,
    store: Arc<dyn ResultStore>,
    source_selection: &[hirsel_syntax::DefPath],
    requested: &[hirsel_syntax::DefPath],
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
    verification: hirsel_engine_store::VerifyStore,
    use_context: impl FnOnce(&hirsel_native::ReadyProgram, &[hirsel_lir::stage::ObjectArtifact]) -> R,
) -> (Objects, R) {
    compile_roots_inspect(
        engine,
        store,
        source_selection,
        requested,
        backend,
        opt,
        options,
        debug,
        jobs,
        verification,
        |_, _, _| (),
        use_context,
    )
}

pub(super) fn compile_with_context_inspect<R>(
    engine: &mut QueryEngine,
    store: Arc<dyn ResultStore>,
    entry: &hirsel_syntax::DefPath,
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
    verification: hirsel_engine_store::VerifyStore,
    inspect: impl FnOnce(
        &mut queries::roots::RootProducts,
        &Arc<hirsel_native::query_model::NativeRootProduct>,
        &[Arc<hirsel_native::query_model::ReadyProduct>],
    ),
    use_context: impl FnOnce(&hirsel_native::ReadyProgram, &[hirsel_lir::stage::ObjectArtifact]) -> R,
) -> (Objects, R) {
    compile_roots_inspect(
        engine,
        store,
        std::slice::from_ref(entry),
        &[],
        backend,
        opt,
        options,
        debug,
        jobs,
        verification,
        inspect,
        use_context,
    )
}

pub(super) fn compile_roots_inspect<R>(
    engine: &mut QueryEngine,
    store: Arc<dyn ResultStore>,
    source_selection: &[hirsel_syntax::DefPath],
    requested: &[hirsel_syntax::DefPath],
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
    verification: hirsel_engine_store::VerifyStore,
    inspect: impl FnOnce(
        &mut queries::roots::RootProducts,
        &Arc<hirsel_native::query_model::NativeRootProduct>,
        &[Arc<hirsel_native::query_model::ReadyProduct>],
    ),
    use_context: impl FnOnce(&hirsel_native::ReadyProgram, &[hirsel_lir::stage::ObjectArtifact]) -> R,
) -> (Objects, R) {
    let mut cache = NativeDiscoveryCache::new(engine.types().clone(), store.clone());
    let mut declarations = NativeDeclarations::collect(engine, &mut cache, requested).unwrap();
    let paths = declarations.sources.keys().cloned().collect::<Vec<_>>();
    let original = engine.native_original_reader(&paths).unwrap();
    for (path, source) in &mut declarations.sources {
        Arc::make_mut(source).lower = original
            .input(path)
            .and_then(|(output, _, _, _)| output.cloned());
    }
    let semantic = Arc::new(
        engine
            .native_semantic_program(declarations.roots.definitions.clone())
            .unwrap(),
    );
    compile_settled(
        declarations,
        original,
        engine.signature_snapshot(),
        semantic,
        store,
        source_selection,
        backend,
        opt,
        options,
        debug,
        jobs,
        verification,
        false,
        inspect,
        use_context,
    )
}

pub(super) fn compile_front<R>(
    facts: Arc<FrontNativeProducts>,
    store: Arc<dyn ResultStore>,
    source_selection: &[hirsel_syntax::DefPath],
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
    verification: hirsel_engine_store::VerifyStore,
    execution: bool,
    inspect: impl FnOnce(
        &mut queries::roots::RootProducts,
        &Arc<hirsel_native::query_model::NativeRootProduct>,
        &[Arc<hirsel_native::query_model::ReadyProduct>],
    ),
    use_context: impl FnOnce(&hirsel_native::ReadyProgram, &[hirsel_lir::stage::ObjectArtifact]) -> R,
) -> (Objects, R) {
    let declarations = NativeDeclarations::from_front(facts.clone()).unwrap();
    let original = facts
        .original_context
        .reader(facts.originals.iter().map(|product| (**product).clone()))
        .unwrap();
    compile_settled(
        declarations,
        original,
        facts.signatures.clone(),
        facts.semantic.clone(),
        store,
        source_selection,
        backend,
        opt,
        options,
        debug,
        jobs,
        verification,
        execution,
        inspect,
        use_context,
    )
}

fn compile_settled<R>(
    declarations: NativeDeclarations,
    original: NativeOriginalReader,
    signatures: Arc<SignatureSnapshot>,
    semantic: Arc<NativeSemanticProgram>,
    store: Arc<dyn ResultStore>,
    source_selection: &[hirsel_syntax::DefPath],
    backend: hirsel_native::Backend,
    opt: hirsel_native::OptLevel,
    options: hirsel_native::Options,
    debug: bool,
    jobs: hirsel_intern::jobs::Jobs,
    verification: hirsel_engine_store::VerifyStore,
    execution: bool,
    inspect: impl FnOnce(
        &mut queries::roots::RootProducts,
        &Arc<hirsel_native::query_model::NativeRootProduct>,
        &[Arc<hirsel_native::query_model::ReadyProduct>],
    ),
    use_context: impl FnOnce(&hirsel_native::ReadyProgram, &[hirsel_lir::stage::ObjectArtifact]) -> R,
) -> (Objects, R) {
    let entries = declarations
        .context
        .sources
        .iter()
        .filter(|(path, _)| {
            matches!(&declarations.program.signatures[*path].kind,
            hirsel_types::SignatureKind::Function(function) if function.generics.is_empty())
        })
        .map(|(path, source)| (path.clone(), source.entry.0))
        .collect::<BTreeMap<_, _>>();
    let source = Arc::new(
        NativeLinkSource::from_settled(
            NativeLinkFacts {
                program: &declarations.program,
                operations: Arc::new(declarations.program.operation_registry()),
                roots: &declarations.roots,
                declarations: &declarations.sources,
                entries: &entries,
                anchors: &declarations.source_anchors,
                signatures,
                semantic,
                selected: source_selection,
                callees: None,
            },
            &original,
        )
        .unwrap(),
    );
    let manifest = declarations.manifest().unwrap();
    let emission = EmissionOptions::request(
        declarations.program.input.clone(),
        opt,
        debug,
        source.foreign_dispatch(),
        true,
        false,
        false,
    );
    let inputs = NativeInstanceInputs::from_products(declarations, original).unwrap();
    let policy = ContentHash::of_bytes(b"original native object controls");
    let target = options.target.clone();
    let runtime_bytes = (jobs.get() as u64)
        .checked_mul(hirsel_intern::jobs::COMPILER_STACK_BYTES as u64)
        .unwrap()
        + (64 << 20);
    let budget = hirsel_engine_admission::ProcessBudget::new(runtime_bytes + (192 << 20));
    // Source/registry products retain their own measured grants. Continuation
    // constructors bound the descriptor frame within this separate allowance.
    let weight = MemoryWeight {
        retained: 256 << 10,
        scratch: 16 << 20,
        stack: 0,
    };
    let instances = InstanceTasks::new(
        inputs,
        store.clone(),
        options.clone(),
        policy,
        weight,
        budget.reserve(64 << 20).unwrap(),
    )
    .unwrap()
    .with_store_verification(verification)
    .unwrap();
    let arenas = ArenaTasks::new(
        instances.clone(),
        store.clone(),
        0,
        weight,
        budget.reserve(64 << 20).unwrap(),
    )
    .unwrap();
    let denied = instances.failure("original native object control resource refusal");
    let errors = RuntimeErrors {
        resource: denied,
        cycle: denied,
        cancelled: denied,
        protocol: denied,
        panicked: denied,
    };
    let mut discovery =
        AdmittedRuntime::new(jobs, budget.reserve(runtime_bytes).unwrap(), errors).unwrap();
    let factory = Arc::new(InstanceTaskFactory::new(instances.clone()));
    for kind in [
        hirsel_native::query_model::INSTANCE_KIND,
        hirsel_native::query_model::CALLABLE_VALUE_KIND,
        queries::instance_tasks::CALLABLE_GRAPH_KIND,
        INSTANCE_INPUT_KIND,
        DISCOVERY_KIND,
    ] {
        discovery.register(kind, factory.clone()).unwrap();
    }
    let factory = Arc::new(ArenaTaskFactory::new(arenas.clone()));
    for kind in [
        hirsel_native::query_model::ARENA_CANDIDATES_KIND,
        hirsel_native::query_model::ARENA_CHOSEN_KIND,
        ARENA_GRAPH_KIND,
    ] {
        discovery.register(kind, factory.clone()).unwrap();
    }
    let request = discovery.request(&[instances.discovery_id(), arenas.query_id()]);
    let selected = discovery.wait(request).unwrap_or_else(|error| {
        panic!(
            "{}",
            String::from_utf8_lossy(&store.get_content(error).unwrap())
        )
    });
    let completed = instances
        .completed_instances((selected[0]).clone())
        .unwrap();
    let operations = instances.source_operations((selected[0]).clone()).unwrap();
    let snapshot = NativeSnapshot::new(
        store.clone(),
        instances.products((selected[0]).clone()).unwrap(),
        arenas.graph((selected[1]).clone()).unwrap(),
        options,
        emission,
        manifest,
        Arc::new(operations),
    )
    .unwrap()
    .with_store_verification(verification)
    .unwrap();
    let native = NativeTasks::new(snapshot, weight).unwrap();
    let roots = NativeRootTasks::new(
        native.clone(),
        source.clone(),
        NativeRootKey {
            roots: source_selection.to_vec(),
            target_abi: target.digest(),
            policy,
        },
        weight,
    )
    .unwrap();
    let keys = completed
        .iter()
        .map(|(owner, _)| CodegenKey {
            owner: owner.clone(),
            backend: backend.name().into(),
            backend_version: ContentHash::of_bytes(&COMPILER_IDENTITY),
            target_features: target.digest(),
            profile_options: ContentHash::of_bytes(format!("{opt:?}/{debug}").as_bytes()),
            runtime_abi: ContentHash::of_bytes(&hirsel_lir::RUNTIME_ABI_VERSION.to_le_bytes()),
        })
        .collect::<Vec<_>>();
    let previous = keys
        .iter()
        .map(|key| store.lookup_product(key.address()))
        .collect::<Vec<_>>();
    let objects = CodegenTasks::new(native.clone(), keys.clone(), weight).unwrap();
    drop(discovery);
    let mut runtime =
        AdmittedRuntime::new(jobs, budget.reserve(runtime_bytes).unwrap(), errors).unwrap();
    let factory = Arc::new(NativeTaskFactory::new(native.clone()));
    for kind in [
        hirsel_native::query_model::INSTANCE_KIND,
        hirsel_native::query_model::CALLABLE_VALUE_KIND,
        hirsel_native::query_model::ARENA_CANDIDATES_KIND,
        hirsel_native::query_model::ARENA_CHOSEN_KIND,
        hirsel_native::query_model::READY_KIND,
    ] {
        runtime.register(kind, factory.clone()).unwrap();
    }
    runtime
        .register(
            hirsel_native::query_model::ROOT_KIND,
            Arc::new(NativeRootFactory::new(roots.clone())),
        )
        .unwrap();
    runtime
        .register(
            hirsel_native::object_queries::CODEGEN_KIND,
            Arc::new(ObjectTaskFactory::new(objects)),
        )
        .unwrap();
    if execution {
        let tasks = ExecutionTasks::new(roots.clone(), &[], weight);
        runtime
            .register(EXECUTION_KIND, Arc::new(ExecutionFactory(tasks.clone())))
            .unwrap();
        let request = runtime.request(&[tasks.query_id()]);
        runtime.wait(request).unwrap_or_else(|error| {
            panic!(
                "{}",
                String::from_utf8_lossy(&store.get_content(error).unwrap())
            )
        });
    }
    let requests = keys
        .iter()
        .map(CodegenKey::query_id)
        .chain(keys.iter().map(|key| key.owner.ready_query_id()))
        .chain(std::iter::once(roots.query_id()))
        .collect::<Vec<_>>();
    let request = runtime.request(&requests);
    let selected = runtime.wait(request).unwrap_or_else(|error| {
        panic!(
            "{}",
            String::from_utf8_lossy(&store.get_content(error).unwrap())
        )
    });
    let count = keys.len();
    let hits = previous
        .iter()
        .zip(&selected[..count])
        .filter(|(old, now)| {
            old.as_ref().is_some_and(|old| {
                old.action == now.action
                    && old.reference
                        == now
                            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                            .unwrap()
                            .reference()
            })
        })
        .count();
    let artifacts = selected[..count]
        .iter()
        .map(|product| {
            load_object(
                store.as_ref(),
                fragment_product(store.as_ref(), product.clone())
                    .unwrap()
                    .object,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let ready = &selected[count..count * 2];
    let (mut products, root) = roots
        .products((selected[count * 2]).clone(), ready)
        .unwrap();
    let readies = ready
        .iter()
        .map(|product| {
            hirsel_native::query_model::ready_product(store.as_ref(), product.clone()).map(Arc::new)
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    inspect(&mut products, &root, &readies);
    let link_objects = products
        .backend_objects(&root, &readies, backend, &artifacts)
        .unwrap();
    let (summaries, output) = products
        .with_link_context(&root, &readies, |program| {
            assert!(
                program.units.is_empty(),
                "fragment checks reconstructed Ready bodies"
            );
            Ok((
                program.summaries.clone(),
                use_context(program, &link_objects),
            ))
        })
        .unwrap();
    assert_eq!(summaries.len(), artifacts.len());
    (
        Objects {
            summaries,
            objects: artifacts,
            hits,
            products: keys
                .into_iter()
                .zip(selected[..count].iter().cloned())
                .collect(),
            instance_imports: instances.imports.load(std::sync::atomic::Ordering::Relaxed),
            ready_products: ready.to_vec(),
            ready_transformations: native
                .ready_transformations
                .load(std::sync::atomic::Ordering::Relaxed),
        },
        output,
    )
}

#[test]
fn native_products_materialize_stored_ready_bodies_with_bounded_live_layout_readers() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let transport = 64 << 20;
        let budget = hirsel_engine_admission::ProcessBudget::new(transport);
        let request = Arc::new(
            hirsel_engine_store::RequestStore::with_scratch(
                Arc::new(hirsel_engine_store::NoneStore),
                budget.reserve(transport).unwrap(),
                directory.path(),
                transport,
            )
            .unwrap(),
        );
        let store: Arc<dyn ResultStore> = request.clone();
        let mut engine = QueryEngine::new(
            default_environment(),
            hirsel_front_phases::compiler_adapters(),
        )
        .with_result_store(store.clone());
        let mut source = String::new();
        let mut roots = Vec::new();
        for index in 0..64 {
            let fields = (0..16)
                .map(|field| format!("x{field}:u64"))
                .collect::<Vec<_>>()
                .join(",");
            source.push_str(&format!("pub type Payload{index} {{ {fields} }}\npub fn body{index}(value:Payload{index})->u64 {{ {index}u64 }}\n"));
            let mut owner = entry_path(&default_context());
            owner.0.name = format!("body{index}");
            roots.push(owner);
        }
        let expected = roots
            .iter()
            .enumerate()
            .map(|(index, owner)| {
                (
                    owner.clone(),
                    hirsel_lir::ir::ConstantValue::Integer(index as u128),
                )
            })
            .collect::<BTreeMap<_, _>>();
        roots.sort();
        let file = engine
            .sources
            .add("bounded-ready-products.hir", source)
            .unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        let mut options =
            hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
        options.validation = hirsel_native::Validation::Ready;
        let (_, ()) = compile_roots_inspect(
            &mut engine,
            store.clone(),
            &roots,
            &roots,
            hirsel_native::Backend::Cranelift,
            hirsel_native::OptLevel::None,
            options,
            false,
            hirsel_intern::jobs::Jobs::ONE,
            hirsel_engine_store::VerifyStore::Off,
            |products, _, readies| {
                // Unrelated live transport leaves room for one closure and
                // read buffer, not the sum of every decoded layout reader.
                let pressure = request
                    .admit_product_memory(transport - request.retained_bytes() - (128 << 10))
                    .unwrap();
                products
                    .with_ready(readies, |units, _| {
                        let mut answers = BTreeMap::new();
                        for unit in units {
                            let unit = unit.unit();
                            let Some(expected_value) = expected.get(&unit.header.definition) else {
                                continue;
                            };
                            let returns = unit
                                .terminators
                                .iter()
                                .filter_map(|term| match term {
                                    hirsel_lir::ir::Terminator::Return(Some(value)) => Some(*value),
                                    _ => None,
                                })
                                .collect::<Vec<_>>();
                            assert!(!returns.is_empty());
                            for returned in returns {
                                use hirsel_lir::ir::Inst;
                                let mut pending = vec![returned];
                                let mut seen = std::collections::BTreeSet::new();
                                let mut answer = None;
                                while let Some(value) = pending.pop() {
                                    if !seen.insert(value) {
                                        continue;
                                    }
                                    if let Some(constant) =
                                        unit.instructions.iter().find_map(|inst| match inst {
                                            Inst::Const { result, constant }
                                                if *result == value =>
                                            {
                                                Some(
                                                    unit.tables.constants[constant.index()]
                                                        .value
                                                        .clone(),
                                                )
                                            }
                                            _ => None,
                                        })
                                    {
                                        assert_eq!(&constant, expected_value);
                                        answer = Some(constant);
                                        continue;
                                    }
                                    let forwarded =
                                        unit.instructions.iter().find_map(|inst| match inst {
                                            Inst::Read {
                                                result, reaching, ..
                                            }
                                            | Inst::Copy {
                                                result, reaching, ..
                                            } if *result == value => Some(*reaching),
                                            Inst::CopyValue {
                                                result,
                                                value: source,
                                            } if *result == value => Some(*source),
                                            Inst::Store {
                                                version,
                                                value: source,
                                                ..
                                            } if *version == value => Some(*source),
                                            _ => None,
                                        });
                                    if let Some(source) = forwarded {
                                        pending.push(source);
                                        continue;
                                    }
                                    let (block, ordinal) = unit
                                        .blocks
                                        .iter()
                                        .enumerate()
                                        .find_map(|(block, data)| {
                                            data.parameters
                                                .get(&unit.pools.values)
                                                .unwrap()
                                                .iter()
                                                .position(|parameter| *parameter == value)
                                                .map(|ordinal| (block, ordinal))
                                        })
                                        .unwrap_or_else(|| {
                                            panic!("unresolved return value {value:?}: {unit:#?}")
                                        });
                                    let incoming = unit
                                        .tables
                                        .edges
                                        .iter()
                                        .filter(|edge| edge.target.index() == block)
                                        .map(|edge| {
                                            edge.arguments.get(&unit.pools.values).unwrap()[ordinal]
                                        })
                                        .collect::<Vec<_>>();
                                    assert!(!incoming.is_empty());
                                    pending.extend(incoming);
                                }
                                answers.insert(unit.header.definition.clone(), answer.unwrap());
                            }
                        }
                        assert_eq!(answers, expected);
                        Ok(())
                    })
                    .unwrap();
                drop(pressure);
            },
            |_, _| (),
        );
    });
}

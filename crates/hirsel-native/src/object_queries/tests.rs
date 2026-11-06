use super::*;
use hirsel_engine_store::{DirStore, MAX_RECORD};

use crate::query_model::*;
fn output_path(store: &DirStore, reference: Reference) -> std::path::PathBuf {
    hirsel_artifact_store::Store::new(store.root().into()).output_path(reference)
}
use hirsel_lir::layout_types::TargetSpec;
use std::cell::Cell;

fn admitted(backend: Arc<dyn ResultStore>) -> hirsel_engine_store::RequestStore {
    let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
    hirsel_engine_store::RequestStore::new(backend, budget.reserve(64 << 20).unwrap())
}

const INPUT: Domain = Domain {
    kind: 0x4f00ff00,
    schema: 1,
};
fn hash(s: &str) -> ContentHash {
    ContentHash::of_bytes(s.as_bytes())
}
fn input(store: &dyn ResultStore, s: &str) -> Reference {
    store.put_content(INPUT, s.as_bytes()).unwrap()
}
fn product(store: &dyn ResultStore, s: &str) -> ProductRef {
    fixture_product(store, Address(hash(s)), Action(hash(s)), input(store, s))
}
fn key(name: &str) -> CodegenKey {
    let types = hirsel_types::TypeInterner::new(None);
    CodegenKey {
        owner: InstanceAddress {
            instance: hirsel_layout::instance::InstanceKey::new(
                &hirsel_syntax::DefPath::core(name),
                0,
                &[],
                &types,
                None,
                None,
            )
            .unwrap(),
            target_abi: TargetSpec::x86_64_unknown_linux_gnu().digest(),
            policy: hash("VerifyOn"),
        },
        backend: "llvm".into(),
        backend_version: hash("llvm23.1.5"),
        target_features: hash("features"),
        profile_options: hash("debug"),
        runtime_abi: hash("abi"),
    }
}
fn ready(store: &dyn ResultStore) -> ReadyProduct {
    ReadyProduct {
        product: product(store, "Ready product"),
        debug: true,
        ready: input(store, "Ready"),
        layout_closure: input(store, "layouts"),
        emission_context: input(store, "emit"),
        unit_summary: input(store, "summary"),
        optimizer_reads: vec![],
    }
}
fn object() -> ObjectArtifact {
    ObjectArtifact {
        ahead: None,
        target: TargetSpec::x86_64_unknown_linux_gnu().digest(),
        format: hirsel_lir::layout_types::ObjectFormat::Elf,
        bytes: b"fixture object".to_vec(),
        exports: vec![],
        imports: vec![],
        relocations: vec![],
        abi: hash("abi"),
        reproducibility: hirsel_lir::stage::Reproducibility {
            toolchain: "fixture".into(),
            options: hash("options"),
            lir_version: hirsel_lir::LIR_VERSION,
            runtime_abi_version: hirsel_lir::RUNTIME_ABI_VERSION,
        },
        debug: vec![],
    }
}
fn inputs(store: &dyn ResultStore, fragments: Vec<FragmentProduct>) -> LinkInputs {
    LinkInputs {
        execution: vec![],
        fragments,
        static_manifest: input(store, "statics"),
        runtime_archives: vec![input(store, "runtime")],
        foreign_inputs: vec![],
        launch_manifest: input(store, "launch"),
    }
}
fn link_key() -> LinkKey {
    LinkKey {
        roots: vec![key("main").owner].into(),
        target: object().target,
        linker_options: hash("linker"),
    }
}

#[test]
fn addresses_cover_every_option_and_canonical_roots() {
    let original = key("main");
    for axis in 0..9 {
        let mut changed = original.clone();
        match axis {
            0 => changed.owner = key("other").owner,
            1 => changed.owner.target_abi = hash("target2"),
            2 => changed.owner.policy = hash("VerifyOff"),
            3 => changed.backend = "cranelift".into(),
            4 => changed.backend_version = hash("backend2"),
            5 => changed.target_features = hash("features2"),
            6 => changed.profile_options = hash("release"),
            7 => changed.runtime_abi = hash("abi2"),
            8 => changed.owner.instance = key("other").owner.instance,
            _ => unreachable!(),
        }
        assert_ne!(original.address(), changed.address(), "axis {axis}");
    }
    let mut a = link_key();
    let mut roots = a.roots.to_vec();
    roots.push(key("other").owner);
    a.roots = roots.into();
    let mut b = a.clone();
    Arc::make_mut(&mut b.roots).reverse();
    assert_eq!(a.address(), b.address());
    b.target = hash("different target");
    assert_ne!(a.address(), b.address());
    b = a.clone();
    b.linker_options = hash("different linker");
    assert_ne!(a.address(), b.address());
}

#[test]
fn hit_never_materializes_and_equal_recomputation_backdates() {
    let dir = tempfile::tempdir().unwrap();
    let store = admitted(Arc::new(DirStore::new(dir.path().into())));
    let key = key("main");
    let mut ready = ready(&store);
    let cold = codegen(&store, &key, &ready, || Ok(object())).unwrap();
    let warm = codegen(&store, &key, &ready, || panic!("hit materialized a body")).unwrap();
    assert_eq!(cold.product, warm.product);
    ready.product.action = Action(hash("recomputed with equal output"));
    assert_eq!(
        codegen(&store, &key, &ready, || panic!(
            "equal action output missed"
        ))
        .unwrap()
        .product,
        cold.product
    );
    ready.ready = input(&store, "changed Ready, equal object");
    let changed = codegen(&store, &key, &ready, || Ok(object())).unwrap();
    assert_ne!(changed.product.action, cold.product.action);
    assert_eq!(
        changed
            .product
            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .unwrap()
            .reference(),
        cold.product
            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .unwrap()
            .reference(),
        "equal output failed to backdate"
    );
    let fresh = admitted(Arc::new(DirStore::new(dir.path().into())));
    assert_eq!(
        codegen(&fresh, &key, &ready, || panic!("fresh hit decoded body"))
            .unwrap()
            .product,
        changed.product
    );
}

#[test]
fn optimizer_body_reads_invalidate_release_but_not_ordinary_debug_calls() {
    let dir = tempfile::tempdir().unwrap();
    let store = admitted(Arc::new(DirStore::new(dir.path().into())));
    let debug = key("caller");
    let ordinary = ready(&store);
    let d = codegen(&store, &debug, &ordinary, || Ok(object())).unwrap();
    let mut release = debug.clone();
    release.profile_options = hash("release");
    let mut copied = ordinary.clone();
    copied.optimizer_reads = vec![product(&store, "callee before")];
    let r = codegen(&store, &release, &copied, || Ok(object())).unwrap();
    copied.optimizer_reads[0] = product(&store, "same-summary callee body after");
    let calls = Cell::new(0);
    let after = codegen(&store, &release, &copied, || {
        calls.set(calls.get() + 1);
        Ok(object())
    })
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_ne!(r.product.action, after.product.action);
    assert_eq!(
        d.product,
        codegen(&store, &debug, &ordinary, || panic!(
            "ordinary caller read callee body"
        ))
        .unwrap()
        .product
    );
}

#[test]
fn complete_ready_context_controls_hits() {
    let dir = tempfile::tempdir().unwrap();
    let store = admitted(Arc::new(DirStore::new(dir.path().into())));
    let key = key("main");
    let original = ready(&store);
    for axis in 0..5 {
        let _ = codegen(&store, &key, &original, || Ok(object())).unwrap();
        let mut changed = original.clone();
        let next = input(&store, &format!("changed-{axis}"));
        match axis {
            0 => {
                changed.product = fixture_product(
                    &store,
                    changed.product.address,
                    changed.product.action,
                    next,
                )
            }
            1 => changed.ready = next,
            2 => changed.layout_closure = next,
            3 => changed.emission_context = next,
            4 => changed.unit_summary = next,
            _ => unreachable!(),
        }
        let called = Cell::new(false);
        codegen(&store, &key, &changed, || {
            called.set(true);
            Ok(object())
        })
        .unwrap();
        assert!(called.get(), "axis {axis}");
    }
}

#[test]
fn corrupt_or_missing_object_recomputes_instead_of_hitting() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(DirStore::new(dir.path().into()));
    let store = admitted(backend.clone());
    let key = key("main");
    let ready = ready(&store);
    for bad in [b"truncated".as_slice(), b"another corrupt payload"] {
        let cold = codegen(&store, &key, &ready, || Ok(object())).unwrap();
        let record: ObjectRecord = get_record(&store, OBJECT, cold.object).unwrap();
        let blob: BlobRecord = get_record(&store, BLOB, record.bytes.into()).unwrap();
        let path = output_path(&backend, blob.chunks[0].clone().into());
        std::fs::write(&path, bad).unwrap();
        let called = Cell::new(false);
        codegen(&store, &key, &ready, || {
            called.set(true);
            Ok(object())
        })
        .unwrap();
        assert!(called.get());
    }
    for corrupt in [false, true] {
        let cold = codegen(&store, &key, &ready, || Ok(object())).unwrap();
        let reference = cold
            .product
            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .unwrap()
            .reference();
        let path = output_path(&backend, reference);
        if corrupt {
            std::fs::write(path, b"corrupt descriptor").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        let called = Cell::new(false);
        let repaired = codegen(&store, &key, &ready, || {
            called.set(true);
            Ok(object())
        })
        .unwrap();
        assert!(called.get());
        assert_eq!(
            repaired
                .product
                .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
            reference
        );
    }
}

#[test]
fn link_order_is_canonical_and_archive_order_is_significant() {
    let dir = tempfile::tempdir().unwrap();
    let store = admitted(Arc::new(DirStore::new(dir.path().into())));
    let ready = ready(&store);
    let a = codegen(&store, &key("a"), &ready, || Ok(object())).unwrap();
    let b = codegen(&store, &key("b"), &ready, || Ok(object())).unwrap();
    let mut manifest = inputs(&store, vec![b, a]);
    let calls = Cell::new(0);
    let mut compute = |_: &LinkKey, _: &LinkInputs, _: &[ObjectArtifact]| {
        calls.set(calls.get() + 1);
        Ok(b"executable".to_vec())
    };
    let first = link(&store, &link_key(), &manifest, &mut compute).unwrap();
    manifest.fragments.reverse();
    let second = link(&store, &link_key(), &manifest, &mut compute).unwrap();
    assert_eq!(first.product, second.product);
    assert_eq!(calls.get(), 1);
    manifest
        .runtime_archives
        .push(input(&store, "second runtime"));
    let third = link(&store, &link_key(), &manifest, &mut compute).unwrap();
    manifest.runtime_archives.reverse();
    let fourth = link(&store, &link_key(), &manifest, &mut compute).unwrap();
    assert_ne!(third.product.action, fourth.product.action);
    assert_eq!(
        third
            .product
            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .unwrap()
            .reference(),
        fourth
            .product
            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .unwrap()
            .reference()
    );
    assert_eq!(calls.get(), 3);
    manifest.fragments.push(manifest.fragments[0].clone());
    assert!(link(&store, &link_key(), &manifest, &mut compute).is_err());
}

#[test]
fn linked_hit_refuses_stale_fragments_and_relinks_missing_executable() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(DirStore::new(dir.path().into()));
    let store = admitted(backend.clone());
    let fragment = codegen(&store, &key("main"), &ready(&store), || Ok(object())).unwrap();
    let manifest = inputs(&store, vec![fragment.clone()]);
    let mut compute = |_: &LinkKey, _: &LinkInputs, _: &[ObjectArtifact]| Ok(b"exe".to_vec());
    let first = link(&store, &link_key(), &manifest, &mut compute).unwrap();
    std::fs::remove_file(output_path(&backend, first.executable)).unwrap();
    let called = Cell::new(false);
    link(&store, &link_key(), &manifest, &mut |_, _, _| {
        called.set(true);
        Ok(b"exe".to_vec())
    })
    .unwrap();
    assert!(called.get());
    for corrupt in [false, true] {
        let old = link(&store, &link_key(), &manifest, &mut compute).unwrap();
        let reference = old
            .product
            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .unwrap()
            .reference();
        let path = output_path(&backend, reference);
        if corrupt {
            std::fs::write(path, b"corrupt descriptor").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        let called = Cell::new(false);
        let repaired = link(&store, &link_key(), &manifest, &mut |_, _, _| {
            called.set(true);
            Ok(b"exe".to_vec())
        })
        .unwrap();
        assert!(called.get());
        assert_eq!(
            repaired
                .product
                .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
            reference
        );
    }
    std::fs::remove_file(output_path(&backend, fragment.object)).unwrap();
    assert!(
        link(&store, &link_key(), &manifest, &mut |_, _, _| panic!(
            "stale fragment reached linker"
        ))
        .is_err()
    );
}

#[test]
fn static_foreign_launch_runtime_and_linker_changes_invalidate() {
    let dir = tempfile::tempdir().unwrap();
    let store = admitted(Arc::new(DirStore::new(dir.path().into())));
    let original = inputs(&store, vec![]);
    let key = link_key();
    let mut compute = |_: &LinkKey, _: &LinkInputs, _: &[ObjectArtifact]| Ok(b"exe".to_vec());
    let first = link(&store, &key, &original, &mut compute).unwrap();
    for axis in 0..5 {
        let mut changed = original.clone();
        let mut changed_key = key.clone();
        let next = input(&store, &format!("manifest-{axis}"));
        match axis {
            0 => changed.static_manifest = next,
            1 => changed.foreign_inputs.push(next),
            2 => changed.launch_manifest = next,
            3 => changed.runtime_archives[0] = next,
            4 => changed_key.linker_options = hash("new linker"),
            _ => unreachable!(),
        }
        let after = link(&store, &changed_key, &changed, &mut compute).unwrap();
        assert_ne!(first.product.action, after.product.action, "axis {axis}");
    }
}

#[test]
fn chunk_boundaries_domains_and_complete_lengths_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let store = DirStore::new(dir.path().into());
    let bytes = vec![0x5a; MAX_RECORD + 1];
    let reference = put_blob(&store, &bytes).unwrap();
    assert_eq!(read_blob(&store, reference).unwrap(), bytes);
    let mut record: BlobRecord = get_record(&store, BLOB, reference).unwrap();
    record.len += 1;
    let bad = put_record(&store, BLOB, &record).unwrap();
    assert!(validate_link_input(&store, bad).is_err());
    record.len -= 1;
    record.chunks[0].kind = INPUT.kind;
    let bad = put_record(&store, BLOB, &record).unwrap();
    assert!(validate_link_input(&store, bad).is_err());
    assert!(
        validate_link_input(
            &store,
            Reference {
                domain: INPUT,
                ..reference
            }
        )
        .is_err()
    );
}

#[test]
fn explicit_recompute_checks_exact_cached_bytes() {
    let store = admitted(Arc::new(hirsel_engine_store::MemStore::default()));
    let key = key("main");
    let descriptor = ready(&store);
    let cold = codegen(&store, &key, &descriptor, || Ok(object())).unwrap();
    let calls = Cell::new(0);
    let checked = codegen_checked(
        &store,
        &key,
        &descriptor,
        &[],
        None,
        VerifyStore::Recompute,
        || {
            calls.set(calls.get() + 1);
            Ok(object())
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(cold.product, checked.product);
    let error = codegen_checked(
        &store,
        &key,
        &descriptor,
        &[],
        None,
        VerifyStore::Recompute,
        || {
            let mut changed = object();
            changed.bytes.push(0);
            Ok(changed)
        },
    )
    .unwrap_err();
    assert!(error.contains("differs from cached output"), "{error}");
}

#[test]
fn object_hit_never_reads_ready_body_payload() {
    struct NoBody {
        inner: hirsel_engine_store::MemStore,
        body: Reference,
    }
    impl ResultStore for NoBody {
        fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
            self.inner.admit_product_memory(bytes)
        }

        fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
            self.inner.retention_scope()
        }

        fn capability(&self) -> hirsel_engine_store::ProviderCapability {
            self.inner.capability()
        }

        fn get(&self, key: &hirsel_engine_store::EntryKey) -> Option<hirsel_artifact_store::Bytes> {
            self.inner.get(key)
        }
        fn put(
            &self,
            key: &hirsel_engine_store::EntryKey,
            value: &[u8],
        ) -> hirsel_engine_store::WriteOutcome {
            self.inner.put(key, value)
        }
        fn get_content(&self, r: Reference) -> Option<hirsel_artifact_store::Bytes> {
            assert_ne!(r, self.body, "object hit read unchanged Ready body");
            self.inner.get_content(r)
        }
        fn put_content(&self, d: Domain, b: &[u8]) -> Option<Reference> {
            self.inner.put_content(d, b)
        }
        fn name(&self) -> &'static str {
            "fixture-no-body"
        }
    }
    let inner = hirsel_engine_store::MemStore::default();
    let descriptor = ready(&inner);
    let store = admitted(Arc::new(NoBody {
        inner,
        body: descriptor.ready,
    }));
    let key = key("main");
    let cold = codegen(&store, &key, &descriptor, || Ok(object())).unwrap();
    let warm = codegen(&store, &key, &descriptor, || {
        panic!("hit materialized Ready")
    })
    .unwrap();
    assert_eq!(cold.product, warm.product);
}

use crate::object_queries::tasks::{
    LinkDependencies, ObjectTaskFactory, ObjectTaskRequest, ObjectTaskSnapshot,
};
use hirsel_engine_runtime::{MemoryWeight, QueryContext, QueryFactory, QueryId, QueryPoll};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

struct TaskSnapshot {
    request: ObjectTaskRequest,
    execution: Option<QueryId>,
    ready: ReadyProduct,
    link: LinkDependencies,
    result: ProductRef,
    store: Arc<dyn ResultStore>,
    makes: Arc<AtomicUsize>,
}
struct TaskProducts {
    result: ProductRef,
    reference: Reference,
}
impl ObjectProducts for TaskProducts {
    fn codegen_whole(
        &mut self,
        _: &crate::object_queries::whole::WholeCodegenKey,
    ) -> Result<Arc<FragmentProduct>, String> {
        Ok(Arc::new(FragmentProduct {
            product: self.result.clone(),
            object: self.reference,
            summary: self.reference,
        }))
    }
    fn codegen(&mut self, _: &CodegenKey) -> Result<Arc<FragmentProduct>, String> {
        Ok(Arc::new(FragmentProduct {
            product: self.result.clone(),
            object: self.reference,
            summary: self.reference,
        }))
    }
    fn link(&mut self, _: &LinkKey, _: &LinkInputs) -> Result<Arc<LinkedProduct>, String> {
        Ok(Arc::new(LinkedProduct {
            product: self.result.clone(),
            executable: self.reference,
        }))
    }
}
impl ObjectTaskSnapshot for TaskSnapshot {
    fn execution_query(&self) -> Option<QueryId> {
        self.execution
    }
    fn store(&self) -> &dyn ResultStore {
        self.store.as_ref()
    }
    fn request(&self, _: QueryId) -> Result<ObjectTaskRequest, Reference> {
        Ok(self.request.clone())
    }
    fn ready_descriptor(&self, p: ProductRef) -> Result<ReadyProduct, Reference> {
        assert_eq!(p, self.ready.product);
        Ok(self.ready.clone())
    }
    fn optimizer_query(&self, p: ProductRef) -> Result<QueryId, Reference> {
        Ok(QueryId {
            kind: 0x4e000004,
            address: p.address,
        })
    }
    fn link_dependencies(&self, _: ProductRef) -> Result<LinkDependencies, Reference> {
        Ok(self.link.clone())
    }
    fn link_input(&self, p: ProductRef) -> Result<Reference, Reference> {
        p.archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
            .map(|archive| archive.reference())
            .map_err(|_| self.failure("link product archive unavailable"))
    }
    fn products(&self, settled: &[ProductRef]) -> Result<Box<dyn ObjectProducts>, Reference> {
        assert!(!settled.is_empty());
        self.makes.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(TaskProducts {
            result: self.result.clone(),
            reference: self
                .result
                .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
        }))
    }
    fn failure(&self, message: &str) -> Reference {
        input(self.store.as_ref(), message)
    }
}
#[derive(Default)]
struct TaskContext {
    settled: Vec<(QueryId, Result<ProductRef, Reference>)>,
    reads: Vec<QueryId>,
}
impl QueryContext for TaskContext {
    fn read(&mut self, id: QueryId) -> Poll<Result<ProductRef, Reference>> {
        self.reads.push(id);
        self.settled
            .iter()
            .find(|(q, _)| *q == id)
            .map_or(Poll::Pending, |(_, p)| Poll::Ready(p.clone()))
    }
}
fn query(product: ProductRef) -> QueryId {
    QueryId {
        kind: 0x4e000004,
        address: product.address,
    }
}
fn task_snapshot(
    request: ObjectTaskRequest,
    ready: ReadyProduct,
    store: Arc<dyn ResultStore>,
    result: ProductRef,
) -> Arc<TaskSnapshot> {
    Arc::new(TaskSnapshot {
        request,
        execution: None,
        ready,
        link: LinkDependencies {
            execution: vec![],
            fragments: vec![],
            native_reads: vec![],
            static_manifest: query(result.clone()),
            runtime_archives: vec![],
            foreign_inputs: vec![],
            launch_manifest: query(result.clone()),
        },
        result,
        store,
        makes: Arc::new(AtomicUsize::new(0)),
    })
}

#[test]
fn codegen_stages_local_ready_while_execution_waits_and_preserves_failure_order() {
    for whole in [false, true] {
        for (execution_ok, ready_ok) in [(true, true), (false, false), (true, false)] {
            let store = Arc::new(hirsel_engine_store::MemStore::default());
            let mut ready = ready(store.as_ref());
            ready.optimizer_reads.clear();
            let key = key("caller");
            let owner = key.owner.ready_query_id();
            ready.product.address = owner.address;
            let weight = MemoryWeight {
                retained: 4096,
                ..MemoryWeight::default()
            };
            let request = if whole {
                ObjectTaskRequest::WholeCodegen {
                    key: crate::object_queries::whole::WholeCodegenKey::new(vec![key]).unwrap(),
                    weight,
                }
            } else {
                ObjectTaskRequest::Codegen {
                    key,
                    ready: owner,
                    weight,
                }
            };
            let id = match &request {
                ObjectTaskRequest::Codegen { key, .. } => key.query_id(),
                ObjectTaskRequest::WholeCodegen { key, .. } => key.query_id(),
                _ => unreachable!(),
            };
            let mut result = product(store.as_ref(), "emitted");
            result.address = id.address;
            let execution_product = product(store.as_ref(), "execution");
            let execution = query(execution_product.clone());
            let mut snapshot = task_snapshot(request, ready.clone(), store.clone(), result.clone());
            Arc::get_mut(&mut snapshot).unwrap().execution = Some(execution);
            let mut task = ObjectTaskFactory::new(snapshot.clone()).create(id).unwrap();
            let mut cx = TaskContext::default();
            assert!(
                matches!(task.poll(&mut cx), QueryPoll::Waiting(p) if p == vec![execution, owner])
            );
            assert_eq!(cx.reads, vec![execution, owner]);
            let ready_error = input(store.as_ref(), "Ready refused");
            cx.settled.push((
                owner,
                if ready_ok {
                    Ok(ready.product)
                } else {
                    Err(ready_error)
                },
            ));
            assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(p) if p == vec![execution]));
            assert_eq!(snapshot.makes.load(Ordering::SeqCst), 0);
            let execution_error = input(store.as_ref(), "execution refused");
            cx.settled.push((
                execution,
                if execution_ok {
                    Ok(execution_product)
                } else {
                    Err(execution_error)
                },
            ));
            match task.poll(&mut cx) {
                QueryPoll::Complete(p) => {
                    assert!(execution_ok && ready_ok);
                    assert_eq!(p, result);
                }
                QueryPoll::Failed(error) => assert_eq!(
                    error,
                    if execution_ok {
                        ready_error
                    } else {
                        execution_error
                    }
                ),
                QueryPoll::Waiting(_) | QueryPoll::WaitingAny(_) => {
                    panic!("all required inputs settled")
                }
            }
            assert_eq!(
                snapshot.makes.load(Ordering::SeqCst),
                usize::from(execution_ok && ready_ok)
            );
        }
    }
}

#[test]
fn codegen_task_create_is_lazy_and_waits_for_complete_optimizer_inventory() {
    let store = Arc::new(admitted(Arc::new(hirsel_engine_store::MemStore::default())));
    let mut ready = ready(store.as_ref());
    ready.optimizer_reads = vec![
        product(store.as_ref(), "copy1"),
        product(store.as_ref(), "copy2"),
    ];
    let key = key("caller");
    let id = key.query_id();
    let owner = query(ready.product.clone());
    let mut result = product(store.as_ref(), "emitted");
    result.address = id.address;
    let denied = task_snapshot(
        ObjectTaskRequest::Codegen {
            key: key.clone(),
            ready: owner,
            weight: MemoryWeight::default(),
        },
        ready.clone(),
        store.clone(),
        result.clone(),
    );
    assert!(matches!(
        ObjectTaskFactory::new(denied.clone()).create(id),
        Err(error) if error == input(store.as_ref(), "object task frame exceeds retained allowance")
    ));
    assert_eq!(denied.makes.load(Ordering::SeqCst), 0);
    let snapshot = task_snapshot(
        ObjectTaskRequest::Codegen {
            key,
            ready: owner,
            // The factory owns a Codegen continuation and its key. This
            // synthetic request must admit that frame before dependency reads.
            weight: MemoryWeight {
                retained: 4096,
                ..MemoryWeight::default()
            },
        },
        ready.clone(),
        store,
        result.clone(),
    );
    let mut task = ObjectTaskFactory::new(snapshot.clone()).create(id).unwrap();
    let mut cx = TaskContext::default();
    assert_eq!(snapshot.makes.load(Ordering::SeqCst), 0);
    assert!(matches!(task.poll(&mut cx),QueryPoll::Waiting(ref p) if p==&vec![owner]));
    cx.settled.push((owner, Ok(ready.product)));
    let copies: Vec<_> = ready.optimizer_reads.iter().cloned().map(query).collect();
    assert!(matches!(task.poll(&mut cx),QueryPoll::Waiting(ref p) if p==&copies));
    assert_eq!(snapshot.makes.load(Ordering::SeqCst), 0);
    for (q, p) in copies.iter().zip(&ready.optimizer_reads) {
        cx.settled.push((*q, Ok((*p).clone())));
    }
    assert!(matches!(task.poll(&mut cx),QueryPoll::Complete(p) if p==result));
    assert!(matches!(task.poll(&mut cx),QueryPoll::Complete(p) if p==result));
    assert_eq!(snapshot.makes.load(Ordering::SeqCst), 1);
}

#[test]
fn codegen_task_rejects_stale_optimizer_and_dependency_failure_before_emission() {
    for stale in [false, true] {
        let store = Arc::new(hirsel_engine_store::MemStore::default());
        let mut ready = ready(store.as_ref());
        let copy = product(store.as_ref(), "old copy");
        ready.optimizer_reads = vec![copy.clone()];
        let key = key("caller");
        let id = key.query_id();
        let owner = query(ready.product.clone());
        let mut result = product(store.as_ref(), "emitted");
        result.address = id.address;
        let snapshot = task_snapshot(
            ObjectTaskRequest::Codegen {
                key,
                ready: owner,
                // The factory owns a Codegen continuation and its key. This
                // synthetic request must admit that frame before dependency reads.
                weight: MemoryWeight {
                    retained: 4096,
                    ..MemoryWeight::default()
                },
            },
            ready.clone(),
            store.clone(),
            result,
        );
        let mut task = ObjectTaskFactory::new(snapshot.clone()).create(id).unwrap();
        let mut cx = TaskContext::default();
        cx.settled.push((owner, Ok(ready.product)));
        let next = if stale {
            Ok(fixture_product(
                store.as_ref(),
                copy.address,
                copy.action,
                input(store.as_ref(), "new body"),
            ))
        } else {
            Err(input(store.as_ref(), "failed validation"))
        };
        cx.settled.push((query(copy), next));
        assert!(matches!(task.poll(&mut cx), QueryPoll::Failed(_)));
        assert_eq!(snapshot.makes.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn link_task_waits_for_manifest_then_all_fragments_and_runtime_inputs() {
    let store = Arc::new(admitted(Arc::new(hirsel_engine_store::MemStore::default())));
    let ready = ready(store.as_ref());
    let fragment = codegen(store.as_ref(), &key("main"), &ready, || Ok(object())).unwrap();
    let key = link_key();
    let id = key.query_id();
    let manifest = product(store.as_ref(), "manifest");
    let manifest_id = query(manifest.clone());
    let mut result = product(store.as_ref(), "linked");
    result.address = id.address;
    let static_p = product(store.as_ref(), "statics");
    let runtime = product(store.as_ref(), "archive");
    let launch = product(store.as_ref(), "launch");
    let native = product(store.as_ref(), "native link metadata");
    let mut snapshot = task_snapshot(
        ObjectTaskRequest::Link {
            key,
            manifest: manifest_id,
            weight: MemoryWeight::default(),
        },
        ready,
        store,
        result.clone(),
    );
    Arc::get_mut(&mut snapshot).unwrap().link = LinkDependencies {
        execution: vec![],
        fragments: vec![QueryId {
            kind: crate::object_queries::tasks::CODEGEN_KIND,
            address: fragment.product.address,
        }],
        native_reads: vec![query(native.clone())],
        static_manifest: query(static_p.clone()),
        runtime_archives: vec![query(runtime.clone())],
        foreign_inputs: vec![],
        launch_manifest: query(launch.clone()),
    };
    let mut task = ObjectTaskFactory::new(snapshot.clone()).create(id).unwrap();
    let mut cx = TaskContext::default();
    assert!(matches!(task.poll(&mut cx),QueryPoll::Waiting(ref p) if p==&vec![manifest_id]));
    cx.settled.push((manifest_id, Ok(manifest)));
    assert!(matches!(task.poll(&mut cx),QueryPoll::Waiting(ref p) if p.len()==5));
    assert_eq!(snapshot.makes.load(Ordering::SeqCst), 0);
    let deps = &snapshot.link;
    cx.settled.extend([
        (deps.fragments[0], Ok(fragment.product)),
        (deps.static_manifest, Ok(static_p)),
        (deps.runtime_archives[0], Ok(runtime)),
        (deps.launch_manifest, Ok(launch)),
    ]);
    assert!(
        matches!(task.poll(&mut cx),QueryPoll::Waiting(ref p) if p==&vec![deps.native_reads[0]])
    );
    assert_eq!(snapshot.makes.load(Ordering::SeqCst), 0);
    cx.settled.push((deps.native_reads[0], Ok(native)));
    assert!(matches!(task.poll(&mut cx),QueryPoll::Complete(p) if p==result));
    assert_eq!(snapshot.makes.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_blob_schema_one_is_refused_with_streamed_schema_two() {
    let store = hirsel_engine_store::MemStore::default();
    let mut bytes = vec![0x5a; MAX_RECORD];
    bytes.extend_from_slice(b"old tail");
    let old = put_record(
        &store,
        Domain {
            kind: BLOB.kind,
            schema: 1,
        },
        &BlobRecord {
            len: bytes.len() as u64,
            chunks: bytes
                .chunks(MAX_RECORD)
                .map(|chunk| store.put_content(CHUNK, chunk).unwrap().into())
                .collect(),
        },
    )
    .unwrap();
    let current = store_link_input(&store, &bytes).unwrap();
    assert_ne!(old.domain, current.domain);
    assert_eq!(
        link_input_bytes(&store, old).unwrap_err(),
        "wrong native product domain/schema"
    );
    assert_eq!(
        validate_link_input(&store, old).unwrap_err(),
        "wrong native product domain/schema"
    );
    assert_eq!(link_input_bytes(&store, current).unwrap(), bytes);
}

#[test]
fn execution_plan_identity_controls_object_hits_and_imported_keys() {
    let store = admitted(Arc::new(hirsel_engine_store::MemStore::default()));
    let key = key("main");
    let ready = ready(&store);
    let plan_domain = Domain {
        kind: hirsel_native_queries::execution::EXECUTION_KIND,
        schema: 1,
    };
    let context = input(&store, "execution context");
    let make_plan = |ahead, continuation, context, action| {
        let bytes = serde_json::to_vec(&(ahead, continuation, context)).unwrap();
        let reference = store.put_content(plan_domain, &bytes).unwrap();
        fixture_product(&store, Address(hash("plan")), Action(action), reference)
    };
    let original = make_plan(
        hash("ahead"),
        hash("continuation"),
        context,
        hash("first observations"),
    );
    let first = codegen_checked(
        &store,
        &key,
        &ready,
        &[],
        Some(&original),
        VerifyStore::Off,
        || Ok(object()),
    )
    .unwrap();
    let reopened = make_plan(
        hash("ahead"),
        hash("continuation"),
        context,
        hash("new observations"),
    );
    let warm = codegen_checked(
        &store,
        &key,
        &ready,
        &[],
        Some(&reopened),
        VerifyStore::Off,
        || panic!("equal persisted plan identity missed the object cache"),
    )
    .unwrap();
    assert_eq!(first.product, warm.product);
    let recomputed = codegen_checked(
        &store,
        &key,
        &ready,
        &[],
        Some(&reopened),
        VerifyStore::Recompute,
        || Ok(object()),
    )
    .unwrap();
    assert_eq!(first.product, recomputed.product);
    for (ahead, continuation, context) in [
        (hash("changed ahead"), hash("continuation"), context),
        (hash("ahead"), hash("changed continuation"), context),
        (
            hash("ahead"),
            hash("continuation"),
            input(&store, "changed emission context"),
        ),
    ] {
        let changed = make_plan(ahead, continuation, context, hash("observations"));
        let calls = Cell::new(0);
        let output = codegen_checked(
            &store,
            &key,
            &ready,
            &[],
            Some(&changed),
            VerifyStore::Off,
            || {
                calls.set(calls.get() + 1);
                Ok(object())
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 1, "changed plan reused a stale object action");
        assert_ne!(first.product.action, output.product.action);
    }
}

use hirsel_engine_runtime::ArchiveAdmission;
use hirsel_engine_store::{Action, Address, Domain, Output, Reference, ResultStore, VerifyStore};
use hirsel_lir::stage::ObjectArtifact;
use std::path::Path;

fn fixture_product(
    store: &dyn hirsel_engine_store::ResultStore,
    address: hirsel_engine_store::Address,
    action: hirsel_engine_store::Action,
    reference: hirsel_engine_store::Reference,
) -> hirsel_engine_runtime::ProductRef {
    let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
    let mut allowance = budget.reserve(64 << 20).unwrap();
    let archive =
        hirsel_engine_runtime::ProductArchive::open(store, reference, &mut allowance).unwrap();
    hirsel_engine_runtime::ProductRef::import_archive(address, action, archive)
}

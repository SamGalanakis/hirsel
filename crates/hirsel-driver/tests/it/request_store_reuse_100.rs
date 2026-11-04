use hirsel_driver::session::Session;
use hirsel_engine::{
    CheckCompletion, CompileConfig, QueryKind,
    store::{DirStore, MemStore, ResultStore},
};
use hirsel_syntax::{DefPath, ItemPath, PackageId};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct ReadScopes {
    inner: MemStore,
    active: Arc<AtomicUsize>,
    epochs: AtomicUsize,
}
struct ReadScope(Arc<AtomicUsize>);
impl Drop for ReadScope {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ResultStore for ReadScopes {
    fn get(&self, key: &hirsel_engine::store::EntryKey) -> Option<hirsel_engine::store::Bytes> {
        self.inner.get(key)
    }
    fn put(&self, key: &hirsel_engine::store::EntryKey, bytes: &[u8]) {
        self.inner.put(key, bytes);
    }
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        if self.active.fetch_add(1, Ordering::SeqCst) == 0 {
            self.epochs.fetch_add(1, Ordering::SeqCst);
        }
        Some(Box::new(ReadScope(self.active.clone())))
    }
}

#[test]
fn selected_request_keeps_one_read_epoch_and_releases_it_before_retained_outputs() {
    use hirsel_driver::requests::CompileOperation;
    use hirsel_driver::requests::CompileRequest;
    use hirsel_driver::requests::CompilerRequests;
    use hirsel_driver::requests::RequestLimits;
    use hirsel_engine::{
        LoweringPolicy,
        requests::{OutputReadiness, RequestEngine, observe_inputs},
        runtime::{ProcessBudget, QueryRuntime},
    };
    hirsel_engine::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"), "pub fn one()->u8{1u8}\npub fn two()->u8{2u8}\npub fn three()->u8{3u8}\npub fn main()->u8{one()+two()+three()}\n").unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"scope","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let store = Arc::new(ReadScopes::default());
        let config = CompileConfig {
            store: store.clone(),
            jobs: hirsel_intern::jobs::Jobs::ONE,
            hold_bytes: 0,
            ..Default::default()
        };
        let compiler = hirsel_front_phases::COMPILER
            .with_config(config.clone())
            .with_owned_engines();
        let (loaded, inputs) = observe_inputs(
            hirsel_engine::loader_inputs::InputLimits {
                bytes: 1 << 20,
                operations: 128,
            },
            3,
            |inputs| compiler.load_check_package_inputs(&manifest, inputs, false),
        )
        .unwrap();
        drop(loaded.unwrap());
        let mut engine = CompilerRequests::new(
            compiler,
            config,
            ProcessBudget::new(1 << 30),
            RequestLimits {
                runtime_bytes: 256 << 20,
                snapshot_bytes: 64 << 20,
                transport_bytes: 32 << 20,
            },
        );
        let request = CompileRequest {
            path: manifest,
            inputs: inputs.into(),
            operation: CompileOperation::Check {
                lowering: LoweringPolicy::Reachable,
            },
        };
        let mut selected = engine.select(&request).unwrap();
        assert_eq!(store.active.load(Ordering::SeqCst), 1);
        let epochs = store.epochs.load(Ordering::SeqCst);
        let products = {
            let mut runtime = selected.runtime.lock().unwrap();
            let request = runtime.request(&selected.roots);
            runtime.wait(request).unwrap()
        };
        assert_eq!(store.epochs.load(Ordering::SeqCst), epochs);
        let retained = selected.output.ready(&products).unwrap();
        assert_eq!(store.active.load(Ordering::SeqCst), 0);
        let cancelled = engine.select(&request).unwrap();
        assert_eq!(store.active.load(Ordering::SeqCst), 1);
        drop(cancelled);
        assert_eq!(store.active.load(Ordering::SeqCst), 0);
        let mut failed = engine.select(&request).unwrap();
        assert_eq!(store.active.load(Ordering::SeqCst), 1);
        let missing = hirsel_artifact_store::Domain { kind: 0, schema: 1 }
            .reference(b"missing")
            .unwrap();
        assert!(failed.output.failed(missing).is_some());
        assert_eq!(store.active.load(Ordering::SeqCst), 0);
        drop(retained);
    });
}

#[test]
fn directory_request_products_reuse_unchanged_bodies_across_sessions_and_edits() {
    hirsel_engine::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.hir");
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(
            &source,
            "fn stable()->u8{1u8}\npub fn main()->u8{stable()}\n",
        )
        .unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"reuse","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let config = CompileConfig {
            store: Arc::new(DirStore::new(directory.path().join("results"))),
            jobs: hirsel_intern::jobs::Jobs::ONE,
            hold_bytes: 0,
            ..Default::default()
        };
        let path = |name: &str| {
            DefPath(ItemPath {
                package: PackageId {
                    name: "reuse".into(),
                    version: "1".into(),
                },
                modules: vec![],
                name: name.into(),
            })
        };
        let queries = ["stable", "main"]
            .into_iter()
            .flat_map(|name| {
                [QueryKind::Check, QueryKind::Modes]
                    .into_iter()
                    .map(move |kind| (path(name), kind))
            })
            .collect::<Vec<_>>();
        let check = |session: &mut Session| {
            let checked = session.check().unwrap();
            assert!(checked.diagnostics.is_empty());
            assert_eq!(checked.completion, CheckCompletion::Complete);
        };
        let mut seed = Session::open_configured(&manifest, config.clone());
        check(&mut seed);
        assert!(
            seed.recompute_counts(queries.clone())
                .unwrap()
                .iter()
                .all(|(_, _, count)| *count == 1)
        );
        drop(seed);
        let mut fresh = Session::open_configured(&manifest, config);
        check(&mut fresh);
        assert!(
            fresh
                .recompute_counts(queries.clone())
                .unwrap()
                .iter()
                .all(|(_, _, count)| *count == 0)
        );
        std::fs::write(
            &source,
            "fn stable()->u8{1u8}\npub fn main()->u8{stable()+1u8}\n",
        )
        .unwrap();
        check(&mut fresh);
        for (definition, _, count) in fresh.recompute_counts(queries).unwrap() {
            assert_eq!(count, u64::from(definition == path("main")), "{definition}");
        }
    });
}

#[test]
fn unfinished_equal_requests_have_independent_abort_owners_and_completed_graphs_reuse() {
    use hirsel_driver::requests::CompileOperation;
    use hirsel_driver::requests::CompileRequest;
    use hirsel_driver::requests::CompilerRequests;
    use hirsel_driver::requests::RequestLimits;
    use hirsel_engine::{
        LoweringPolicy,
        requests::{OutputReadiness, RequestEngine, observe_inputs},
        runtime::{ProcessBudget, QueryRuntime},
    };
    hirsel_engine::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(
            directory.path().join("main.hir"),
            "pub fn main()->u8{7u8}\n",
        )
        .unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"independent","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let config = CompileConfig {
            store: Arc::new(DirStore::new(directory.path().join("results"))),
            jobs: hirsel_intern::jobs::Jobs::ONE,
            hold_bytes: 0,
            ..Default::default()
        };
        let compiler = hirsel_front_phases::COMPILER
            .with_config(config.clone())
            .with_owned_engines();
        let (loaded, inputs) = observe_inputs(
            hirsel_engine::loader_inputs::InputLimits {
                bytes: 1 << 20,
                operations: 128,
            },
            3,
            |inputs| compiler.load_check_package_inputs(&manifest, inputs, false),
        )
        .unwrap();
        drop(loaded.unwrap());
        let mut engine = CompilerRequests::new(
            compiler,
            config,
            ProcessBudget::new(1 << 30),
            RequestLimits {
                runtime_bytes: 256 << 20,
                snapshot_bytes: 64 << 20,
                transport_bytes: 32 << 20,
            },
        );
        let request = CompileRequest {
            path: manifest,
            inputs: inputs.into(),
            operation: CompileOperation::Check {
                lowering: LoweringPolicy::Reachable,
            },
        };
        let first = engine.select(&request).unwrap();
        {
            let mut runtime = first.runtime.lock().unwrap();
            let demand = runtime.request(&first.roots);
            runtime.wait(demand).unwrap();
        }
        let mut second = engine.select(&request).unwrap();
        assert!(!Arc::ptr_eq(&first.runtime, &second.runtime));
        drop(first);
        let products = {
            let mut runtime = second.runtime.lock().unwrap();
            let demand = runtime.request(&second.roots);
            runtime.wait(demand).unwrap()
        };
        let output = second.output.ready(&products).unwrap();
        let hirsel_driver::requests::CompileResult::Check(checked) = &output.result else {
            panic!()
        };
        assert_eq!(checked.completion, CheckCompletion::Complete);
        assert!(checked.diagnostics.is_empty());
        let third = engine.select(&request).unwrap();
        assert!(Arc::ptr_eq(&second.runtime, &third.runtime));
    });
}

#[test]
fn check_diagnostics_do_not_require_a_valid_run_or_export_entry() {
    use hirsel_driver::requests::CompileOperation;
    use hirsel_driver::requests::CompileResult;
    use hirsel_engine::LoweringPolicy;
    hirsel_engine::with_compiler_stack(|| {
        let tree = tempfile::tempdir().unwrap();
        let source = tree.path().join("main.hir");
        let manifest = tree.path().join("hirsel.json");
        for entry in ["", r#","entry":{"module":[],"name":"main"}"#] {
            std::fs::write(&manifest, format!(r#"{{"schema":1,"package":{{"name":"check_only","version":"1"}},"sources":[{{"path":"main.hir","module":[]}}],"dependencies":[]{entry}}}"#)).unwrap();
            for (text, valid) in [
                ("pub fn value()->u8{7u8}\n", true),
                ("pub fn main()->Missing{missing}\n", false),
            ] {
                std::fs::write(&source, text).unwrap();
                let mut session = Session::open_configured(&manifest, CompileConfig::default());
                for lowering in [LoweringPolicy::Comptime, LoweringPolicy::Reachable] {
                    let output = session
                        .request(CompileOperation::Check { lowering })
                        .unwrap();
                    let CompileResult::Check(checked) = &output.result else {
                        panic!("expected Check")
                    };
                    assert_eq!(checked.completion, CheckCompletion::Complete);
                    assert_eq!(checked.diagnostics.is_empty(), valid, "{text}");
                }
            }
        }
    });
}

#[test]
fn source_operations_share_one_graph_only_for_exact_inputs_and_compiler() {
    use hirsel_driver::requests::*;
    use hirsel_engine::{
        LoweringPolicy,
        requests::OutputReadiness,
        requests::{RequestEngine, observe_inputs},
        runtime::QueryRuntime,
    };
    hirsel_engine::with_compiler_stack(|| {
        for provider in ["none@1", "mem@1", "dir@1"] {
            let tree = tempfile::tempdir().unwrap();
            let source = tree.path().join("main.hir");
            let path = tree.path().join("hirsel.json");
            std::fs::write(&source, "const N:u8=comptime 7u8; pub fn main()->u8{N}\n").unwrap();
            std::fs::write(&path, r#"{"schema":1,"package":{"name":"graph","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
            let config = CompileConfig {
                store: hirsel_engine::store::store_by_name(
                    provider,
                    Some(tree.path().join("results")),
                )
                .unwrap(),
                ..CompileConfig::default()
            };
            let mut requests =
                CompilerRequests::for_host(hirsel_front_phases::COMPILER, config.clone());
            let capture = |requests: &CompilerRequests| {
                let (loaded, inputs) = observe_inputs(
                    requests.input_limits(),
                    requests.capture_attempts(),
                    |inputs| {
                        requests
                            .compiler()
                            .load_check_package_inputs(&path, inputs, false)
                    },
                )
                .unwrap();
                drop(loaded.unwrap());
                CompileInputs::from(inputs)
            };
            let inputs = capture(&requests);
            let mut original = None;
            let mut first_execution = None;
            for operation in [
                CompileOperation::Check {
                    lowering: LoweringPolicy::Comptime,
                },
                CompileOperation::Check {
                    lowering: LoweringPolicy::Reachable,
                },
                CompileOperation::Tir { definition: None },
                CompileOperation::Artifacts { native: false },
                CompileOperation::ExportTir,
            ] {
                let request = CompileRequest {
                    path: path.clone(),
                    inputs: inputs.clone(),
                    operation,
                };
                let mut selected = requests.select(&request).unwrap();
                let runtime = Arc::downgrade(&selected.runtime);
                if let Some(original) = &original {
                    assert!(
                        std::sync::Weak::ptr_eq(original, &runtime),
                        "operation rebuilt the source graph"
                    );
                } else {
                    original = Some(runtime);
                }
                let products = {
                    let mut runtime = selected.runtime.lock().unwrap();
                    let id = runtime.request(&selected.roots);
                    runtime.wait(id).unwrap()
                };
                let output = selected.output.ready(&products).unwrap();
                drop(selected);
                match &output.result {
                    CompileResult::Check(checked) => {
                        assert_eq!(checked.completion, hirsel_engine::CheckCompletion::Complete);
                        assert!(checked.diagnostics.is_empty());
                    }
                    CompileResult::Tir(execution) => first_execution = Some(execution.clone()),
                    CompileResult::Artifacts(artifacts) => {
                        assert!(artifacts["diagnostics"].is_empty());
                        assert!(!artifacts["tir"].is_empty());
                    }
                    CompileResult::ExportTir(text) => assert!(!text.is_empty()),
                    _ => panic!("wrong operation"),
                }
            }
            std::fs::write(&source, "const N:u8=comptime 8u8; pub fn main()->u8{N}\n").unwrap();
            let changed = capture(&requests);
            assert!(inputs != changed);
            let request = CompileRequest {
                path: path.clone(),
                inputs: changed,
                operation: CompileOperation::Tir { definition: None },
            };
            let selected = requests.select(&request).unwrap();
            assert!(!std::sync::Weak::ptr_eq(
                original.as_ref().unwrap(),
                &Arc::downgrade(&selected.runtime)
            ));
            drop(selected);
            let output = requests.execute_request(&request).unwrap();
            let CompileResult::Tir(edited) = &output.result else {
                panic!("expected Tir")
            };
            assert_ne!(first_execution.unwrap().revision, edited.revision);
            let mut other = CompilerRequests::for_host(
                hirsel_front_phases::COMPILER.with_verify(hirsel_engine::Verify::Off),
                CompileConfig {
                    verify: hirsel_engine::Verify::Off,
                    ..config
                },
            );
            let selected = other.select(&request).unwrap();
            assert!(!std::sync::Weak::ptr_eq(
                original.as_ref().unwrap(),
                &Arc::downgrade(&selected.runtime)
            ));
        }
    });
}

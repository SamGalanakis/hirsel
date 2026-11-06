use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::*;
use hirsel_intern::jobs::with_compiler_stack;

use hirsel_query_products::{LoweringPolicy, QueryKind};

use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use std::sync::{Arc, Mutex};

struct Observed {
    inner: Arc<dyn ResultStore>,
    syntax_writes: Mutex<Vec<u32>>,
}
impl Default for Observed {
    fn default() -> Self {
        Self {
            inner: Arc::new(MemStore::default()),
            syntax_writes: Mutex::default(),
        }
    }
}
impl ResultStore for Observed {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        self.inner.admit_product_memory(bytes)
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.inner.retention_scope()
    }

    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        self.inner.capability()
    }

    fn name(&self) -> &'static str {
        "demand-capture"
    }
    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        self.inner.get(key)
    }
    fn put(&self, key: &EntryKey, bytes: &[u8]) -> hirsel_engine_store::WriteOutcome {
        self.inner.put(key, bytes)
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        self.inner.get_content(reference)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        if [0x4601_000a, 0x4600_0013].contains(&domain.kind) {
            self.syntax_writes.lock().unwrap().push(domain.kind);
        }
        self.inner.put_content(domain, bytes)
    }
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("hirsel.json");
    std::fs::write(
        directory.path().join("main.hir"),
        "pub fn main()->u8{1u8}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("unused.hir"),
        "pub fn unused()->u8{true}\n",
    )
    .unwrap();
    std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"demand390","version":"1"},"sources":[{"path":"main.hir","module":[]},{"path":"unused.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
    (directory, manifest)
}
#[test]
fn lower_captures_only_its_demand_and_full_check_still_rejects_unused_body() {
    with_compiler_stack(|| {
        let (_directory, manifest) = fixture();
        let store = Arc::new(Observed::default());
        let compiler = COMPILER
            .with_config(CompileConfig {
                store: store.clone(),
                ..Default::default()
            })
            .with_owned_engines();
        let mut program = compiler.load_program(&manifest).unwrap();
        let definitions = program.engine.front_source_roots();
        let mut unused = program.entry.clone();
        unused.0.name = "unused".into();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
        let weight = MemoryWeight {
            retained: 2 << 20,
            scratch: 8 << 20,
            stack: 0,
        };
        let errors = crate::scheduled_front::errors();
        let front = program
            .engine
            .front_body_queries(
                &definitions,
                weight,
                budget.reserve(8 << 20).unwrap(),
                errors,
            )
            .unwrap();
        assert!(
            !front
                .signatures()
                .header_ready(hirsel_syntax::DefId::intern(&unused))
        );
        let lower = program
            .engine
            .front_lower_queries(
                &definitions,
                front.clone(),
                weight,
                32,
                budget.reserve(8 << 20).unwrap(),
                errors,
            )
            .unwrap();
        let mut runtime = AdmittedRuntime::new(
            hirsel_intern::jobs::Jobs::new(1),
            budget.reserve(512 << 20).unwrap(),
            errors,
        )
        .unwrap();
        runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
        runtime
            .register_local(FRONT_LOWER_KIND, lower.factory())
            .unwrap();
        runtime
            .register_local(FRONT_EVALUATE_KIND, lower.factory())
            .unwrap();
        let request = runtime.request(&[lower.query(&program.entry).unwrap()]);
        let product = runtime.wait(request).unwrap().remove(0);
        assert!(lower.outcome(product).is_some());
        assert_eq!(lower.work(&unused).lowerings, 0);
        for kind in [QueryKind::Resolve, QueryKind::Check, QueryKind::Modes] {
            assert_eq!(front.recompute_count(&unused, kind), 0);
        }
        assert!(
            store.syntax_writes.lock().unwrap().is_empty(),
            "live body demand must not use syntax archives"
        );
        runtime.shutdown();
        let checked = program.check(LoweringPolicy::Comptime).unwrap();
        assert_eq!(checked.completion, CheckCompletion::Complete);
        assert!(
            checked
                .diagnostics
                .iter()
                .any(|d| d.severity == hirsel_diag::Severity::Error)
        );
    });
}
#[test]
fn full_check_agrees_cold_resident_restarted_store_off_and_remote_restore() {
    with_compiler_stack(|| {
        let (_directory, manifest) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let durable = directory.path().join("products");
        let remote = Arc::new(
            hirsel_artifact_store::reapi::DirCas::open(directory.path().join("remote")).unwrap(),
        );
        let stores: Vec<Arc<dyn ResultStore>> = vec![
            Arc::new(MemStore::default()),
            Arc::new(DirStore::new(durable.clone())),
            Arc::new(DirStore::new(durable)),
            Arc::new(NoneStore),
            Arc::new(RemoteStore::new(remote.clone(), remote.clone())),
            Arc::new(RemoteStore::new(remote.clone(), remote)),
        ];
        let mut expected = None;
        for backend in stores {
            let store = Arc::new(Observed {
                inner: backend,
                syntax_writes: Mutex::default(),
            });
            let compiler = COMPILER
                .with_config(CompileConfig {
                    store: store.clone(),
                    ..Default::default()
                })
                .with_owned_engines();
            let mut program = compiler.load_program(&manifest).unwrap();
            for _ in 0..2 {
                let checked = program.check(LoweringPolicy::Comptime).unwrap();
                assert_eq!(checked.completion, CheckCompletion::Complete);
                assert!(!checked.diagnostics.is_empty());
                let verdict: Vec<_> = checked
                    .diagnostics
                    .iter()
                    .map(|d| (d.rule.clone(), d.severity, d.message.clone()))
                    .collect();
                if let Some(expected) = &expected {
                    assert_eq!(&verdict, expected);
                } else {
                    expected = Some(verdict);
                }
            }
            assert!(
                store.syntax_writes.lock().unwrap().is_empty(),
                "Check must use live syntax owners"
            );
        }
    });
}

#[test]
fn native_catalogue_is_lazy_and_store_off_archives_require_export() {
    with_compiler_stack(|| {
        let (_directory, manifest) = fixture();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
        let backend = Arc::new(Observed {
            inner: Arc::new(NoneStore),
            syntax_writes: Mutex::default(),
        });
        let store = Arc::new(RequestStore::new(
            backend,
            budget.reserve(16 << 20).unwrap(),
        ));
        assert_eq!(store.persistence(), Persistence::Disabled);
        let compiler = COMPILER
            .with_config(CompileConfig {
                store,
                ..Default::default()
            })
            .with_owned_engines();
        let mut program = compiler.load_program(&manifest).unwrap();
        let definitions = program.engine.front_source_roots();
        let mut unused = program.entry.clone();
        unused.0.name = "unused".into();
        let weight = MemoryWeight {
            retained: 2 << 20,
            scratch: 8 << 20,
            stack: 0,
        };
        let errors = crate::scheduled_front::errors();
        let front = program
            .engine
            .front_body_queries(
                &definitions,
                weight,
                budget.reserve(8 << 20).unwrap(),
                errors,
            )
            .unwrap();
        let lower = program
            .engine
            .front_lower_queries(
                &definitions,
                front.clone(),
                weight,
                32,
                budget.reserve(8 << 20).unwrap(),
                errors,
            )
            .unwrap();
        let native = program
            .engine
            .front_native_definition_queries(
                &definitions,
                front.clone(),
                lower.clone(),
                weight,
                budget.reserve(8 << 20).unwrap(),
                errors,
            )
            .unwrap();
        assert!(
            !front
                .signatures()
                .header_ready(hirsel_syntax::DefId::intern(&unused))
        );
        let mut runtime = AdmittedRuntime::new(
            hirsel_intern::jobs::Jobs::new(1),
            budget.reserve(512 << 20).unwrap(),
            errors,
        )
        .unwrap();
        runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
        runtime
            .register_local(FRONT_LOWER_KIND, lower.factory())
            .unwrap();
        runtime
            .register_local(FRONT_EVALUATE_KIND, lower.factory())
            .unwrap();
        runtime
            .register(FRONT_NATIVE_DEFINITION_KIND, native.factory())
            .unwrap();
        let request = runtime.request(&[native.query(&program.entry).unwrap()]);
        let product = runtime
            .wait(request)
            .unwrap_or_else(|error| panic!("{error:?}: {:?}", native.failure(error)))
            .remove(0);
        assert!(native.outcome(product.clone()).is_some());
        assert!(product.archive(ArchiveAdmission::Retained).is_err());
        for kind in [QueryKind::Resolve, QueryKind::Check, QueryKind::Modes] {
            assert_eq!(front.recompute_count(&unused, kind), 0);
        }
        let mut scratch = budget.reserve(2 << 20).unwrap();
        let archive = product
            .archive(ArchiveAdmission::Materialize {
                scratch: &mut scratch,
                working_bytes: 2 << 20,
            })
            .unwrap();
        assert_eq!(
            archive.reference(),
            product
                .archive(ArchiveAdmission::Retained)
                .unwrap()
                .reference()
        );
        runtime.shutdown();
    });
}

#[test]
fn native_source_export_with_installed_std_uses_lazy_constant_inventory() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.hir");
        std::fs::write(
            &source,
            r###"pub type AppError = Grant(GrantError) | Pool(CpuPoolError)
pub fn main(rt: Io, args: Args) -> Result[(), AppError] {
  let output = io.stdout(rt) ? Grant
  let pool = match arg.text(args, 0) { Ok("pool") => true, _ => false }
  let width = match arg.text(args, 1) { Ok("4") => 4u32, _ => 1u32 }
  let seed = match arg.text(args, 2) { Ok("21") => 21u64, _ => 0u64 }
  let value = seed * 2u64
  failure.check(value == (if pool && width == 4u32 { 180u64 } else { 42u64 }), Bounds())
  let _ = out.print(output, f"{value}\n")
  Ok(())
}
"###,
        )
        .unwrap();
        let mut session = session::Session::open_configured(
            &source,
            CompileConfig {
                store: store_by_name("none@1", None).unwrap(),
                ..Default::default()
            },
        );
        let output = session
            .request(hirsel_requests::CompileOperation::NativeSources { roots: vec![] })
            .unwrap_or_else(|error| panic!("startup native sources: {error}"));
        assert!(matches!(
            output.result,
            hirsel_requests::CompileResult::NativeSources(_)
        ));
    });
}

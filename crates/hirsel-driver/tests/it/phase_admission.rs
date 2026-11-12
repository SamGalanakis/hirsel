use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::MemStore;
use hirsel_intern::jobs::with_compiler_stack;

use hirsel_query_products::LoweringPolicy;

use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use std::sync::{Arc, Mutex};

struct Fixture {
    program: LoadedProgram,
    runtime: AdmittedRuntime,
    front: Arc<FrontBodyQueries>,
    lower: Arc<FrontLowerQueries>,
    check: Arc<FrontCheckQueries>,
    budget: Arc<hirsel_engine_admission::ProcessBudget>,
    weight: MemoryWeight,
    errors: RuntimeErrors,
    store: Arc<MemStore>,
    events: Arc<Events>,
    _directory: Arc<tempfile::TempDir>,
}
impl Fixture {
    fn new(source: &str) -> Self {
        Self::in_directory(source, Arc::new(tempfile::tempdir().unwrap()))
    }
    fn in_directory(source: &str, directory: Arc<tempfile::TempDir>) -> Self {
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"), source).unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"phase_admission","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let store = Arc::new(MemStore::default());
        let mut program = COMPILER
            .with_config(CompileConfig {
                store: store.clone(),
                hold_bytes: 0,
                ..Default::default()
            })
            .with_owned_engines()
            .load_program(&manifest)
            .unwrap();
        let paths = program.engine.front_source_roots();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 30);
        let weight = MemoryWeight {
            retained: 2 << 20,
            scratch: 8 << 20,
            stack: 0,
        };
        let errors = crate::scheduled_front::errors();
        let mut runtime = AdmittedRuntime::new(budget.reserve(512 << 20).unwrap(), errors).unwrap();
        let front = program
            .engine
            .front_body_queries(&paths, weight, &budget, errors)
            .unwrap();
        let lower = program
            .engine
            .front_lower_queries(&paths, front.clone(), weight, 32, &budget, errors)
            .unwrap();
        let growth = program
            .engine
            .front_growth_queries(&program.entry, front.clone(), weight, &budget, errors)
            .unwrap();
        let check = program
            .engine
            .front_check_queries(
                FrontCheckPlan {
                    files: program
                        .files
                        .iter()
                        .map(|(file, context)| (*file, context.clone(), LoweringPolicy::Reachable))
                        .collect(),
                    front: front.clone(),
                    lower: lower.clone(),
                    growth: growth.clone(),
                    unsafe_ffi: program.unsafe_ffi.clone(),
                    lint_package: None,
                },
                weight,
                &budget,
                errors,
            )
            .unwrap();
        let events = Arc::new(Events::default());
        runtime.observe(events.clone()).unwrap();
        runtime.register(FRONT_BODY_KIND, front.factory()).unwrap();
        runtime
            .register_local(FRONT_LOWER_KIND, lower.factory())
            .unwrap();
        runtime
            .register_local(FRONT_EVALUATE_KIND, lower.factory())
            .unwrap();
        runtime
            .register(FRONT_GROWTH_KIND, growth.factory())
            .unwrap();
        runtime.register(FRONT_CHECK_KIND, check.factory()).unwrap();
        Self {
            program,
            runtime,
            front,
            lower,
            check,
            budget,
            weight,
            errors,
            store,
            events,
            _directory: directory,
        }
    }
    fn path(&self, name: &str) -> hirsel_syntax::DefPath {
        let mut path = self.program.entry.clone();
        path.0.name = name.into();
        path
    }
    fn run(&mut self) -> Arc<FrontRunQueries> {
        self.program
            .engine
            .front_run_queries(
                self.check.clone(),
                self.front.clone(),
                self.lower.clone(),
                self.weight,
                self.budget.reserve(16 << 20).unwrap(),
                self.errors,
            )
            .unwrap()
    }
}
#[test]
fn failed_check_withholds_acceptance_but_independent_lower_still_runs() {
    with_compiler_stack(|| {
        let mut f = Fixture::new("pub fn bad()->u8{true}\npub fn main()->u8{7u8}\n");
        let run = f.run();
        f.runtime.register(FRONT_RUN_KIND, run.factory()).unwrap();
        let request = f.runtime.request(&[run.query(&f.program.entry).unwrap()]);
        let refusal = f.runtime.wait(request).unwrap_err();
        assert!(matches!(
            run.failure(refusal),
            Some(QueryError::Diagnostics(_))
        ));
        let checked = f
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .find_map(|event| match event {
                RuntimeEvent::Settled {
                    query,
                    result: Ok(product),
                } if *query == f.check.query() => Some(product.clone()),
                _ => None,
            })
            .expect("an early semantic failure still joins the complete ordered source Check");
        let output = f.check.outcome(checked.clone()).unwrap();
        assert_eq!(f.check.accepted(checked), Some(false));
        assert!(!output.checked.diagnostics.is_empty());
        assert!(
            output.lowered.is_empty(),
            "a failed file publishes no executable source prefix"
        );
        assert_eq!(
            f.lower.work(&f.path("bad")).lowerings,
            0,
            "a rejected unit cannot lower"
        );
        assert_eq!(
            f.lower.work(&f.program.entry).lowerings,
            1,
            "an independent ready unit can lower"
        );
        f.runtime.shutdown();
    });
}

#[derive(Default)]
struct Events(Mutex<Vec<RuntimeEvent>>);
impl RuntimeObserver for Events {
    fn record(&self, event: RuntimeEvent) {
        self.0.lock().unwrap().push(event);
    }
}

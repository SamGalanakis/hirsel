use hirsel_driver::*;
use hirsel_engine::{store::MemStore, *};
use hirsel_engine_runtime::*;

use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use std::{
    sync::{Arc, Mutex, mpsc},
    task::Poll,
    time::Duration,
};

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
    _directory: Arc<tempfile::TempDir>,
}
impl Fixture {
    fn new(source: &str, jobs: usize) -> Self {
        Self::in_directory(source, jobs, Arc::new(tempfile::tempdir().unwrap()))
    }
    fn in_directory(source: &str, jobs: usize, directory: Arc<tempfile::TempDir>) -> Self {
        Self::with_completion_order(source, jobs, directory, None).0
    }
    fn with_completion_order(
        source: &str,
        jobs: usize,
        directory: Arc<tempfile::TempDir>,
        order: Option<[&str; 2]>,
    ) -> (Self, Arc<CompletionOrder>) {
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
        let mut runtime = AdmittedRuntime::new(
            hirsel_intern::jobs::Jobs::new(jobs),
            budget.reserve(512 << 20).unwrap(),
            errors,
        )
        .unwrap();
        let front = program
            .engine
            .front_body_queries(&paths, weight, budget.reserve(16 << 20).unwrap(), errors)
            .unwrap();
        let lower = program
            .engine
            .front_lower_queries(
                &paths,
                front.clone(),
                weight,
                32,
                budget.reserve(32 << 20).unwrap(),
                errors,
            )
            .unwrap();
        let growth = program
            .engine
            .front_growth_queries(
                &program.entry,
                front.clone(),
                weight,
                budget.reserve(8 << 20).unwrap(),
                errors,
            )
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
                budget.reserve(16 << 20).unwrap(),
                errors,
            )
            .unwrap();
        let paths = order.map(|names| {
            names.map(|name| {
                let mut path = program.entry.clone();
                path.0.name = name.into();
                front.query(&path).unwrap()
            })
        });
        let (release, wait) = mpsc::channel();
        let completion = Arc::new(CompletionOrder {
            order: paths,
            release,
            events: Mutex::new(Vec::new()),
        });
        runtime.observe(completion.clone()).unwrap();
        runtime
            .register(
                FRONT_BODY_KIND,
                Arc::new(CompletionFactory {
                    inner: front.factory(),
                    delayed: paths.map(|ids| ids[1]),
                    wait: Arc::new(Mutex::new(Some(wait))),
                }),
            )
            .unwrap();
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
        (
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
                _directory: directory,
            },
            completion,
        )
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
// A dependency that is absent from `ready` never finishes. This observes the
// real continuation's scheduler reads without a time-dependent slow fixture.
#[derive(Default)]
struct PendingContext {
    ready: Vec<(QueryId, Result<ProductRef, hirsel_engine::store::Reference>)>,
    reads: Vec<QueryId>,
    released: Vec<QueryId>,
}
impl QueryContext for PendingContext {
    fn scratch_bytes(&self) -> u64 {
        8 << 20
    }
    fn read(&mut self, id: QueryId) -> Poll<Result<ProductRef, hirsel_engine::store::Reference>> {
        self.reads.push(id);
        self.ready
            .iter()
            .find(|(query, _)| *query == id)
            .map(|(_, result)| result.clone())
            .map_or(Poll::Pending, Poll::Ready)
    }
    fn release_body(&mut self, id: QueryId) {
        self.released.push(id);
    }
}

#[test]
fn ready_lower_is_demanded_before_an_unrelated_check_finishes() {
    with_compiler_stack(|| {
        let mut f = Fixture::new("pub fn main()->u8{7u8}\nfn unrelated()->u8{9u8}\n", 1);
        let first = f.front.query(&f.program.entry).unwrap();
        let request = f.runtime.request(&[first]);
        let product = f.runtime.wait(request).unwrap()[0].clone();
        let mut cx = PendingContext::default();
        cx.ready.push((first, Ok(product)));
        let mut task = f.check.factory().create(f.check.query()).unwrap();
        assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(_)));
        if let Some(child) = cx
            .reads
            .iter()
            .find(|id| id.kind == FRONT_CHECK_KIND)
            .copied()
        {
            task = f.check.factory().create(child).unwrap();
            cx.reads.clear();
            assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(_)));
        }
        assert!(
            cx.reads.contains(&f.lower.query(&f.program.entry).unwrap()),
            "ready Lower must start while the unrelated Check remains pending"
        );
        assert!(
            !cx.reads
                .contains(&f.front.query(&f.path("unrelated")).unwrap())
        );
        assert!(
            cx.released.contains(&first),
            "the admitted window releases consumed Front body leases"
        );
        f.runtime.shutdown();
    });
}

#[test]
fn run_demands_semantic_dependencies_before_final_source_acceptance() {
    with_compiler_stack(|| {
        let mut f = Fixture::new("pub fn main()->u8{7u8}\nfn unrelated()->u8{9u8}\n", 1);
        let run = f.run();
        let mut task = run
            .factory()
            .create(run.query(&f.program.entry).unwrap())
            .unwrap();
        let mut cx = PendingContext::default();
        assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(_)));
        assert!(
            cx.reads.iter().any(|id| *id != f.check.query()),
            "a source-wide Check cannot bar semantic work"
        );
        if let Some(child) = cx
            .reads
            .iter()
            .find(|id| id.kind == FRONT_RUN_KIND)
            .copied()
        {
            task = run.factory().create(child).unwrap();
            cx.reads.clear();
            assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(_)));
            assert!(!cx.reads.contains(&f.check.query()));
            assert!(cx.reads.iter().any(|id| id.kind == FRONT_RUN_KIND));
        }
        f.runtime.shutdown();
    });
}

#[test]
fn native_root_demands_a_source_unit_before_run_acceptance() {
    with_compiler_stack(|| {
        let mut f = Fixture::new("pub fn main()->u8{7u8}\nfn unrelated()->u8{9u8}\n", 1);
        let run = f.run();
        let paths = f.program.engine.front_source_roots();
        let definitions = f
            .program
            .engine
            .front_native_definition_queries(
                &paths,
                f.front.clone(),
                f.lower.clone(),
                f.weight,
                f.budget.reserve(16 << 20).unwrap(),
                f.errors,
            )
            .unwrap();
        let native = f
            .program
            .engine
            .front_native_queries(
                FrontNativePlan {
                    entry: f.program.entry.clone(),
                    roots: vec![f.program.entry.clone()],
                    run,
                    definitions,
                },
                f.weight,
                f.budget.reserve(16 << 20).unwrap(),
                f.errors,
            )
            .unwrap();
        let mut task = native.factory().create(native.query()).unwrap();
        let mut cx = PendingContext::default();
        assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(_)));
        if let Some(child) = cx
            .reads
            .iter()
            .find(|id| id.kind == FRONT_NATIVE_ROOT_KIND)
            .copied()
        {
            task = native.factory().create(child).unwrap();
            cx.reads.clear();
            assert!(matches!(task.poll(&mut cx), QueryPoll::Waiting(_)));
        }
        assert!(
            cx.reads
                .contains(&native.definition_query(&f.program.entry).unwrap()),
            "native work starts before the whole Run is accepted"
        );
        let mut definition = native
            .definition_factory()
            .create(native.definition_query(&f.program.entry).unwrap())
            .unwrap();
        let mut cx = PendingContext::default();
        assert!(matches!(definition.poll(&mut cx), QueryPoll::Waiting(_)));
        assert!(
            cx.reads.contains(&f.front.query(&f.program.entry).unwrap()),
            "the selected catalogue admits its local dependencies before the whole Run"
        );
        f.runtime.shutdown();
    });
}

#[test]
fn failed_check_withholds_acceptance_but_independent_lower_still_runs() {
    use hirsel_engine::store::ResultStore;
    with_compiler_stack(|| {
        for jobs in [1, 2, 8] {
            let mut f = Fixture::new("pub fn bad()->u8{true}\npub fn main()->u8{7u8}\n", jobs);
            let run = f.run();
            f.runtime.register(FRONT_RUN_KIND, run.factory()).unwrap();
            let request = f.runtime.request(&[run.query(&f.program.entry).unwrap()]);
            let refusal = f.runtime.wait(request).unwrap_err();
            assert!(matches!(
                run.failure(refusal),
                Some(QueryError::Diagnostics(_))
            ));
            assert!(
                f.store.lookup_product(f.check.query().address).is_some(),
                "an early semantic failure still joins the complete ordered source Check"
            );
            let request = f.runtime.request(&[f.check.query()]);
            let products = f.runtime.wait(request).unwrap();
            let output = f.check.outcome(products[0].clone()).unwrap();
            assert_eq!(f.check.accepted(products[0].clone()), Some(false));
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
        }
    });
}

// The factory seam already used by the cancellation law holds one producer
// before its first poll. Only the other producer's Settled event opens it.
// Zed's order.rs:72-125 supplies the permutation technique; the source and
// the request roots stay fixed here, and only physical completion is permuted.
struct CompletionOrder {
    order: Option<[QueryId; 2]>,
    release: mpsc::Sender<()>,
    events: Mutex<Vec<RuntimeEvent>>,
}
impl RuntimeObserver for CompletionOrder {
    fn record(&self, event: RuntimeEvent) {
        if let RuntimeEvent::Settled { query, .. } = &event
            && self.order.is_some_and(|ids| ids[0] == *query)
        {
            self.release.send(()).unwrap();
        }
        self.events.lock().unwrap().push(event);
    }
}
struct CompletionFactory {
    inner: Arc<dyn QueryFactory>,
    delayed: Option<QueryId>,
    wait: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
}
impl QueryFactory for CompletionFactory {
    fn evict(&self, id: QueryId, product: &ProductRef, scratch: u64) -> bool {
        self.inner.evict(id, product, scratch)
    }
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, hirsel_engine::store::Reference> {
        Ok(Box::new(CompletionTask {
            inner: self.inner.create(id)?,
            wait: (self.delayed == Some(id)).then(|| self.wait.clone()),
        }))
    }
}
struct CompletionTask {
    inner: Box<dyn QueryTask>,
    wait: Option<Arc<Mutex<Option<mpsc::Receiver<()>>>>>,
}
impl QueryTask for CompletionTask {
    fn id(&self) -> QueryId {
        self.inner.id()
    }
    fn weight(&self) -> MemoryWeight {
        self.inner.weight()
    }
    fn continuation_bytes(&self) -> u64 {
        self.inner.continuation_bytes()
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(wait) = self.wait.take() {
            wait.lock()
                .unwrap()
                .take()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("independent producer did not settle before the completion gate deadline");
        }
        self.inner.poll(cx)
    }
}

#[test]
fn diagnostics_and_source_prefix_archives_are_identical_at_jobs_one_two_and_eight() {
    with_compiler_stack(|| {
        for source in [
            "const BAD:u8=comptime(1u8/0u8);\nconst LATER:u8=comptime 2u8;\npub fn main()->u8{LATER}\n",
            "pub fn early()->u8{true}\npub fn main()->u8{7u8}\npub fn later()->u64{false}\n",
            "pub fn early()->u8{3u8}\npub fn main()->u8{7u8}\npub fn later()->u8{9u8}\n",
        ] {
            let names = if source.starts_with("const") {
                ["BAD", "LATER"]
            } else {
                ["early", "later"]
            };
            let mut expected = None;
            let directory = Arc::new(tempfile::tempdir().unwrap());
            for (jobs, reverse) in [
                (1, None),
                (2, Some(false)),
                (2, Some(true)),
                (8, Some(false)),
                (8, Some(true)),
            ] {
                let order =
                    reverse.map(|reverse| if reverse { [names[1], names[0]] } else { names });
                let (mut f, completion) =
                    Fixture::with_completion_order(source, jobs, directory.clone(), order);
                let roots = names.map(|name| f.front.query(&f.path(name)).unwrap());
                // Both independent producers are demanded in the same order,
                // including when source admission is waiting on the first one.
                let producers = f.runtime.request(&roots);
                let request = f.runtime.request(&[f.check.query()]);
                let product = f.runtime.wait(request).unwrap()[0].clone();
                let producer_result = f.runtime.wait(producers);
                let output = f.check.outcome(product.clone()).unwrap();
                let archive = |product: &ProductRef| {
                    let mut scratch = f.budget.reserve(f.weight.scratch).unwrap();
                    product
                        .archive(ArchiveAdmission::Materialize {
                            scratch: &mut scratch,
                            working_bytes: f.weight.scratch,
                        })
                        .unwrap()
                        .bytes()
                        .to_vec()
                };
                let actual = (
                    (
                        product.address,
                        product.action,
                        product.domain(),
                        archive(&product),
                    ),
                    output
                        .checked
                        .diagnostics
                        .iter()
                        .flat_map(|diagnostic| {
                            diagnostic
                                .render_human(&output.checked.sources)
                                .into_bytes()
                        })
                        .collect::<Vec<_>>(),
                    f.check.accepted(product.clone()),
                    output
                        .front
                        .iter()
                        .map(|(path, observation)| {
                            (
                                path.clone(),
                                observation.address,
                                observation.action,
                                observation.domain(),
                            )
                        })
                        .collect::<Vec<_>>(),
                    output
                        .lowered
                        .iter()
                        .map(|(path, product)| {
                            (
                                path.clone(),
                                product.address,
                                product.action,
                                product.domain(),
                                archive(product),
                            )
                        })
                        .collect::<Vec<_>>(),
                    producer_result
                        .map(|products| products.iter().map(archive).collect::<Vec<_>>()),
                );
                if source.contains("true") || source.starts_with("const") {
                    assert_eq!(actual.2, Some(false));
                    assert!(!actual.1.is_empty());
                    assert!(
                        actual.4.is_empty(),
                        "failed file accepts no executable prefix"
                    );
                } else {
                    assert_eq!(actual.2, Some(true));
                    assert!(actual.1.is_empty());
                    assert_eq!(actual.3.len(), 3);
                    assert_eq!(actual.4.len(), 3);
                }
                let independent = if source.starts_with("const") {
                    f.path("LATER")
                } else {
                    f.program.entry.clone()
                };
                assert_eq!(f.lower.work(&independent).lowerings, 1);
                f.runtime.shutdown();
                let stats = f.runtime.stats();
                assert_eq!(stats.active, 0);
                assert_eq!(stats.charged_bytes, 0);
                assert_eq!(stats.claims, stats.settled, "all admitted producers drain");
                let events = completion.events.lock().unwrap();
                let settled = events
                    .iter()
                    .filter_map(|event| match event {
                        RuntimeEvent::Settled { query, .. } if roots.contains(query) => {
                            Some(*query)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if let Some(order) = completion.order {
                    assert_eq!(settled, order, "forced completion order at jobs={jobs}");
                }
                // Poll counts vary. The dependency graph and each producer's
                // first-read order must not, nor may any worker claim leak.
                let mut reads = std::collections::BTreeMap::new();
                let mut claims = std::collections::BTreeMap::new();
                let mut running = std::collections::BTreeMap::new();
                for event in events.iter() {
                    match event {
                        RuntimeEvent::Read { query, dependency } => {
                            let row = reads
                                .entry((query.kind, query.address.0))
                                .or_insert_with(Vec::new);
                            if !row.contains(dependency) {
                                row.push(*dependency);
                            }
                        }
                        RuntimeEvent::Claim(query) => {
                            *claims.entry((query.kind, query.address.0)).or_insert(0i32) += 1;
                        }
                        RuntimeEvent::Settled { query, .. } => {
                            *claims.entry((query.kind, query.address.0)).or_insert(0i32) -= 1;
                        }
                        RuntimeEvent::Admit { query, .. } => {
                            *running.entry((query.kind, query.address.0)).or_insert(0i32) += 1;
                        }
                        RuntimeEvent::Release { query, .. } => {
                            *running.entry((query.kind, query.address.0)).or_insert(0i32) -= 1;
                        }
                        _ => {}
                    }
                }
                assert!(
                    claims.values().all(|count| *count == 0),
                    "every producer claim settles"
                );
                assert!(
                    running.values().all(|count| *count == 0),
                    "every worker admission is released"
                );
                let actual = (actual, reads, claims, stats.active, stats.charged_bytes);
                if let Some(expected) = &expected {
                    assert_eq!(&actual, expected, "jobs={jobs}, reverse={reverse:?}");
                } else {
                    expected = Some(actual);
                }
            }
        }
    });
}

struct PausingFactory {
    inner: Arc<dyn QueryFactory>,
    root: QueryId,
    entered: std::sync::mpsc::Sender<bool>,
    release: Arc<std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>>,
}
struct PausingTask {
    inner: Box<dyn QueryTask>,
    root: QueryId,
    entered: std::sync::mpsc::Sender<bool>,
    release: Arc<std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>>,
}
impl QueryFactory for PausingFactory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, hirsel_engine::store::Reference> {
        Ok(Box::new(PausingTask {
            inner: self.inner.create(id)?,
            root: self.root,
            entered: self.entered.clone(),
            release: self.release.clone(),
        }))
    }
}
struct SpyContext<'a> {
    inner: &'a mut dyn QueryContext,
    admission: bool,
    lower: bool,
}
impl QueryContext for SpyContext<'_> {
    fn scratch_bytes(&self) -> u64 {
        self.inner.scratch_bytes()
    }
    fn workers(&self) -> usize {
        self.inner.workers()
    }
    fn release_body(&mut self, id: QueryId) {
        self.inner.release_body(id);
    }
    fn consume(&mut self, id: QueryId) {
        self.inner.consume(id);
    }
    fn evict_body(&mut self) -> bool {
        self.inner.evict_body()
    }
    fn poll_evict_body(&mut self) -> Poll<bool> {
        self.inner.poll_evict_body()
    }
    fn cycle_members(&self, id: QueryId, limit: usize) -> Option<Vec<QueryId>> {
        self.inner.cycle_members(id, limit)
    }
    fn poll_publication(&mut self) -> Poll<Result<(), hirsel_engine::store::Reference>> {
        self.inner.poll_publication()
    }
    fn read(&mut self, id: QueryId) -> Poll<Result<ProductRef, hirsel_engine::store::Reference>> {
        self.admission |= id.kind == FRONT_CHECK_KIND;
        self.lower |= id.kind == FRONT_LOWER_KIND;
        self.inner.read(id)
    }
}
impl QueryTask for PausingTask {
    fn id(&self) -> QueryId {
        self.inner.id()
    }
    fn weight(&self) -> MemoryWeight {
        self.inner.weight()
    }
    fn continuation_bytes(&self) -> u64 {
        self.inner.continuation_bytes()
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        let mut spy = SpyContext {
            inner: cx,
            admission: false,
            lower: false,
        };
        let result = self.inner.poll(&mut spy);
        if self.id() != self.root || !spy.admission {
            let release = self.release.lock().unwrap().take();
            if let Some(release) = release {
                let _ = self.entered.send(spy.lower);
                let _ = release.recv();
            }
        }
        result
    }
}

#[test]
fn cancellation_in_an_admitted_body_window_releases_claims_and_publishes_no_check() {
    use hirsel_engine::store::ResultStore;
    with_compiler_stack(|| {
        let mut f = Fixture::new("pub fn main()->u8{7u8}\npub fn unrelated()->u8{9u8}\n", 2);
        let first = f.front.query(&f.program.entry).unwrap();
        let request = f.runtime.request(&[first]);
        f.runtime.wait(request).unwrap();
        // Recreate only scheduler ownership; the immutable Front result stays
        // settled in its producer, as it does in resident/restarted requests.
        f.runtime.shutdown();
        let mut runtime = AdmittedRuntime::new(
            hirsel_intern::jobs::Jobs::new(2),
            f.budget.reserve(512 << 20).unwrap(),
            f.errors,
        )
        .unwrap();
        runtime
            .register(FRONT_BODY_KIND, f.front.factory())
            .unwrap();
        runtime
            .register_local(FRONT_LOWER_KIND, f.lower.factory())
            .unwrap();
        runtime
            .register_local(FRONT_EVALUATE_KIND, f.lower.factory())
            .unwrap();
        // The cancellation is at the independent admission boundary. Growth
        // need not complete to make the admitted body/window observable.
        let (entered, rx) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        runtime
            .register(
                FRONT_CHECK_KIND,
                Arc::new(PausingFactory {
                    inner: f.check.factory(),
                    root: f.check.query(),
                    entered,
                    release: Arc::new(std::sync::Mutex::new(Some(wait))),
                }),
            )
            .unwrap();
        let request = runtime.request(&[first]);
        runtime.wait(request).unwrap();
        let request = runtime.request(&[f.check.query()]);
        let lower_started = rx.recv().unwrap();
        runtime.cancel(request);
        release.send(()).unwrap();
        assert_eq!(runtime.wait(request), Err(f.errors.cancelled));
        runtime.shutdown();
        assert_eq!(runtime.stats().active, 0);
        assert_eq!(
            runtime.stats().charged_bytes,
            0,
            "all scheduler/body claims are released after home-worker destruction"
        );
        assert!(
            f.store.lookup_product(f.check.query().address).is_none(),
            "cancelled Check publishes no source prefix"
        );
        assert!(
            lower_started,
            "the cancellation point follows dependency-ready Lower admission"
        );
    });
}

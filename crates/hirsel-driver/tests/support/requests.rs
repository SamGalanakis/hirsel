//! Request executor fixtures for tests that start with an indexed engine.
use hirsel_engine::requests::source_graph::InstalledSource;
use hirsel_engine::requests::source_graph::*;
use hirsel_engine::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store::*;

use hirsel_query_products::reads::Reads;
use hirsel_query_products::{LoweringPolicy, QueryKind};

use hirsel_intern::jobs::Jobs;
use std::{collections::BTreeMap, sync::Arc};

pub struct Graph {
    pub installed: InstalledSource,
    pub runtime: AdmittedRuntime,
    pub check: Arc<FrontCheckQueries>,
    _store: Arc<RequestStore>,
    operation: Option<StoreOperation>,
    failure_sources: Option<hirsel_span::SourceMap>,
}
impl Graph {
    pub fn new(engine: &mut QueryEngine, jobs: Jobs) -> Self {
        Self::try_new(engine, jobs).unwrap()
    }
    pub fn try_new(engine: &mut QueryEngine, jobs: Jobs) -> Result<Self, ToolError> {
        let (budget, cap) = host_budget();
        let limits = host_limits(cap, jobs.get());
        let mut loading = LoadingAdmission::reserve(budget, limits, 0).unwrap();
        let store = Arc::new(
            RequestStore::with_scratch_and_verifier_transport(
                engine
                    .product_store()
                    .unwrap_or_else(|| Arc::new(NoneStore)),
                loading.take_transport(),
                &std::env::temp_dir(),
                limits.runtime_bytes + limits.snapshot_bytes + limits.transport_bytes,
            )
            .unwrap(),
        );
        let operation = store.operation_scope();
        let previous = engine
            .product_store()
            .unwrap_or_else(|| Arc::new(NoneStore));
        let owned = std::mem::replace(
            engine,
            QueryEngine::new(default_environment(), Default::default()),
        );
        *engine = owned.with_result_store(store.clone());
        let definitions = engine.indexed_definitions();
        let mut files = BTreeMap::new();
        for path in &definitions {
            let syntax = engine.parse_definition(path).unwrap().value;
            let file = engine.sources.resolve(syntax.item.name_span).file;
            files.insert(
                file,
                FileContext {
                    package: path.0.package.clone(),
                    modules: path.0.modules.clone(),
                },
            );
        }
        let files = files
            .into_iter()
            .map(|(file, context)| (file, context, LoweringPolicy::Comptime))
            .collect::<Vec<_>>();
        let selection = SourceSelection {
            input_bytes: source_bytes(&engine.sources, &files),
            files,
            entry: definitions
                .first()
                .cloned()
                .unwrap_or_else(|| entry_path(&default_context())),
            lint_package: None,
            unsafe_ffi: vec![],
        };
        let built: Result<_, ToolError> = (|| {
            let mut installed = install(
                engine,
                &selection,
                SourceShape {
                    storage: true,
                    ..Default::default()
                },
                jobs,
                limits,
                &BudgetHost(budget),
                loading,
                &*store,
                None,
                None,
            )?;
            let check = installed.check(engine, &selection)?;
            Ok((installed, check))
        })();
        let (installed, check) = match built {
            Ok(built) => built,
            Err(error) => {
                drop(engine.release_private_inference_admission());
                let owned = std::mem::replace(
                    engine,
                    QueryEngine::new(default_environment(), Default::default()),
                );
                *engine = owned.with_result_store(previous);
                return Err(error);
            }
        };
        let allowance = engine.release_private_inference_admission().unwrap();
        let mut runtime = AdmittedRuntime::new(jobs, allowance, installed.errors).unwrap();
        installed.register(&mut runtime, &check).unwrap();
        let owned = std::mem::replace(
            engine,
            QueryEngine::new(default_environment(), Default::default()),
        );
        *engine = owned.with_result_store(previous);
        Ok(Self {
            installed,
            runtime,
            check,
            _store: store,
            operation: Some(operation),
            failure_sources: None,
        })
    }
    pub fn finish(&mut self) {
        self.runtime.shutdown();
        self.operation.take().unwrap().finish().unwrap();
    }
    pub fn body(&mut self, path: &DefPath) -> ModeResult {
        let query = self.installed.front.body_query(path).unwrap();
        let request = self.runtime.request(&[query]);
        let result = self
            .runtime
            .wait(request)
            .map(|mut products| products.remove(0));
        self.installed
            .front
            .outcome(result)
            .expect("body request has an outcome")
            .outcome
    }
    pub fn bodies(&mut self, paths: &[DefPath]) -> Vec<ModeResult> {
        let requests: Vec<_> = paths
            .iter()
            .map(|path| {
                let query = self.installed.front.body_query(path).unwrap();
                self.runtime.request(&[query])
            })
            .collect();
        requests
            .into_iter()
            .map(|request| {
                let result = self
                    .runtime
                    .wait(request)
                    .map(|mut products| products.remove(0));
                self.installed
                    .front
                    .outcome(result)
                    .expect("body request has an outcome")
                    .outcome
            })
            .collect()
    }
    pub fn publications(&mut self, paths: &[DefPath]) -> Vec<PublishedResult> {
        paths
            .iter()
            .map(|path| {
                let query = self.installed.front.query(path).unwrap();
                let request = self.runtime.request(&[query]);
                let selected = self
                    .runtime
                    .wait(request)
                    .map(|mut outputs| outputs.remove(0));
                self.installed
                    .front
                    .publication(selected)
                    .expect("front request has compact publication")
            })
            .collect()
    }
    pub fn lower(
        &mut self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<FrontLowerOutput, QueryError> {
        let query = self.installed.lower.query(path).ok_or_else(|| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "Lower request selection".into(),
                message: format!("no Lower producer for {path}"),
            })
        })?;
        let request = self.runtime.request(&[query]);
        let result = self.runtime.wait(request);
        self.installed.lower.commit_namespace(engine)?;
        match result {
            Ok(mut products) => self
                .installed
                .lower
                .outcome(products.remove(0))
                .ok_or_else(|| self.runtime_failure(self.installed.errors.protocol)),
            Err(failure) => {
                self.failure_sources = self.installed.lower.failure_sources(failure);
                Err(self.failure(failure))
            }
        }
    }
    pub fn constant(
        &mut self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<hirsel_tir::Materialized, QueryError> {
        let query = self.installed.lower.evaluate_query(path).ok_or_else(|| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "constant request selection".into(),
                message: format!("no Evaluate producer for {path}"),
            })
        })?;
        let request = self.runtime.request(&[query]);
        let result = self.runtime.wait(request);
        self.installed.lower.commit_namespace(engine)?;
        match result {
            Ok(_) => self
                .installed
                .lower
                .settled_constant(path, &[])
                .map(|value| (*value).clone()),
            Err(failure) => Err(self.failure(failure)),
        }
    }
    fn failure(&self, reference: Reference) -> QueryError {
        self.installed
            .lower
            .failure(reference)
            .or_else(|| {
                self.installed
                    .front
                    .outcome(Err(reference))
                    .and_then(|outcome| outcome.outcome.result.err())
            })
            .unwrap_or_else(|| self.runtime_failure(reference))
    }
    fn runtime_failure(&self, reference: Reference) -> QueryError {
        let errors = self.installed.errors;
        let message = if reference == errors.resource {
            "compiler query allowance exhausted"
        } else if reference == errors.cycle {
            "compiler dependency cycle"
        } else if reference == errors.cancelled {
            "compiler query cancelled"
        } else if reference == errors.panicked {
            "compiler query producer panicked"
        } else {
            "compiler query protocol failure"
        };
        QueryError::Operational(hirsel_types::OperationalFailure {
            operation: "request completion".into(),
            message: message.into(),
        })
    }
}
impl Drop for Graph {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}

pub fn front(engine: &mut QueryEngine, paths: Vec<DefPath>, jobs: Jobs) -> Vec<ModeResult> {
    let mut graph = Graph::new(engine, jobs);
    let outputs = graph.bodies(&paths);
    graph.finish();
    outputs
}
pub fn publications(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
    retention: hirsel_engine::memo::Retention,
) -> Vec<PublishedResult> {
    engine.set_retention(retention);
    let mut graph = Graph::new(engine, jobs);
    let outputs = graph.publications(&paths);
    graph.finish();
    outputs
}

#[derive(Debug)]
pub struct LowerOutcome<T> {
    pub definition: DefPath,
    pub result: Result<QueryOutput<T>, QueryError>,
    pub phase: Option<QueryKind>,
    pub reads: Option<Arc<Reads>>,
    pub depth: Option<hirsel_query_products::depth::DepthEvidence<hirsel_span::Span>>,
}
#[derive(Debug)]
pub struct ConstantOutcome {
    pub definition: DefPath,
    pub result: Result<hirsel_tir::Materialized, QueryError>,
    pub phase: Option<QueryKind>,
    pub reads: Option<Arc<Reads>>,
}
pub fn constants(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
) -> Vec<Result<hirsel_tir::Materialized, QueryError>> {
    let mut graph = Graph::new(engine, jobs);
    let outputs = paths
        .iter()
        .map(|path| graph.constant(engine, path))
        .collect();
    graph.finish();
    outputs
}
pub fn constant_outcomes(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
) -> Vec<ConstantOutcome> {
    let mut graph = Graph::new(engine, jobs);
    let outputs = paths
        .into_iter()
        .map(|definition| {
            let mut result = graph.constant(engine, &definition);
            let phase = graph
                .installed
                .lower
                .settled_phase(&definition, QueryKind::Evaluate);
            let reads = graph
                .installed
                .lower
                .settled_reads(&definition, QueryKind::Evaluate);
            if reads.is_none() {
                result = Err(missing_reads(&definition, QueryKind::Evaluate));
            }
            ConstantOutcome {
                definition,
                result,
                phase,
                reads,
            }
        })
        .collect();
    graph.finish();
    outputs
}
pub fn lowers(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
) -> Vec<LowerOutcome<hirsel_tir::DefinitionRef>> {
    lower_requests(engine, paths, jobs, false)
}
pub fn lower_prefix(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
) -> Vec<LowerOutcome<hirsel_tir::DefinitionRef>> {
    lower_requests(engine, paths, jobs, true)
}
fn lower_requests(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
    prefix: bool,
) -> Vec<LowerOutcome<hirsel_tir::DefinitionRef>> {
    let mut graph = Graph::new(engine, jobs);
    let mut outcomes = Vec::new();
    for definition in paths {
        let (result, reads, depth, phase) = match graph.lower(engine, &definition) {
            Ok(output) => (
                Ok(output.output),
                Some(Arc::new(output.reads)),
                Some(output.depth),
                Some(QueryKind::Lower),
            ),
            Err(error) => {
                if let Some(sources) = graph.failure_sources.take() {
                    engine.sources = sources;
                }
                let phase = graph
                    .installed
                    .lower
                    .settled_phase(&definition, QueryKind::Lower);
                let reads = graph
                    .installed
                    .lower
                    .settled_reads(&definition, QueryKind::Lower);
                let error = if reads.is_some() {
                    error
                } else {
                    missing_reads(&definition, QueryKind::Lower)
                };
                (Err(error), reads, None, phase)
            }
        };
        let failed = result.is_err();
        outcomes.push(LowerOutcome {
            definition,
            result,
            reads,
            depth,
            phase,
        });
        if prefix && failed {
            break;
        }
    }
    graph.finish();
    outcomes
}
fn missing_reads(definition: &DefPath, phase: QueryKind) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "settled query evidence".into(),
        message: format!("missing {phase:?} read evidence for {definition}"),
    })
}

pub fn checked_prefix(
    engine: &mut QueryEngine,
    paths: Vec<DefPath>,
    jobs: Jobs,
) -> Vec<LowerOutcome<()>> {
    lower_prefix(engine, paths, jobs)
        .into_iter()
        .map(|outcome| LowerOutcome {
            definition: outcome.definition,
            result: outcome.result.map(|output| QueryOutput {
                value: (),
                diagnostics: output.diagnostics,
                dependencies: output.dependencies,
            }),
            reads: outcome.reads,
            depth: outcome.depth,
            phase: outcome.phase,
        })
        .collect()
}

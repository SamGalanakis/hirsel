//! Explicit host policy and one process allowance for compiler request graphs.
use super::*;
use hirsel_engine::CompileConfig;
use hirsel_engine::Compiler;
use hirsel_engine::loader_inputs::InputLimits;

use std::sync::Arc;

pub use hirsel_engine::requests::source_graph::RequestLimits;
pub(crate) use hirsel_engine::requests::source_graph::host_budget;
use hirsel_engine::requests::source_graph::host_limits;
/// Concrete selector policy shared by fresh and resident hosts. It retains
/// no LoadedProgram or checked-output cache. Selected immutable graphs own
/// their products and reservations independently of this constructor's lifetime.
pub struct CompilerRequests {
    compiler: Compiler,
    invocation_store: Option<Arc<dyn hirsel_engine::store::ResultStore>>,
    ledger: Arc<super::inspection::InspectionLedger>,
    pub(super) graphs: super::host_selector::GraphSelections,
    config: CompileConfig,
    budget: Arc<hirsel_engine_admission::ProcessBudget>,
    limits: RequestLimits,
    /// Accepted front facts and Lower outcomes carried from one source
    /// revision's graph to the next.
    carry: Option<super::source_graph::Carry>,
    source_index: Arc<std::sync::Mutex<super::source_graph::SourceIndexSlot>>,
    scratch_parent: std::path::PathBuf,
    toolchain: Option<Arc<hirsel_native::Toolchain>>,
    host_toolchain: bool,
    #[cfg(test)]
    before_check: Option<fn(&mut hirsel_engine::LoadedProgram)>,
}
impl CompilerRequests {
    /// Capture the existing host guard cap. Construction is lazy/infallible;
    /// actual graph reservations and refusal belong to request selection.
    pub fn for_host(compiler: Compiler, config: CompileConfig) -> Self {
        let (budget, cap) = host_budget();
        let limits = host_limits(cap, config.jobs.get());
        let mut selected = Self::new(compiler, config, budget.clone(), limits);
        selected.host_toolchain = true;
        selected
    }
    /// Execute a fresh immutable request, including declaration export's source-only discovery.
    pub fn execute(
        &mut self,
        path: &std::path::Path,
        operation: CompileOperation,
    ) -> Result<Arc<CompileOutput>, hirsel_engine::ToolError> {
        hirsel_intern::jobs::with_compiler_stack(|| self.execute_on_stack(path, operation))
    }
    fn execute_on_stack(
        &mut self,
        path: &std::path::Path,
        operation: CompileOperation,
    ) -> Result<Arc<CompileOutput>, hirsel_engine::ToolError> {
        use hirsel_engine::requests::observe_inputs;
        // Discovery and graph installation use the same independent provider.
        // Restore selector policy before returning; retained older graphs keep
        // their own completed products and never inherit this operation's tail.
        let previous_config = self.config.clone();
        let previous_compiler = self.compiler.clone();
        self.config.store = previous_config
            .store
            .for_operation()
            .unwrap_or_else(|| previous_config.store.clone());
        self.compiler = previous_compiler.clone().with_config(self.config.clone());
        let store_operation = self.config.store.operation_scope();
        let output = (|| {
            let (loaded, inputs) =
                observe_inputs(self.input_limits(), self.capture_attempts(), |inputs| {
                    if matches!(operation, CompileOperation::DataAbi) {
                        self.compiler.load_export_package_inputs(path, inputs)
                    } else {
                        self.compiler.load_check_package_inputs(path, inputs, true)
                    }
                })
                .map_err(|_| {
                    hirsel_engine::ToolError::Runtime(
                        "compiler inputs kept changing during discovery".into(),
                    )
                })?;
            drop(loaded?);
            self.execute_request(&CompileRequest {
                path: path.to_path_buf(),
                inputs: inputs.into(),
                operation,
            })
        })();
        self.config = previous_config;
        self.compiler = previous_compiler;
        if let Ok(completed) = &output
            && !matches!(&completed.result, CompileResult::Check(checked) if checked.completion != hirsel_engine::CheckCompletion::Complete)
        {
            let publication = store_operation.finish();
            if matches!(completed.result, CompileResult::Precomputed { .. }) {
                publication
                    .map_err(|error| hirsel_engine::ToolError::Runtime(error.to_string()))?;
            }
        }
        output
    }
    /// Execute exactly the supplied observed inputs without polling the filesystem.
    /// Source overlays retain the same graph selection, admission and readiness.
    pub fn execute_request(
        &mut self,
        request: &CompileRequest,
    ) -> Result<Arc<CompileOutput>, hirsel_engine::ToolError> {
        hirsel_intern::jobs::with_compiler_stack(|| self.execute_request_on_stack(request))
    }
    fn execute_request_on_stack(
        &mut self,
        request: &CompileRequest,
    ) -> Result<Arc<CompileOutput>, hirsel_engine::ToolError> {
        use hirsel_engine::requests::{OutputReadiness, RequestEngine};
        use hirsel_engine_runtime::QueryRuntime;

        let mut selected = self.select(request)?;
        let completed = {
            let mut runtime = selected
                .runtime
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let request = runtime.request(&selected.roots);
            runtime.wait(request)
        }
        .map_err(|reference| {
            selected.output.failed(reference).unwrap_or_else(|| {
                hirsel_engine::ToolError::Runtime("compiler request diagnostics unavailable".into())
            })
        })?;
        selected.output.ready(&completed)
    }
    /// Actual cumulative work across this selector's source graphs, including
    /// graphs already evicted. This reads the bounded ledger without observing
    /// inputs, installing a producer, or retaining an idle runtime.
    pub fn recompute_counts(
        &self,
        queries: &[(hirsel_syntax::DefPath, hirsel_engine::QueryKind)],
    ) -> Result<
        Vec<(hirsel_syntax::DefPath, hirsel_engine::QueryKind, u64)>,
        hirsel_engine::ToolError,
    > {
        self.ledger.counts(queries)
    }

    pub fn input_limits(&self) -> InputLimits {
        InputLimits {
            bytes: usize::try_from(self.limits.snapshot_bytes.min(16 << 20)).unwrap_or(usize::MAX),
            operations: 4096,
        }
    }
    pub fn capture_attempts(&self) -> usize {
        3
    }

    pub fn new(
        compiler: Compiler,
        config: CompileConfig,
        budget: Arc<hirsel_engine_admission::ProcessBudget>,
        limits: RequestLimits,
    ) -> Self {
        // Every provider can evict ordinary products. Compact front and Lower
        // facts therefore have the same bounded carry regardless of provider;
        // with no store, they and the memo graph are a session's only reuse.
        let quarter = limits.transport_bytes / 4;
        let front = budget
            .reserve(quarter)
            .ok()
            .and_then(|allowance| hirsel_engine::FrontCarry::new(allowance).ok());
        let lower = budget
            .reserve(quarter)
            .ok()
            .and_then(|allowance| hirsel_engine::LowerCarry::new(allowance).ok());
        let carry = front
            .zip(lower)
            .filter(|_| config.verify_store == hirsel_engine::store::VerifyStore::Off)
            .map(|(front, lower)| super::source_graph::Carry { front, lower });
        let graphs = super::host_selector::GraphSelections::new(&budget, config.hold_bytes != 0);
        let ledger =
            super::inspection::InspectionLedger::new(budget.clone(), limits.snapshot_bytes / 8);
        Self {
            graphs,
            ledger,
            invocation_store: None,
            compiler: compiler.with_config(config.clone()).with_owned_engines(),
            config,
            budget,
            limits,
            carry,
            source_index: Arc::new(std::sync::Mutex::new(Default::default())),
            scratch_parent: std::env::temp_dir(),
            toolchain: None,
            host_toolchain: false,
            #[cfg(test)]
            before_check: None,
        }
    }
    /// Physical request transport policy, captured before any source read.
    pub fn with_scratch_parent(mut self, parent: std::path::PathBuf) -> Self {
        self.scratch_parent = parent;
        self.graphs =
            super::host_selector::GraphSelections::new(&self.budget, self.config.hold_bytes != 0);
        self
    }
    pub fn with_toolchain(mut self, toolchain: Arc<hirsel_native::Toolchain>) -> Self {
        self.toolchain = Some(toolchain);
        self.graphs =
            super::host_selector::GraphSelections::new(&self.budget, self.config.hold_bytes != 0);
        self
    }
    pub fn compiler(&self) -> &Compiler {
        &self.compiler
    }
    pub fn config(&self) -> &CompileConfig {
        &self.config
    }
    pub fn budget(&self) -> &Arc<hirsel_engine_admission::ProcessBudget> {
        &self.budget
    }
    pub fn limits(&self) -> RequestLimits {
        self.limits
    }
    pub fn toolchain(&self) -> Option<&Arc<hirsel_native::Toolchain>> {
        self.toolchain.as_ref()
    }
    /// Test fault injection: run on each freshly loaded program before Check.
    #[cfg(test)]
    pub(crate) fn set_before_check(
        &mut self,
        before_check: Option<fn(&mut hirsel_engine::LoadedProgram)>,
    ) {
        self.before_check = before_check;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_graphs_share_budget_and_charge_stacks_without_raising_the_cap() {
        let first = CompilerRequests::for_host(crate::COMPILER, CompileConfig::default());
        let second = CompilerRequests::for_host(crate::COMPILER, CompileConfig::default());
        assert!(Arc::ptr_eq(first.budget(), second.budget()));
        let cap = hirsel_guard::DEFAULT_LIMIT;
        for jobs in [1, 2, 4, 8, 16] {
            let limits = host_limits(cap, jobs);
            assert!(
                limits.runtime_bytes + limits.snapshot_bytes + limits.transport_bytes
                    <= cap - cap / 4
            );
        }
        let zero = host_limits(0, 2);
        assert_eq!(zero.snapshot_bytes + zero.transport_bytes, 0);
        assert!(
            hirsel_engine_admission::ProcessBudget::new(0)
                .reserve(zero.runtime_bytes)
                .is_err()
        );
        assert_eq!(first.capture_attempts(), 3);
        assert!(first.input_limits().bytes <= 16 << 20);
        let (budget, cap) = host_budget();
        let before = budget.reserved();
        let directory = tempfile::tempdir().unwrap();
        let target = hirsel_native::TargetSpec::x86_64_unknown_linux_gnu();
        let tools = hirsel_native::Toolchain::in_tree(directory.path(), &[&target]).unwrap();
        let charged = before + (cap / 8).min(128 << 20);
        assert_eq!(budget.reserved(), charged);
        let view = tools.products.clone();
        drop(tools);
        assert_eq!(budget.reserved(), charged);
        drop(view);
        assert_eq!(budget.reserved(), before);
    }
}

impl CompilerRequests {
    /// Swap the invocation store shared by discovery and graph selection.
    pub fn set_invocation_store(
        &mut self,
        store: Option<Arc<dyn hirsel_engine::store::ResultStore>>,
    ) -> Option<Arc<dyn hirsel_engine::store::ResultStore>> {
        std::mem::replace(&mut self.invocation_store, store)
    }
}

impl hirsel_engine::requests::RequestEngine for CompilerRequests {
    type Request = CompileRequest;
    type Error = hirsel_engine::ToolError;
    type Runtime = hirsel_engine_runtime::AdmittedRuntime;
    type Readiness = CompilerReadiness;

    fn select(
        &mut self,
        request: &CompileRequest,
    ) -> Result<hirsel_engine::requests::SelectedRequest<Self::Runtime, Self::Readiness>, Self::Error>
    {
        self.ledger.admit()?;
        let ledger = self.ledger.clone();
        let mut config = self.config.clone();
        if let Some(store) = &self.invocation_store {
            config.store = store.clone();
        }
        let compiler = self.compiler.clone().with_config(config.clone());
        let limits = self.limits;
        let carry = self.carry.as_ref();
        let source_index = &self.source_index;
        let scratch_parent = &self.scratch_parent;
        let toolchain = self.toolchain.as_ref();
        let host_toolchain = self.host_toolchain;
        #[cfg(test)]
        let before_check = self.before_check;
        self.graphs.select(request, |budget, may_wait| {
            super::source_graph::install(
                &compiler,
                &config,
                limits,
                budget,
                may_wait,
                request,
                ledger,
                carry,
                source_index,
                scratch_parent,
                toolchain,
                host_toolchain,
                #[cfg(test)]
                before_check,
            )
        })
    }
}

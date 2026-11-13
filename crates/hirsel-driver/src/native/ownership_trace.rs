//! Source requests and launches for explicit ownership diagnostics.
use hirsel_native::*;
use hirsel_native_testing::ownership_trace::LeafExecution;
/// `hirsel run --native --trace-ownership`: select the source-owned diagnostic
/// graph and execute its original Value-to-Ready plans with the opt-in runtime.
/// Unsupported ownership transport refuses before any executable runs.
pub fn run_path(
    path: &std::path::Path,
    backend: Backend,
    policy: crate::session::Policy,
    event_limit: usize,
) -> Result<Result<LeafExecution, NativeError>, crate::ToolError> {
    run_path_with_launch(
        path,
        crate::HostLaunch::empty(),
        backend,
        policy,
        event_limit,
    )
}

/// Admit the same grants or replay record as ordinary native execution before
/// launching the source-owned diagnostic objects. No trace grants authority.
pub fn run_path_with_launch(
    path: &std::path::Path,
    launch: crate::HostLaunch<'_>,
    backend: Backend,
    policy: crate::session::Policy,
    event_limit: usize,
) -> Result<Result<LeafExecution, NativeError>, crate::ToolError> {
    hirsel_intern::jobs::with_compiler_stack(|| {
        use crate::requests::{
            CompileOperation, CompileResult, CompilerRequests, OwnershipRequest,
        };
        let selection = crate::launch_binding::LaunchSelection::read_for_compilation(launch)?;
        let inputs = selection.compiler_inputs()?;
        let definition = selection.entry().cloned();
        let compiler = policy.compiler().with_owned_engines();
        let config = compiler.configuration();
        let selector = CompilerRequests::for_host(compiler.clone(), config.clone());
        let limits = selector.input_limits();
        let attempts = selector.capture_attempts();
        let mut host = crate::resident::ObservedSession::new(
            path.to_owned(),
            compiler,
            config,
            selector,
            limits,
            attempts,
        );
        let target = super::TargetSpec::x86_64_unknown_linux_gnu();
        let output = host.request(CompileOperation::NativeOwnership(OwnershipRequest {
            selection: hirsel_lir::ownership_trace::Selection::Resources,
            definition,
            target: target.clone(),
            validation: super::Validation::EveryStage,
            launch: inputs,
        }))?;
        let CompileResult::Ownership(execution) = &output.result else {
            return Err(crate::ToolError::Runtime(
                "ownership request returned another operation".into(),
            ));
        };
        let execution = execution.clone();
        drop(output);
        drop(host);
        Ok((|| {
            let objects = execution
                .emit(backend)
                .map_err(|error| NativeError::new(StageName::Emission, error))?;
            let cache = std::env::var_os("HIRSEL_NATIVE_CACHE")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(super::runtime_cache::default_directory);
            let toolchain = super::Toolchain::in_tree_with_runtime(
                &cache,
                &[&target],
                super::RuntimeBuild {
                    ownership_trace: true,
                    ..Default::default()
                },
            )?;
            let scratch = hirsel_native::Scratch::new()?;
            execution
                .launch_with_selection(
                    &objects,
                    &toolchain,
                    &scratch.0,
                    event_limit,
                    launch,
                    selection,
                )
                .map_err(|error| NativeError::new(StageName::Link, error))
        })())
    })
}

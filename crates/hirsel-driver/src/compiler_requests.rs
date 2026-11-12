//! Explicit compiler entry points use the same immutable requests as Sessions.
use crate::session::Session;
use hirsel_engine::{CheckOutput, Compiler, ToolError};
use std::path::Path;

/// Explicit checks preserve this compiler's policy on immutable request graphs.
pub trait CompilerRequestsExt {
    /// Populate per-definition source products without requiring a launch entry.
    fn precompute_package(&self, manifest: &Path, output: &Path) -> Result<(), ToolError> {
        self.precompute_package_observed(manifest, output).map(drop)
    }
    /// Precompute, returning what the published products' work used, by
    /// part; None when the root already held exactly this Precompute.
    fn precompute_package_observed(
        &self,
        manifest: &Path,
        output: &Path,
    ) -> Result<Option<hirsel_engine_store::Observations>, ToolError>;
    /// Check a source or package, returning its current source diagnostics.
    fn check_path(&self, path: &Path) -> Result<CheckOutput, ToolError>;
    /// Check the source, then observe the accepted launch entry's unsafe reach.
    fn check_safe_path(
        &self,
        path: &Path,
    ) -> Result<
        (
            CheckOutput,
            Option<(hirsel_syntax::DefPath, Option<hirsel_engine::UnsafeReach>)>,
        ),
        ToolError,
    >;
}
impl CompilerRequestsExt for Compiler {
    fn precompute_package_observed(
        &self,
        manifest: &Path,
        output: &Path,
    ) -> Result<Option<hirsel_engine_store::Observations>, ToolError> {
        use crate::requests::{CompileOperation, CompileResult, CompilerRequests};
        use hirsel_engine::Verify;
        use hirsel_engine::memo::Retention;
        use hirsel_engine_store::DirStore;
        let mut config = self.configuration();
        if config.verify == Verify::Off {
            return Err(ToolError::Runtime(
                "package precomputation requires V".into(),
            ));
        }
        let installation = hirsel_artifact_store::registration::Installation::default_state()
            .and_then(|state| {
                hirsel_artifact_store::registration::Installation::begin(
                    output,
                    &state,
                    self.identity(),
                )
                .ok()
            });
        let store = DirStore::installation(output.to_path_buf());
        let store = std::sync::Arc::new(store);
        // Recording is on for exactly the work that publishes: everything
        // from here to the published header, on every thread it reaches.
        let edges = std::sync::Arc::new(hirsel_intern::attribution::Edges::default());
        let _traced = hirsel_intern::attribution::traced(edges.clone());
        config.store = store.clone();
        config.retention = Retention::Batch;
        config.hold_bytes = 0;
        let compiler = self
            .clone()
            .with_owned_engines()
            .with_config(config.clone());
        let mut requests = CompilerRequests::for_host(compiler, config);
        if installation_current(self, output, requests.input_limits()) {
            // The root already holds exactly this Precompute, and V's own
            // registration of its products loads. Nothing is written.
            return Ok(None);
        }
        let completed = requests.execute(manifest, CompileOperation::Precompute)?;
        if !matches!(completed.result, CompileResult::Precomputed { .. }) {
            return Err(ToolError::Runtime(
                "precompute source product is unavailable".into(),
            ));
        }
        use hirsel_engine::loader_inputs::LoaderInputs;
        // Indexed, so the image's name index (N) is built from the same
        // parse heads the installation's records hold.
        let loaded = self.load_check_package_inputs(manifest, &completed.inputs, true)?;
        let manifest_path = completed.inputs.canonicalize(manifest)?;
        let package = loaded.installation_inputs(
            self.identity(),
            completed.inputs.read(&manifest_path)?.to_vec(),
            match &completed.inputs {
                crate::requests::CompileInputs::Frozen(inputs) => {
                    self.installation_footprint(inputs)
                }
                // No recorded footprint: a later Precompute never reuses it.
                _ => Default::default(),
            },
        );
        let header = store.publish_installation(package)?;
        if let Some(installation) = installation {
            // Product installation remains usable when optional durable local
            // registration cannot be published. A later Check runs V again.
            // V binds the header this run published, not what the root holds.
            let _ = installation.finish(header);
        }
        drop(_traced);
        Ok(Some(hirsel_engine_store::Observations::of(
            &edges.take(),
            &store.take_installation_uses(),
        )))
    }
    fn check_path(&self, path: &Path) -> Result<CheckOutput, ToolError> {
        let config = self.configuration();
        let compiler = self.clone().with_config(config.clone());
        Session::open_with_compiler(path, compiler).check()
    }
    fn check_safe_path(
        &self,
        path: &Path,
    ) -> Result<
        (
            CheckOutput,
            Option<(hirsel_syntax::DefPath, Option<hirsel_engine::UnsafeReach>)>,
        ),
        ToolError,
    > {
        let config = self.configuration();
        let compiler = self.clone().with_config(config.clone());
        Session::open_with_compiler(path, compiler).check_safe()
    }
}

/// A previous Precompute into `output` is reused only when its header opens
/// checked with every record intact, a replay of its loader footprint gives
/// the same key under this compiler, and V's registration of exactly this
/// root's products loads. A key match alone is never V authority.
fn installation_current(
    compiler: &Compiler,
    output: &Path,
    limits: hirsel_engine::loader_inputs::InputLimits,
) -> bool {
    let Ok(recorded) =
        hirsel_engine_store::InstalledPackage::current_inputs(output, compiler.identity())
    else {
        return false;
    };
    if recorded.operations.is_empty() {
        return false;
    }
    let Some(replayed) = Compiler::replay_installation_footprint(&recorded.operations, limits)
    else {
        return false;
    };
    if compiler.installation_key(&replayed) != recorded.key {
        return false;
    }
    hirsel_artifact_store::registration::Installation::default_state().is_some_and(|state| {
        hirsel_artifact_store::registration::Installation::registered(
            output,
            &state,
            compiler.identity(),
        )
        .is_some()
    })
}

impl crate::resident::ObservedEngine for crate::requests::CompilerRequests {
    fn execute_request(
        &mut self,
        request: &crate::requests::CompileRequest,
    ) -> Result<std::sync::Arc<crate::requests::CompileOutput>, hirsel_engine::ToolError> {
        crate::requests::CompilerRequests::execute_request(self, request)
    }

    fn set_invocation_store(
        &mut self,
        store: Option<std::sync::Arc<dyn hirsel_engine_store::ResultStore>>,
    ) -> Option<std::sync::Arc<dyn hirsel_engine_store::ResultStore>> {
        Self::set_invocation_store(self, store)
    }
}

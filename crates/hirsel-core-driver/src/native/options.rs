use super::*;

/// Runtime and linker inputs are explicit, independently produced artifacts.
#[derive(Clone)]
pub struct NativeOptions {
    /// Recording runtime (`build --recording`, `run --record`, `replay`, `test --replay`).
    pub recording: bool,
    pub store_teardown: bool,
    /// Coverage build (`docs/coverage.md`) scope, or none.
    pub coverage: Option<hirsel_core_ir::coverage::Scope>,
    pub replay: Option<ReplayPlan>,
    pub backend: Backend,
    pub optimize: bool,
    pub debug_lines: bool,
    pub target: String,
    pub runtime: PathBuf,
    pub link: Link,
    pub cas: PathBuf,
    pub output: PathBuf,
    pub output_kind: OutputKind,
    /// Optional explicit list of closed function exports for `OutputKind::Module` (PACK-14/15).
    pub module_exports: Vec<DefPath>,
    pub root_limit: u64,
    pub instance_limit: usize,
    pub directories: Vec<String>,
    pub grant_mask: u64,
    pub net_rules: Vec<hirsel_core_net::GrantRule>,
    pub unix_roots: std::collections::BTreeMap<String, String>,
    pub secret_grants: Vec<hirsel_core_net::SecretGrant>,
    /// `Cred` grants, one per named `main` parameter (TLS-7).
    pub cred_grants: Vec<hirsel_core_net::CredGrant>,
    pub foreign_grants: BTreeSet<String>,
    pub(crate) exec_grants: Vec<crate::exec_launch::Binding>,
    pub env_patterns: Option<hirsel_core_net::EnvPatterns>,
    /// Diagnostic build: exact root reservation peak, unsuitable for timing samples.
    pub logical_charge_report: Option<PathBuf>,
    /// The remote cache authorized for this request.
    pub(crate) remote: Option<Arc<crate::store::remote::Remote>>,
    /// The toolchain this request already observed for its request record's
    /// key: the build links with it rather than observing again, so the
    /// executable is the one the key names.
    pub(crate) observed: Option<
        Arc<(
            crate::tool_inputs::Toolchain,
            crate::tool_inputs::LinkRecipe,
        )>,
    >,
}

impl NativeOptions {
    pub(crate) fn compiler_worker(&self) -> Option<PathBuf> {
        if self.grant_mask & (1 << hirsel_core_ir::release_recipe::GrantKind::Compiler as u8) == 0 {
            return None;
        }
        crate::installation::compiler_executable().ok()
    }

    pub(crate) fn llvm_release(&self) -> bool {
        matches!(self.backend, Backend::Llvm) && self.optimize
    }

    pub(crate) fn bind_exec(&mut self) -> Result<(), NativeStop> {
        self.exec_grants
            .iter_mut()
            .try_for_each(crate::exec_launch::Binding::seal_policy)
    }

    /// Development defaults use Cranelift without optimizations, line tables,
    /// or a charge report. An installation prebuilds with these options.
    pub fn development(target: String, runtime: PathBuf, link: Link, output: PathBuf) -> Self {
        Self {
            backend: Backend::Cranelift,
            optimize: false,
            debug_lines: false,
            target,
            runtime,
            link,
            cas: crate::store::collect::user(),
            output,
            output_kind: OutputKind::Executable,
            module_exports: vec![],
            root_limit: DEFAULT_ROOT_LIMIT,
            instance_limit: DEFAULT_INSTANCE_LIMIT,
            recording: false,
            store_teardown: false,
            coverage: None,
            replay: None,
            directories: vec![],
            grant_mask: 0,
            net_rules: vec![],
            unix_roots: Default::default(),
            secret_grants: vec![],
            cred_grants: vec![],
            foreign_grants: BTreeSet::new(),
            exec_grants: vec![],
            env_patterns: None,
            logical_charge_report: None,
            observed: None,
            remote: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BuildProduct {
    /// Native implementation copies, never semantic type instances.
    pub private_variants: usize,
    pub executable: PathBuf,
    /// The executable's key: its objects' identities, the entry plan, the
    /// sources digest and the link inputs. Its record is stored under it.
    pub identity: ContentHash,
    /// The entry's PROG-10 code address.
    pub address: ContentHash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ContentHash>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assembly: Option<recipe_build::AssemblyRecord>,
    /// The entry's PROG-9 revision.
    pub revision: ContentHash,
    pub compiled: usize,
    pub linked: bool,
    /// True means diagnostic instrumentation; timings are not performance samples.
    pub observation_mode: bool,
    pub debug_lines: bool,
    /// The walk's and the link's work counters.
    pub reach: crate::reach::ReachCounters,
    /// A coverage build's site map: the fragments of its linked objects,
    /// one record per id (`docs/coverage.md`, "The site map").
    #[serde(skip)]
    pub(crate) coverage: Option<Arc<hirsel_core_ir::coverage::Fragment>>,
}

/// The environment every development object key starts with, encoded: the
/// object schema, the compiler, the runtime ABI, the engine library, the
/// target, the backend and the emission settings.
pub(crate) fn environment(options: &NativeOptions, runtime: ContentHash) -> io::Result<Vec<u8>> {
    let mut environment = encode(&(
        crate::native_object::SCHEMA,
        compiler_identity(),
        runtime,
        engine_library(options.backend)?,
        &options.target,
        options.backend,
        options.optimize,
        options.recording,
        options.logical_charge_report.is_some(),
        !options.foreign_grants.is_empty(),
        matches!(options.backend, Backend::Llvm) && !hirsel_llvm_lazy::core::facts_enabled(),
    ))?;
    // Coverage and store-teardown switches append their tags only when set.
    if let Some(scope) = &options.coverage {
        environment.extend(encode(&(hirsel_core_ir::coverage::KEY_TAG, scope))?);
    }
    if options.store_teardown {
        environment.extend(encode(&"hirsel.store-teardown.v1")?);
    }
    if options.output_kind == OutputKind::Module {
        environment.extend(encode(&"hirsel.pack-module.v1")?);
    } else if options.output_kind == OutputKind::StaticLibrary {
        environment.extend(encode(&"hirsel.pack-host.v1")?);
    }
    Ok(environment)
}

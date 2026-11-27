use super::*;

impl NativeOptions {
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

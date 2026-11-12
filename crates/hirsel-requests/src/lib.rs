//! Explicit immutable request selection shared by fresh and resident callers.
//! The engine installs factories; these descriptors select operations and never
//! accept caller-supplied tasks, caches, verifier receipts or mutable programs.
use hirsel_engine::loader_inputs::DirectoryEntry;
use hirsel_engine::loader_inputs::FrozenInputs;
use hirsel_engine::loader_inputs::InputKind;
use hirsel_engine::loader_inputs::LoaderInputs;
use hirsel_engine::loader_inputs::namespace::NamespaceLease;
use hirsel_native::BuildOptions;
use hirsel_native::LaunchInputs;
use hirsel_native::TargetSpec;
use hirsel_native::Validation;
use hirsel_query_products::LoweringPolicy;
use hirsel_syntax::DefPath;
use std::io;
use std::path::PathBuf;
mod host_selector;
mod inspection;
mod memory_inputs;
mod native_artifacts;
mod native_assembly;
mod native_bootstrap;
mod native_link;
mod native_lower;
mod native_platform;
mod native_products;
mod native_routing;
mod native_selection;
mod native_sources;
mod native_store;
mod output;
pub use memory_inputs::MemoryInputs;
mod ownership_admission;
mod ownership_bootstrap;
mod ownership_selection;
mod proof_export;
mod replay_product;
mod safe_check;
mod selector;
mod source_graph;
mod tir_products;
pub use native_platform::{HostLauncher, HostPlatform, host_executable_platform};
pub use native_products::evidence_identity;
pub use output::{
    CompileInspection, CompileOutput, CompileResult, CompilerReadiness, DefinitionWork,
    NativeExecution, NativeObjectOutput, NativeObservation, NativeReady, PrecomputedSources,
    RequestWork, TirBatchExecution, TirExecution, TirProducts,
};
pub use ownership_selection::{OwnershipExecution, OwnershipObjects};
pub use replay_product::import_replay_execution;
pub use selector::{CompilerRequests, RequestLimits};

/// Already observed inputs, including missing files, manifests, aliases and
/// directory membership. Selection must use these bytes verbatim; observing
/// newer disk state is a separate request. Compiler policy is captured by the
/// request engine's Compiler, including explicit jobs, V and selected stores.
#[derive(Clone, Debug)]
pub enum CompileInputs {
    Frozen(FrozenInputs),
    Namespace(NamespaceLease),
    Memory(MemoryInputs),
}
impl CompileInputs {
    pub(crate) fn execution_identity(&self) -> hirsel_syntax::ContentHash {
        match self {
            Self::Frozen(inputs) => hirsel_syntax::ContentHash::of_parts(
                "hirsel.execution.frozen-inputs.v1",
                inputs.identity_parts().iter().map(Vec::as_slice),
            ),
            Self::Namespace(inputs) => inputs.digest(),
            Self::Memory(inputs) => inputs.execution_identity(),
        }
    }
    pub fn known_path(&self, path: &std::path::Path) -> Option<PathBuf> {
        match self {
            Self::Frozen(inputs) => inputs.known_path(path),
            Self::Memory(inputs) => inputs.canonicalize(path).ok(),
            Self::Namespace(inputs) => inputs.canonicalize(path).ok(),
        }
    }
    pub fn replace_sources(
        &self,
        replacements: &std::collections::BTreeMap<PathBuf, std::sync::Arc<[u8]>>,
        limit: usize,
    ) -> io::Result<Self> {
        match self {
            Self::Frozen(inputs) => inputs
                .replace_sources(replacements, limit)
                .map(Self::Frozen),
            Self::Memory(inputs) => inputs
                .replace_sources(replacements, limit)
                .map(Self::Memory),
            Self::Namespace(_) => Err(io::Error::other(
                "namespace edits require a new namespace lease",
            )),
        }
    }
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::Frozen(inputs) => inputs.retained_bytes(),
            Self::Namespace(inputs) => inputs.usage().charged_bytes,
            Self::Memory(inputs) => inputs.retained_bytes(),
        }
    }

    pub fn read_work(&self) -> (u64, u64) {
        match self {
            Self::Frozen(inputs) => inputs.read_work(),
            Self::Namespace(inputs) => inputs.read_work(),
            Self::Memory(_) => (0, 0),
        }
    }
}
impl From<FrozenInputs> for CompileInputs {
    fn from(inputs: FrozenInputs) -> Self {
        Self::Frozen(inputs)
    }
}
impl From<NamespaceLease> for CompileInputs {
    fn from(inputs: NamespaceLease) -> Self {
        Self::Namespace(inputs)
    }
}
impl PartialEq for CompileInputs {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Frozen(a), Self::Frozen(b)) => a == b,
            (Self::Namespace(a), Self::Namespace(b)) => a.same_namespace(b),
            (Self::Memory(a), Self::Memory(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for CompileInputs {}
impl LoaderInputs for CompileInputs {
    fn read(&self, path: &std::path::Path) -> io::Result<std::sync::Arc<[u8]>> {
        match self {
            Self::Frozen(inputs) => inputs.read(path),
            Self::Namespace(inputs) => inputs.read(path),
            Self::Memory(inputs) => inputs.read(path),
        }
    }

    fn canonicalize(&self, path: &std::path::Path) -> io::Result<PathBuf> {
        match self {
            Self::Frozen(inputs) => inputs.canonicalize(path),
            Self::Namespace(inputs) => inputs.canonicalize(path),
            Self::Memory(inputs) => inputs.canonicalize(path),
        }
    }

    fn metadata(&self, path: &std::path::Path) -> io::Result<InputKind> {
        match self {
            Self::Frozen(inputs) => inputs.metadata(path),
            Self::Namespace(inputs) => inputs.metadata(path),
            Self::Memory(inputs) => inputs.metadata(path),
        }
    }

    fn directory(&self, path: &std::path::Path) -> io::Result<Vec<DirectoryEntry>> {
        match self {
            Self::Frozen(inputs) => inputs.directory(path),
            Self::Namespace(inputs) => inputs.directory(path),
            Self::Memory(inputs) => inputs.directory(path),
        }
    }

    fn toolchain_std_manifest(&self) -> Option<io::Result<PathBuf>> {
        match self {
            Self::Frozen(inputs) => inputs.toolchain_std_manifest(),
            Self::Namespace(inputs) => inputs.toolchain_std_manifest(),
            Self::Memory(inputs) => inputs.toolchain_std_manifest(),
        }
    }
}
#[derive(Clone)]
pub struct CompileRequest {
    pub path: PathBuf,
    pub inputs: CompileInputs,
    pub operation: CompileOperation,
}
#[derive(Clone)]
pub enum CompileOperation {
    /// Pure selected-graph metadata; this operation never demands a producer.
    Inspect {
        definitions: Vec<DefPath>,
    },
    /// Explicit canonical oracle exports from the same compiler graph.
    Artifacts {
        native: bool,
    },
    Check {
        lowering: LoweringPolicy,
    },
    /// Populate complete source semantic, audit, Lower and constant products.
    Precompute,
    SafeCheck,
    /// Generated bodies in primary-source order after the accepted Check.
    Expand,
    /// LANG-6 hashes of a package and its complete source closure.
    Hashes {
        package: Option<hirsel_syntax::PackageId>,
    },
    /// Canonical TIR text from the accepted immutable Run closure.
    ExportTir,
    /// Explicit evidence collection; normal Check does not construct receipts.
    CheckedProducts {
        lowering: LoweringPolicy,
        canonical: bool,
    },
    /// Observe work counters without demanding or recomputing these definitions.
    RecomputeCounts {
        queries: Vec<(DefPath, hirsel_query_products::QueryKind)>,
    },
    /// None selects the declared launch entry using the observed manifest.
    Tir {
        definition: Option<DefPath>,
    },
    /// One checked source closure serving these independently bound entries.
    TirBatch {
        definitions: Vec<DefPath>,
    },
    NativeBuild(NativeRequest),
    NativeRun(NativeRequest),
    /// Explicit source-selected Value-to-Ready ownership diagnostics.
    NativeOwnership(OwnershipRequest),
    /// Checked C declaration exports before foreign object inputs exist.
    DataAbi,
    /// Checked source inventory and original Lower products, before native import.
    NativeSources {
        roots: Vec<DefPath>,
    },
}
/// Backend and launch policy, independent of destination paths or host output
/// writers. Build and run select the same compilation products; execution and
/// executable materialization follow their completed readiness descriptor.
#[derive(Clone)]
pub struct NativeRequest {
    /// Selected launch definition; None uses the observed package entry.
    pub definition: Option<DefPath>,
    /// Ordered test dispatcher entries and their seeds. Empty for an ordinary
    /// executable; binding occurs before native metadata admission.
    pub test_entries: Vec<(DefPath, u64)>,
    /// Additional source produced by the package test or surface compiler.
    /// These ordinary inputs are parsed and checked in the owning package.
    pub supplemental_sources: Vec<NativeSourceText>,
    pub output: NativeOutput,
    /// Every compiler choice, including target, validation and launch inputs.
    /// Presets such as `BuildProfile` produce this value; nothing overrides it.
    pub options: BuildOptions,
    /// The independently validated run bound against the selected source facts.
    pub run: Option<std::sync::Arc<hirsel_run_config::ValidatedRun>>,

    /// Independently assessed source evidence; its selected discharge policy
    /// participates in native actions, never a launch-time fallback.
    pub evidence: Option<std::sync::Arc<hirsel_native::Evidence>>,
    /// A platform selection made for this request before it ran (a program
    /// record lookup's). Outside request identity: the platform identity
    /// already keys every link action. Taken only by a selection with the
    /// same key and capture mode, so a request with foreign links or another
    /// mode captures for itself.
    pub platform:
        Option<std::sync::Arc<hirsel_native::object_queries::platform::record::HandedPlatform>>,
}

/// Immutable source input, never a body, task, or validation capability.
#[derive(Clone, serde::Serialize)]
pub struct NativeSourceText {
    pub name: String,
    pub context: hirsel_syntax::FileContext,
    pub text: std::sync::Arc<str>,
}

/// Diagnostic compiler policy; execution consumes only its checked products.
#[derive(Clone, Debug)]
pub struct OwnershipRequest {
    pub selection: hirsel_lir::ownership_trace::Selection,
    pub definition: Option<DefPath>,
    pub target: TargetSpec,
    pub validation: Validation,
    pub launch: LaunchInputs,
}

/// The requested build artifact, independent of runtime launchability.
#[derive(Clone, serde::Serialize)]
pub enum NativeOutput {
    Executable,
    Objects {
        roots: Vec<DefPath>,
    },
    /// The settled Ready products of the entry and test entries, as an owned
    /// validated execution view (`NativeReady`); no Codegen or Link runs.
    Ready,
}

use hirsel_compiler_identity::COMPILER_IDENTITY;
#[cfg(test)]
use hirsel_front_phases::{COMPILER, compiler_adapters};

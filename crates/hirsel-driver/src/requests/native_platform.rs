//! Capture compiler-selected platform inputs and emission policy before native
//! root selection. This layer uses the ordinary native library resolver and
//! captured platform producer; it performs no source or native compiler work.
use super::{
    NativeRequest,
    native_bootstrap::NativePlan,
    native_link::{FrozenPlatform, LinkInputs},
};
use crate::native::{
    self, NativeError, StageName, Toolchain,
    object_queries::{
        capture::{CaptureLimits, FrozenLinkKit, FrozenLinker},
        platform::{CapturedPlatform, PlatformCaptureInputs, RuntimeSelection},
        runtime::RuntimeLimits,
    },
    queries::compilation_tasks::CompilationOutput,
};
use hirsel_engine::{LinkInput, runtime::MemoryReservation, store::ResultStore};
use hirsel_syntax::{ContentHash, DefPath};
use std::{path::PathBuf, sync::Arc};

/// Explicit package selection, captured by the host before input observation.
/// No tool or loader is discovered by an admitted producer's factory.
#[derive(Clone)]
pub(super) struct NativePlatformPolicy {
    pub linker: PathBuf,
    pub auditors: PathBuf,
    pub floor: PathBuf,
}
impl NativePlatformPolicy {
    pub fn capture() -> Result<Self, NativeError> {
        let selected = |name| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .ok_or_else(|| NativeError::new(StageName::Link, format!("missing {name}")))
        };
        Ok(Self {
            linker: selected("HIRSEL_CAPTURE_LINKER")?,
            auditors: selected("HIRSEL_AUDITOR_PACKAGE")?,
            floor: selected("HIRSEL_GLIBC_FLOOR")?,
        })
    }
}
/// The operation: a build or a launch. Emission policy is the request's
/// complete `BuildOptions`; a bound executable build can also carry a run.
#[derive(Clone, Copy)]
pub(super) enum NativePurpose {
    Build,
    Run,
}
/// Codegen identity of the request's complete backend selection. Ready pass
/// choices enter the separate native policy identity.
pub(super) fn profile_options(request: &NativeRequest) -> Result<ContentHash, NativeError> {
    let options = &request.options;
    let opt = match options.opt {
        native::OptLevel::None => 0u8,
        native::OptLevel::Default => 1,
        native::OptLevel::Speed => 2,
    };
    let partition = match options.partition {
        native::Partition::Whole => (0u8, 0u64),
        native::Partition::Singleton => (1, 0),
        native::Partition::Random(seed) => (2, seed),
    };
    let bytes = serde_json::to_vec(&(
        options.backend.name(),
        opt,
        partition,
        options.debug_tables,
        request.fuel,
    ))
    .map_err(|error| NativeError::new(StageName::Emission, error.to_string()))?;
    Ok(ContentHash::of_parts(
        "hirsel.host.native-profile.v4",
        [bytes.as_slice()],
    ))
}
/// The request's object partition as its ordinary Codegen grouping.
pub(super) fn codegen_partition(
    partition: native::Partition,
) -> native::queries::link_tasks::CodegenPartition {
    use native::queries::link_tasks::CodegenPartition;
    match partition {
        native::Partition::Whole => CodegenPartition::WholeModule,
        native::Partition::Singleton => CodegenPartition::PerInstance,
        native::Partition::Random(seed) => CodegenPartition::Seeded(seed),
    }
}
pub(super) struct NativePlatformInputs<'a> {
    pub purpose: NativePurpose,
    pub verify_store: hirsel_engine::store::VerifyStore,
    pub request: &'a NativeRequest,
    pub entry: &'a DefPath,
    pub tools: &'a Toolchain,
    pub links: &'a [LinkInput],
    pub policy: Option<&'a NativePlatformPolicy>,
}
pub(super) struct NativePlatformSelection {
    pub plan: NativePlan,
    pub platform: Option<Arc<FrozenPlatform>>,
}
impl NativePlatformSelection {
    pub fn capture(
        store: Arc<dyn ResultStore>,
        inputs: NativePlatformInputs<'_>,
        limits: CaptureLimits,
        runtime_limits: RuntimeLimits,
        allowance: impl Into<Arc<MemoryReservation>>,
    ) -> Result<Self, NativeError> {
        let request = inputs.request;
        let options = &request.options;
        let failed = |message| NativeError::new(StageName::Link, message);
        if request
            .run
            .as_ref()
            .is_some_and(|run| run.record().inputs != options.passes.launch)
        {
            return Err(NativeError::new(
                StageName::Manifest,
                "selected native launch inputs differ from the validated run",
            ));
        }
        let target = &options.passes.target;
        let unlinked = match &request.output {
            super::NativeOutput::Objects { roots } => {
                if roots.is_empty()
                    || !request.test_entries.is_empty()
                    || request.run.is_some()
                    || matches!(inputs.purpose, NativePurpose::Run)
                {
                    return Err(failed(
                        "object emission needs explicit roots and no launch selection".into(),
                    ));
                }
                Some(CompilationOutput::Objects)
            }
            super::NativeOutput::Ready => {
                if request.run.is_some() || matches!(inputs.purpose, NativePurpose::Run) {
                    return Err(failed(
                        "a Ready execution view has no launch selection".into(),
                    ));
                }
                Some(CompilationOutput::Ready)
            }
            super::NativeOutput::Executable => None,
        };
        if let Some(output) = unlinked {
            let profile_options = profile_options(request)?;
            return Ok(Self {
                plan: NativePlan {
                    options: options.passes.clone(),
                    policy: profile_options,
                    verify_store: if options.passes.validation == native::Validation::EveryStage {
                        hirsel_engine::store::VerifyStore::Recompute
                    } else {
                        inputs.verify_store
                    },
                    opt: options.opt,
                    debug: options.debug_tables,
                    verify_backend: true,
                    measurement: inputs.tools.runtime_build.measurement,
                    fuel: request.fuel,
                    requested_run: None,
                    test_entries: if matches!(output, CompilationOutput::Ready) {
                        request.test_entries.clone()
                    } else {
                        Vec::new()
                    },
                    output,
                    backends: vec![
                        crate::native::queries::compilation_tasks::CompilationBackend {
                            backend: options.backend.name().into(),
                            backend_version: native::backend_version(),
                            target_features: target.digest(),
                            profile_options,
                            runtime_abi: ContentHash::of_bytes(
                                &hirsel_lir::RUNTIME_ABI_VERSION.to_le_bytes(),
                            ),
                            linker_options: ContentHash::of_bytes(
                                b"native object output has no linker",
                            ),
                            runtime_archives: vec![],
                            foreign_inputs: vec![],
                            executor: Arc::new(ObjectOutput(options.partition)),
                        },
                    ],
                },
                platform: None,
            });
        }
        let policy = inputs
            .policy
            .ok_or_else(|| failed("native executable has no selected platform policy".into()))?;
        let (loader_name, emulator) = match target.triple.as_str() {
            "x86_64-unknown-linux-gnu" => ("ld-linux-x86-64.so.2", None),
            "aarch64-unknown-linux-gnu" => (
                "ld-linux-aarch64.so.1",
                Some(
                    inputs
                        .tools
                        .qemu_aarch64
                        .clone()
                        .ok_or_else(|| failed("missing selected AArch64 emulator".into()))?,
                ),
            ),
            _ => {
                return Err(failed(
                    "native executable capture requires a supported ELF target".into(),
                ));
            }
        };
        let floor = inputs
            .tools
            .glibc_floor
            .as_ref()
            .unwrap_or(&policy.floor)
            .join(&target.triple);
        let (loader, libraries) = inputs
            .tools
            .loader(target)?
            .unwrap_or_else(|| (floor.join(loader_name), floor));
        let auditor = policy.auditors.join(format!("{}.so", target.triple));
        let stripped = inputs
            .tools
            .archive(&target.triple)
            .ok_or_else(|| failed(format!("no runtime archive for {}", target.triple)))?;
        let archive =
            native::runtime_archive(inputs.tools, &target.triple, stripped, options.debug_tables)?;
        let foreign = native::selected_foreign_inputs(inputs.tools, target, inputs.links)?;
        let linker = FrozenLinker::snapshot(store.as_ref(), &policy.linker).map_err(failed)?;
        let kit =
            FrozenLinkKit::snapshot(store.as_ref(), &inputs.tools.kit, target, linker, limits)
                .map_err(failed)?;
        let captured = CapturedPlatform::capture(
            store.as_ref(),
            target,
            PlatformCaptureInputs {
                linker,
                kit,
                runtime_archive: archive,
                foreign: &foreign,
                runtime: RuntimeSelection {
                    loader: &loader,
                    auditor: &auditor,
                    library_path: libraries.as_os_str(),
                    emulator: emulator.as_deref(),
                    timeout: inputs.tools.launch_timeout,
                    events_bytes: runtime_limits.name_bytes as u64,
                    limits: runtime_limits,
                },
                limits,
            },
        )
        .map_err(failed)?;
        let platform = FrozenPlatform::new(
            store,
            LinkInputs {
                platform: captured,
                emulator,
                timeout: inputs.tools.launch_timeout,
                runtime_limits,
                limits,
            },
            inputs.entry.clone(),
            options.backend,
            options.partition,
            target.clone(),
            request.run.clone(),
            allowance,
        )
        .map_err(failed)?;
        let profile_options = profile_options(request)?;
        Ok(Self {
            plan: NativePlan {
                options: options.passes.clone(),
                policy: profile_options,
                verify_store: if options.passes.validation == native::Validation::EveryStage {
                    hirsel_engine::store::VerifyStore::Recompute
                } else {
                    inputs.verify_store
                },
                opt: options.opt,
                debug: options.debug_tables,
                verify_backend: true,
                measurement: inputs.tools.runtime_build.measurement,
                fuel: request.fuel,
                requested_run: request.run.clone(),
                test_entries: request.test_entries.clone(),
                output: CompilationOutput::Executable,
                backends: vec![platform.backend(
                    native::backend_version(),
                    target.digest(),
                    profile_options,
                    ContentHash::of_bytes(&hirsel_lir::RUNTIME_ABI_VERSION.to_le_bytes()),
                )],
            },
            platform: Some(platform),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::Backend;

    /// Every codegen choice reaches the Codegen identity, so no compilation
    /// product is reused across another backend selection.
    #[test]
    fn every_codegen_choice_is_explicit_in_codegen_profile_identity() {
        let request = |options: native::BuildOptions, fuel| NativeRequest {
            definition: None,
            test_entries: Vec::new(),
            supplemental_sources: Vec::new(),
            output: super::super::NativeOutput::Executable,
            options,
            run: None,
            fuel,
            evidence: None,
        };
        let base = native::BuildProfile::Default
            .options(
                Backend::Llvm,
                native::TargetSpec::x86_64_unknown_linux_gnu(),
                native::Validation::EveryStage,
            )
            .unwrap();
        let identity = |options, fuel| profile_options(&request(options, fuel)).unwrap();
        let plain = identity(base.clone(), false);
        assert_eq!(plain, identity(base.clone(), false));
        let mut variants = vec![identity(base.clone(), true)];
        for change in [
            |o: &mut native::BuildOptions| o.backend = Backend::Cranelift,
            |o: &mut native::BuildOptions| o.opt = native::OptLevel::None,
            |o: &mut native::BuildOptions| o.partition = native::Partition::Singleton,
            |o: &mut native::BuildOptions| o.partition = native::Partition::Random(7),
            |o: &mut native::BuildOptions| o.debug_tables = true,
        ] {
            let mut options = base.clone();
            change(&mut options);
            variants.push(identity(options, false));
        }
        for (index, variant) in variants.iter().enumerate() {
            assert_ne!(
                plain, *variant,
                "codegen choice {index} reuses another identity"
            );
        }
    }
}

/// Only chooses the request's backend partition. Compilation's emit-only and
/// Ready modes never create a Link plan, so calling `link` is a protocol error.
struct ObjectOutput(native::Partition);
impl native::queries::link_tasks::NativeLinkExecutor for ObjectOutput {
    fn codegen_partition(&self) -> native::queries::link_tasks::CodegenPartition {
        codegen_partition(self.0)
    }
    fn link(
        &self,
        _: &mut native::queries::roots::RootProducts,
        _: native::queries::link_tasks::LinkExecution<'_>,
    ) -> Result<Vec<u8>, String> {
        Err("object-only compilation cannot execute Link".into())
    }
}

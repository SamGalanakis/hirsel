//! Capture compiler-selected platform inputs and emission policy before native
//! root selection. This layer uses the ordinary native library resolver and
//! captured platform producer; it performs no source or native compiler work.
use super::NativeRequest;
use super::native_bootstrap::NativePlan;
use super::native_link::FrozenPlatform;
use super::native_link::LinkInputs;
use hirsel_engine::LinkInput;
use hirsel_engine_store::ResultStore;

use hirsel_native::NativeError;
use hirsel_native::StageName;
use hirsel_native::Toolchain;
use hirsel_native::object_queries::capture::CaptureLimits;
use hirsel_native::object_queries::platform::record::PlatformSpec;
use hirsel_native::object_queries::runtime::RuntimeLimits;
use hirsel_native_queries::compilation_tasks::CompilationOutput;
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
        hirsel_native::OptLevel::None => 0u8,
        hirsel_native::OptLevel::Default => 1,
        hirsel_native::OptLevel::Speed => 2,
    };
    let partition = match options.partition {
        hirsel_native::Partition::Whole => (0u8, 0u64),
        hirsel_native::Partition::Singleton => (1, 0),
        hirsel_native::Partition::Random(seed) => (2, seed),
    };
    let bytes = serde_json::to_vec(&(options.backend.name(), opt, partition, options.debug_tables))
        .map_err(|error| NativeError::new(StageName::Emission, error.to_string()))?;
    Ok(ContentHash::of_parts(
        "hirsel.host.native-profile.v4",
        [bytes.as_slice()],
    ))
}
/// The request's object partition as its ordinary Codegen grouping.
pub(super) fn codegen_partition(
    partition: hirsel_native::Partition,
) -> hirsel_native_queries::link_tasks::CodegenPartition {
    use hirsel_native_queries::link_tasks::CodegenPartition;
    match partition {
        hirsel_native::Partition::Whole => CodegenPartition::WholeModule,
        hirsel_native::Partition::Singleton => CodegenPartition::PerInstance,
        hirsel_native::Partition::Random(seed) => CodegenPartition::Seeded(seed),
    }
}
pub(super) struct NativePlatformInputs<'a> {
    pub purpose: NativePurpose,
    pub verify_store: hirsel_engine_store::VerifyStore,
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
        allowance: impl Into<Arc<hirsel_engine_admission::MemoryReservation>>,
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
                    verify_store: if options.passes.validation
                        == hirsel_native::Validation::EveryStage
                    {
                        hirsel_engine_store::VerifyStore::Recompute
                    } else {
                        inputs.verify_store
                    },
                    opt: options.opt,
                    debug: options.debug_tables,
                    verify_backend: true,
                    measurement: inputs.tools.runtime_build.measurement,

                    requested_run: None,
                    test_entries: if matches!(output, CompilationOutput::Ready) {
                        request.test_entries.clone()
                    } else {
                        Vec::new()
                    },
                    output,
                    backends: vec![
                        hirsel_native_queries::compilation_tasks::CompilationBackend {
                            backend: options.backend.name().into(),
                            backend_version: hirsel_native::backend_version(),
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
        let archive = hirsel_native::runtime_archive(
            inputs.tools,
            &target.triple,
            stripped,
            options.debug_tables,
        )?;
        let foreign = hirsel_native::selected_foreign_inputs(inputs.tools, target, inputs.links)?;
        let spec = PlatformSpec {
            target: target.clone(),
            linker: policy.linker.clone(),
            kit: inputs.tools.kit.clone(),
            runtime_archive: archive.to_path_buf(),
            foreign,
            loader,
            auditor,
            library_path: libraries.into_os_string(),
            emulator: emulator.clone(),
            timeout: inputs.tools.launch_timeout,
            events_bytes: runtime_limits.name_bytes as u64,
            runtime_limits,
            limits,
            debug_tables: options.debug_tables,
        };
        let mode = options.capture_mode().map_err(failed)?;
        let selected = spec.select(store.as_ref(), mode).map_err(failed)?;
        let allowance: Arc<hirsel_engine_admission::MemoryReservation> = allowance.into();
        let new = |platform, record| {
            FrozenPlatform::new(
                store.clone(),
                LinkInputs {
                    platform,
                    emulator: emulator.clone(),
                    timeout: inputs.tools.launch_timeout,
                    runtime_limits,
                    limits,
                    record,
                },
                inputs.entry.clone(),
                options.backend,
                options.partition,
                target.clone(),
                request.run.clone(),
                allowance.clone(),
            )
        };
        // A platform from a record that its own runtime check rejects was
        // shown wrong: delete the record and verify fully.
        let platform = if selected.hit {
            match new(selected.platform, Some(spec.clone())) {
                Ok(platform) => platform,
                Err(error) => {
                    let fresh = spec
                        .recapture(store.as_ref(), &format!("runtime check failed: {error}"))
                        .map_err(failed)?;
                    new(fresh, None).map_err(failed)?
                }
            }
        } else {
            new(selected.platform, None).map_err(failed)?
        };
        let profile_options = profile_options(request)?;
        Ok(Self {
            plan: NativePlan {
                options: options.passes.clone(),
                policy: profile_options,
                verify_store: if options.passes.validation == hirsel_native::Validation::EveryStage
                {
                    hirsel_engine_store::VerifyStore::Recompute
                } else {
                    inputs.verify_store
                },
                opt: options.opt,
                debug: options.debug_tables,
                verify_backend: true,
                measurement: inputs.tools.runtime_build.measurement,

                requested_run: request.run.clone(),
                test_entries: request.test_entries.clone(),
                output: CompilationOutput::Executable,
                backends: vec![platform.backend(
                    hirsel_native::backend_version(),
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
    use hirsel_native::Backend;

    /// Every codegen choice reaches the Codegen identity, so no compilation
    /// product is reused across another backend selection.
    #[test]
    fn every_codegen_choice_is_explicit_in_codegen_profile_identity() {
        let request = |options: hirsel_native::BuildOptions| NativeRequest {
            definition: None,
            test_entries: Vec::new(),
            supplemental_sources: Vec::new(),
            output: super::super::NativeOutput::Executable,
            options,
            run: None,

            evidence: None,
        };
        let base = hirsel_native::BuildProfile::Default
            .options(
                Backend::Llvm,
                hirsel_native::TargetSpec::x86_64_unknown_linux_gnu(),
                hirsel_native::Validation::EveryStage,
            )
            .unwrap();
        let identity = |options| profile_options(&request(options)).unwrap();
        let plain = identity(base.clone());
        assert_eq!(plain, identity(base.clone()));
        let mut variants = vec![];
        for change in [
            |o: &mut hirsel_native::BuildOptions| o.backend = Backend::Cranelift,
            |o: &mut hirsel_native::BuildOptions| o.opt = hirsel_native::OptLevel::None,
            |o: &mut hirsel_native::BuildOptions| o.partition = hirsel_native::Partition::Singleton,
            |o: &mut hirsel_native::BuildOptions| o.partition = hirsel_native::Partition::Random(7),
            |o: &mut hirsel_native::BuildOptions| o.debug_tables = true,
        ] {
            let mut options = base.clone();
            change(&mut options);
            variants.push(identity(options));
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
struct ObjectOutput(hirsel_native::Partition);
impl hirsel_native_queries::link_tasks::NativeLinkExecutor for ObjectOutput {
    fn codegen_partition(&self) -> hirsel_native_queries::link_tasks::CodegenPartition {
        codegen_partition(self.0)
    }
    fn link(
        &self,
        _: &mut hirsel_native_queries::roots::RootProducts,
        _: hirsel_native_queries::link_tasks::LinkExecution<'_>,
    ) -> Result<Vec<u8>, String> {
        Err("object-only compilation cannot execute Link".into())
    }
}

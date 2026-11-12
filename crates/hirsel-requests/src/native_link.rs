//! Host Link executor over a selected stored tool, pre-codegen kit snapshot and
//! ordinary archive references. The shared native planner owns launcher/glue,
//! manifest and ABI checks; this adapter owns neither compiler bodies nor caches.
use hirsel_engine_store::ResultStore;

use hirsel_native::Backend;
use hirsel_native::object_queries::capture::CaptureLimits;
use hirsel_native::object_queries::platform::CapturedPlatform;
use hirsel_native::object_queries::platform::record::PlatformSpec;
use hirsel_native::object_queries::runtime::RuntimeFiles;
use hirsel_native::object_queries::runtime::RuntimeLimits;
use hirsel_native_queries::compilation_tasks::CompilationBackend;
use hirsel_native_queries::link_tasks::CodegenPartition;
use hirsel_native_queries::link_tasks::LinkExecution;
use hirsel_native_queries::link_tasks::NativeLinkExecutor;
use hirsel_native_queries::roots::RootCapturedExecutableInputs;
use hirsel_native_queries::roots::RootProducts;
use hirsel_syntax::{ContentHash, DefPath};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The capability comes only from selected-input capture and closed target
/// resolution. Its original metadata reservation transfers into this executor.
pub(super) struct LinkInputs {
    pub platform: CapturedPlatform,
    pub emulator: Option<PathBuf>,
    pub timeout: Duration,
    pub runtime_limits: RuntimeLimits,
    pub limits: CaptureLimits,
    /// The selection whose platform record supplied `platform`; None when
    /// it was captured cold. A Link failure on a record's platform deletes
    /// the record and verifies fully before any output is kept.
    pub record: Option<PlatformSpec>,
}
pub(crate) struct FrozenPlatform {
    store: Arc<dyn ResultStore>,
    inputs: LinkInputs,
    entry: DefPath,
    backend: Backend,
    partition: hirsel_native::Partition,
    target: hirsel_native::TargetSpec,
    requested: Option<Arc<hirsel_run_config::ValidatedRun>>,
    linker_options: ContentHash,
    /// Set once a failed Link on a record's platform recaptured it.
    recovered: AtomicBool,
    _allowance: Arc<hirsel_engine_admission::MemoryReservation>,
}
impl FrozenPlatform {
    pub fn new(
        store: Arc<dyn ResultStore>,
        inputs: LinkInputs,
        entry: DefPath,
        backend: Backend,
        partition: hirsel_native::Partition,
        target: hirsel_native::TargetSpec,
        requested: Option<Arc<hirsel_run_config::ValidatedRun>>,
        allowance: impl Into<Arc<hirsel_engine_admission::MemoryReservation>>,
    ) -> Result<Arc<Self>, String> {
        let allowance = allowance.into();
        if inputs.limits.archive_bytes == 0 || inputs.limits.files == 0 {
            return Err("invalid selected native platform input bounds".into());
        }
        inputs
            .platform
            .runtime()
            .validate(store.as_ref(), &target, inputs.runtime_limits)?;
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Self>());
        weight.path(&entry);
        weight.add(target.triple.capacity());
        if let Some(emulator) = &inputs.emulator {
            weight.add(emulator.capacity());
        }
        let metadata = weight
            .bytes()
            .checked_add(inputs.platform.retained_metadata_bytes())
            .ok_or("selected native platform metadata overflow")?;
        if metadata > allowance.bytes() {
            return Err("selected native platform metadata exceeds admission".into());
        }
        let encoded = serde_json::to_vec(&(
            2u32,
            inputs.platform.identity(),
            target.digest(),
            backend.name(),
            &entry,
            requested.as_ref().map(|run| run.identity()),
        ))
        .map_err(|error| error.to_string())?;
        let linker_options =
            ContentHash::of_parts("hirsel.host.frozen-platform.v2", [encoded.as_slice()]);
        Ok(Arc::new(Self {
            store,
            inputs,
            entry,
            backend,
            partition,
            target,
            requested,
            linker_options,
            recovered: AtomicBool::new(false),
            _allowance: allowance,
        }))
    }
    pub fn identity(&self) -> ContentHash {
        self.linker_options
    }
    /// The captured platform's own identity.
    pub fn platform_identity(&self) -> ContentHash {
        self.inputs.platform.identity()
    }
    /// The emulator its executables launch under; None runs them on the host.
    pub fn emulator(&self) -> Option<&Path> {
        self.inputs.emulator.as_deref()
    }
    /// Run this for reopened Link hits too, before exposing any executable.
    pub fn validate_image(&self, image: &Path) -> Result<(), String> {
        self.validate_image_on(&self.inputs.platform, image)
    }
    fn validate_image_on(&self, platform: &CapturedPlatform, image: &Path) -> Result<(), String> {
        platform.validate_image(
            self.store.as_ref(),
            &self.target,
            image,
            self.inputs.emulator.as_deref(),
            self.inputs.timeout,
            self.inputs.runtime_limits,
        )
    }
    pub fn runtime_files(&self, image: &Path) -> Result<RuntimeFiles, String> {
        self.validate_image(image)?;
        self.inputs.platform.runtime().materialize(
            self.store.as_ref(),
            &self.target,
            self.inputs.runtime_limits,
        )
    }
    pub fn executable(&self, path: PathBuf) -> Result<hirsel_native::Runnable, String> {
        let runtime = self.runtime_files(&path)?;
        Ok(hirsel_native::Runnable::closed(
            path,
            runtime,
            self.inputs.emulator.clone(),
        ))
    }
    pub fn backend(
        self: &Arc<Self>,
        backend_version: ContentHash,
        target_features: ContentHash,
        profile_options: ContentHash,
        runtime_abi: ContentHash,
    ) -> CompilationBackend {
        CompilationBackend {
            backend: self.backend.name().into(),
            backend_version,
            target_features,
            profile_options,
            runtime_abi,
            linker_options: self.linker_options,
            runtime_archives: vec![self.inputs.platform.runtime_archive().content],
            foreign_inputs: self
                .inputs
                .platform
                .foreign()
                .iter()
                .map(|input| input.content)
                .collect(),
            executor: self.clone(),
        }
    }
}
impl NativeLinkExecutor for FrozenPlatform {
    fn codegen_partition(&self) -> CodegenPartition {
        super::native_platform::codegen_partition(self.partition)
    }
    fn link(&self, native: &mut RootProducts, input: LinkExecution<'_>) -> Result<Vec<u8>, String> {
        if input.key.linker_options != self.linker_options
            || input.inputs.runtime_archives != [self.inputs.platform.runtime_archive().content]
            || !input.inputs.foreign_inputs.iter().copied().eq(self
                .inputs
                .platform
                .foreign()
                .iter()
                .map(|input| input.content))
        {
            return Err("native Link received another selected platform".into());
        }
        let failed = match self.link_on(&self.inputs.platform, native, &input) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => error,
        };
        let Some(record) = &self.inputs.record else {
            return Err(failed);
        };
        if self.recovered.swap(true, Ordering::SeqCst) {
            return Err(failed);
        }
        // Rule 5: delete the record and verify fully; run again only on the
        // recorded platform, else withhold.
        let fresh = record.recover(
            self.store.as_ref(),
            self.inputs.platform.identity(),
            &format!("Link failed: {failed}"),
        )?;
        self.link_on(&fresh, native, &input)
    }
}
impl FrozenPlatform {
    fn link_on(
        &self,
        platform: &CapturedPlatform,
        native: &mut RootProducts,
        input: &LinkExecution<'_>,
    ) -> Result<Vec<u8>, String> {
        let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
        let tests = !native.test_entries(input.root)?.is_empty();
        let inputs = RootCapturedExecutableInputs {
            entry: &self.entry,
            backend: self.backend,
            objects: input.objects,
            directory: scratch.path(),
            requested: self.requested.as_deref(),
            standalone: self.requested.is_none(),
            platform: hirsel_native::CapturedExecutableLinkInputs {
                store: self.store.as_ref(),
                platform,
                loader: None,
            },
        };
        let plan = if tests {
            native.plan_captured_tests(input.root, input.ready, inputs)
        } else {
            native.plan_captured_executable(input.root, input.ready, inputs)
        }?;
        // Foreign code compiled against another data ABI is PROG-4's refusal.
        let refused = |error: String| plan.refusal(&error).unwrap_or(error);
        let capture = plan
            .capture(self.store.as_ref(), platform, self.inputs.limits)
            .map_err(refused)?;
        capture.validate(self.store.as_ref())?;
        let executable = scratch.path().join("program");
        capture
            .link(self.store.as_ref(), &executable)
            .map_err(refused)?;
        self.validate_image_on(platform, &executable)?;
        let limit = self.inputs.limits.archive_bytes;
        let mut bytes = Vec::new();
        std::fs::File::open(executable)
            .map_err(|error| error.to_string())?
            .take(
                limit
                    .checked_add(1)
                    .ok_or("native executable admission overflow")?,
            )
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > limit {
            return Err("native executable exceeds admitted Link bytes".into());
        }
        Ok(bytes)
    }
}

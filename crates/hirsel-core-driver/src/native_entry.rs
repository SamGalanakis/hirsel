//! The fresh link object: C-ABI startup, entry binding and current source data.
//! Both program engines use this one Object/Cranelift writer on both targets.
use crate::native::{Backend, EntryAbi, NativeOptions, NativeStop, grant_kind};
use cranelift_codegen::{
    ir,
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hirsel_core_ir::source::SourceMap;
use hirsel_core_layout::{Fixed, Layout, Target};
use hirsel_cranelift::unwind::Unwind;
use serde::Serialize;
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;

mod adapter;
mod code;
mod data;
mod embed;
pub(crate) use embed::APPLE_MANIFEST_SECTION;
mod foreign;
pub(crate) mod launch_config;
mod report;
mod sites;
mod start;
use code::{Code, I8, I32, I64};
pub(crate) use data::encode_module_sources;
pub(crate) use data::source_site;
pub(crate) use sites::{Referenced, check_linked_sites, live_site_words, runtime_sources};
type Result<T> = std::result::Result<T, String>;
/// The link's members and their keys (`runtime_sources`).
type Inputs<'a> = (
    &'a [std::sync::Arc<[u8]>],
    &'a [(hirsel_core_ids::ContentHash, u64)],
);
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
struct Requirement<'a> {
    kind: u8,
    index: u64,
    name: &'a str,
}

/// Whether a program reaching these definitions reads the place-only launch
/// words: only the toolchain's runner package's launch reads them (launch.md,
/// "Private prefix"), so any other program receives them in `Args`.
/// `supplied_by_toolchain` says where each package came from; a package that
/// only names itself `runner@1` is not the runner.
pub(crate) fn reaches_place<'d>(
    definitions: impl IntoIterator<Item = &'d hirsel_core_ids::DefPath>,
    supplied_by_toolchain: impl Fn(&hirsel_core_ids::PackageId) -> bool,
) -> bool {
    definitions.into_iter().any(|d| {
        let package = &d.0.package;
        package.is_toolchain_runner(supplied_by_toolchain(package))
    })
}

/// A cheap link-key input. Machine code is emitted only after a link-cache miss.
#[derive(Serialize)]
pub(crate) struct Plan<'a> {
    symbol: String,
    identity: String,
    #[serde(skip)]
    manifest: Option<Vec<u8>>,
    layout: &'a Fixed,
    exit_offset: Option<u64>,
    /// PROG-3: the encoded text program of a non-`Exit` error.
    error_text: Option<Vec<u8>>,
    parameters: Vec<Requirement<'a>>,
    cleanup: Option<String>,
    launch_digest_unused: bool,
    release_sites: &'a [hirsel_core_ir::source::ReleaseSite],
    pub(crate) release_debug_sites: &'a [hirsel_core_ir::source::ReleaseSite],
    pub(crate) remarks: &'a [hirsel_core_ir::source::CodegenRemark],
    root_limit: u64,
    grant_mask: u64,
    net_rules: &'a [hirsel_core_net::GrantRule],
    unix_roots: &'a std::collections::BTreeMap<String, String>,
    env_patterns: Option<String>,
    secret_grants: Vec<Option<&'a hirsel_core_net::SecretGrant>>,
    cred_grants: Vec<Option<&'a hirsel_core_net::CredGrant>>,
    directories: &'a [String],
    charge_report: Option<Vec<u8>>,
    recording: bool,
    /// A coverage executable's entry maps its flags onto the raw file
    /// (`docs/coverage.md`, "The raw file"). Absent from a plain plan's
    /// encoding, which is the encoding it had before coverage.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    coverage: bool,
    /// The program reaches the toolchain's `runner` package, so the entry reads the
    /// place-only launch words (launch.md, "Private prefix"). Absent from a
    /// plain plan's encoding, which keeps every other program's link key.
    #[serde(skip_serializing_if = "Option::is_none")]
    place_backend: Option<Backend>,
    #[serde(skip)]
    backend: Backend,
    /// `store@1/store/verify_teardown` symbol registered at `@test` startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    store_teardown_fn: Option<String>,
    replay: Option<&'a crate::native::ReplayPlan>,
    foreign: Vec<foreign::Function<'a>>,
    /// The contained modules the foreign functions call into (FFI-15).
    /// Absent from a plan with none, which keeps every native link key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    contained: Vec<foreign::Contained<'a>>,
    exec: Vec<(String, Vec<u8>, [u8; 32])>,
}
/// Every link-plan input the environment does not cover, canonically: the
/// root limit, grant mask, explicit Net rules, ordered directories, charge-report
/// path, recording flag and optional replay plan. The plan's other fields are the entry's:
/// `symbol` is the entry's
/// identity, `layout`, `exit_offset`, `error_text` and `parameters` are read from the entry
/// body (its record's address and the declaration part), `cleanup` is the
/// cleanup instance's identity. The request's source map is the link's, and
/// the executable key takes its digest.
pub(crate) fn policy(
    options: &NativeOptions,
) -> (
    u64,
    u64,
    (
        &[hirsel_core_net::GrantRule],
        &std::collections::BTreeMap<String, String>,
    ),
    (
        &[hirsel_core_net::SecretGrant],
        &[hirsel_core_net::CredGrant],
    ),
    Option<&hirsel_core_net::EnvPatterns>,
    &[String],
    &std::collections::BTreeSet<String>,
    &[crate::exec_launch::Binding],
    Option<&[u8]>,
    bool,
    Option<&crate::native::ReplayPlan>,
) {
    (
        options.root_limit,
        options.grant_mask,
        (&options.net_rules, &options.unix_roots),
        (&options.secret_grants, &options.cred_grants),
        options.env_patterns.as_ref(),
        &options.directories,
        &options.foreign_grants,
        &options.exec_grants,
        options
            .logical_charge_report
            .as_ref()
            .map(|p| p.as_os_str().as_bytes()),
        options.recording,
        options.replay.as_ref(),
    )
}
impl<'a> Plan<'a> {
    /// A recording or replay entry of a program that can spawn units: it
    /// hands the runtime the link's descriptor index and spawn table.
    pub(crate) fn recording_units(&self, spawns_units: bool) -> bool {
        (self.recording || self.replay.is_some()) && spawns_units
    }

    /// The link's descriptor names and spawn table for
    /// [`Self::recording_units`], else none.
    pub(crate) fn descriptors(
        &self,
        spawns_units: bool,
        members: &[std::sync::Arc<[u8]>],
        keys: &[(hirsel_core_ids::ContentHash, u64)],
    ) -> std::io::Result<sites::Descriptors> {
        sites::descriptors(self.recording_units(spawns_units), members, keys)
    }
    pub(crate) fn new(
        symbol: String,
        identity: String,
        binding: &'a EntryAbi,
        cleanup: Option<String>,
        launch_digest_unused: bool,
        options: &'a NativeOptions,
    ) -> std::result::Result<Self, NativeStop> {
        let Layout::Fixed(layout) = &binding.recipe.layout else {
            return Err(NativeStop::Entry("Result needs fixed layout".into()));
        };
        crate::native::recipe_grants::refuse_unmatched_grants(
            &identity,
            &binding.parameters,
            options,
        )?;
        let mut ordinals = [0u64; 15];
        let mut grant_mask = options.grant_mask;
        let parameters = binding
            .parameters
            .iter()
            .map(|p| {
                let kind = if p.foreign_interface().is_some() {
                    let kind = grant_kind("Foreign").unwrap();
                    grant_mask |= 1 << kind;
                    kind
                } else {
                    p.ty.strip_prefix("core@1/core/")
                        .and_then(grant_kind)
                        .ok_or_else(|| {
                            NativeStop::Entry(format!("unsupported provider parameter {}", p.ty))
                        })?
                };
                let index = ordinals[kind as usize];
                ordinals[kind as usize] += 1;
                Ok(Requirement {
                    kind,
                    index,
                    name: &p.name,
                })
            })
            .collect::<std::result::Result<Vec<_>, NativeStop>>()?;
        let secret_kind = grant_kind("Secret").unwrap();
        let secret_grants: Vec<_> = parameters
            .iter()
            .filter(|p| p.kind == secret_kind)
            .map(|p| options.secret_grants.iter().find(|g| g.parameter == p.name))
            .collect();
        if secret_grants.iter().flatten().count() != options.secret_grants.len() {
            return Err(NativeStop::Entry(
                "Secret grants must name distinct Secret parameters".into(),
            ));
        }
        let cred_kind = grant_kind("Cred").unwrap();
        let cred_grants: Vec<_> = parameters
            .iter()
            .filter(|p| p.kind == cred_kind)
            .map(|p| options.cred_grants.iter().find(|g| g.parameter == p.name))
            .collect();
        if cred_grants.iter().flatten().count() != options.cred_grants.len() {
            return Err(NativeStop::Entry(
                "Cred grants must name distinct Cred parameters".into(),
            ));
        }
        if !options.cred_grants.is_empty() && options.replay.is_some() {
            return Err(NativeStop::Entry(
                "replay takes Cred grants from its tape".into(),
            ));
        }
        if !options.exec_grants.is_empty() && options.replay.is_some() {
            return Err(NativeStop::Entry(
                "replay takes Exec grants from its tape".into(),
            ));
        }
        let exec = options
            .exec_grants
            .iter()
            .map(|grant| {
                Ok((
                    grant.request.text(),
                    grant
                        .workdir
                        .as_ref()
                        .map(|path| path.as_os_str().as_bytes().to_vec())
                        .unwrap_or_default(),
                    grant.digest()?,
                ))
            })
            .collect::<std::result::Result<_, NativeStop>>()?;
        Ok(Self {
            manifest: None,
            symbol,
            identity,
            layout,
            exit_offset: binding.exit_offset,
            error_text: binding.error_text.as_ref().map(|show| show.encode()),
            parameters,
            launch_digest_unused: launch_digest_unused && cleanup.is_none(),
            release_sites: &[],
            release_debug_sites: &[],
            remarks: &[],
            cleanup,
            root_limit: options.root_limit,
            grant_mask,
            net_rules: &options.net_rules,
            unix_roots: &options.unix_roots,
            env_patterns: options
                .env_patterns
                .as_ref()
                .map(hirsel_core_net::EnvPatterns::text),
            secret_grants,
            cred_grants,
            directories: &options.directories,
            recording: options.recording,
            coverage: options.coverage.is_some(),
            place_backend: None,
            backend: options.backend,
            store_teardown_fn: None,
            replay: options.replay.as_ref(),
            foreign: vec![],
            contained: vec![],
            exec,
            charge_report: options
                .logical_charge_report
                .as_ref()
                .map(|p| p.as_os_str().as_bytes().to_vec()),
        })
    }

    pub(crate) fn with_foreign(
        mut self,
        leaves: impl IntoIterator<Item = &'a hirsel_core_ir::release_recipe::ForeignLeaf>,
        archives: Option<&'a crate::foreign_archive::LinkArchives>,
    ) -> Self {
        (self.foreign, self.contained) = foreign::Function::collect(leaves, archives);
        self
    }

    /// Marks a program that reaches the toolchain's `runner` package.
    pub(crate) fn with_place(mut self, place: bool) -> Self {
        self.place_backend = place.then_some(self.backend);
        self
    }

    /// Registers `store@1/store/verify_teardown` in `@test` builds when present.
    pub(crate) fn with_store_teardown<'b>(
        mut self,
        enabled: bool,
        defs: impl IntoIterator<Item = &'b hirsel_core_ir::Identity>,
    ) -> Self {
        self.store_teardown_fn = enabled
            .then(|| {
                defs.into_iter()
                    .find(|i| {
                        i.definition.to_string() == hirsel_core_ir::law::STORE_VERIFY_TEARDOWN
                    })
                    .map(|i| i.symbol())
            })
            .flatten();
        self
    }

    pub(crate) fn with_release_sites(
        mut self,
        sites: &'a [hirsel_core_ir::source::ReleaseSite],
        debug_sites: &'a [hirsel_core_ir::source::ReleaseSite],
        remarks: &'a [hirsel_core_ir::source::CodegenRemark],
    ) -> Self {
        self.release_sites = sites;
        self.release_debug_sites = debug_sites;
        self.remarks = remarks;
        self
    }

    pub(crate) fn with_manifest(mut self, manifest: Option<Vec<u8>>) -> Self {
        self.manifest = manifest;
        self
    }

    /// The entry object of the executable. It also defines
    /// `hirsel_code_revision`, the entry's code revision, which a Run tape's
    /// header carries (REPLAY-SPEC v2.3 R10): a rebuild or another engine keeps
    /// it, a changed reachable body changes it. And it defines each `revisions`
    /// row's carrier, a Bytes `{bytes, 32, 0}` over the revision, under its
    /// symbol: these override the empty carriers the units define (PROG-9).
    /// A recording or replay entry also holds `build`, the digest of the
    /// build without its launch policy: a Run tape records it and a replay
    /// whose own differs writes `replay: build differs`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit(
        &self,
        target: &str,
        sources: &SourceMap,
        source_identity: hirsel_core_ids::ContentHash,
        revision: &[u8; 32],
        build: &[u8; 32],
        revisions: &[(String, [u8; 32])],
        (live_sites, spawns_units, (members, keys)): (Option<&BTreeSet<u64>>, bool, Inputs<'_>),
    ) -> Result<Vec<u8>> {
        let target = Target::from_triple(target)
            .ok_or_else(|| error(format!("{target} is not a Hirsel target")))?;
        let format = target.object_format();
        let mut object = object(target)?;
        let mut unwind = Unwind::new(object.isa()).map_err(error)?;
        let waits = foreign::emit(
            &mut object,
            &mut unwind,
            target.c_abi(),
            &self.foreign,
            &self.contained,
        )?;
        let data = data::write(&mut object, format, self, sources, live_sites)?;
        let revision_data = data::bytes(
            &mut object,
            Some("hirsel_code_revision"),
            revision.to_vec(),
            8,
        )?;
        for (symbol, revision) in revisions {
            data::carrier(&mut object, symbol, revision)?;
        }
        let build = if self.recording || self.replay.is_some() {
            Some(data::bytes(&mut object, None, build.to_vec(), 8)?)
        } else {
            None
        };
        let descriptors = if self.recording_units(spawns_units) {
            let link = self
                .descriptors(spawns_units, members, keys)
                .map_err(error)?;
            Some((
                data::descriptors(&mut object, &link.names)?,
                data::spawns(&mut object, &link.spawns)?,
            ))
        } else {
            None
        };
        let signature = signature(&object, &[I64, I64, I64], Some(I8));
        let entry = object
            .declare_function("__hirsel_entry", Linkage::Local, &signature)
            .map_err(error)?;
        adapter::emit(&mut object, &mut unwind, self, entry, signature)?;
        if let Some(manifest) = &self.manifest {
            embed::emit(
                &mut object,
                &mut unwind,
                target,
                self,
                &data,
                entry,
                revision_data,
                manifest,
                waits,
            )?;
        } else {
            let launch = launch_config::emit(&mut object, format, self.place_backend, revision)?;
            start::emit(
                &mut object,
                &mut unwind,
                target,
                self,
                &data,
                entry,
                waits,
                spawns_units,
                build,
                descriptors,
                launch,
            )?;
        }
        let mut product = object.finish();
        if self.coverage && self.manifest.is_none() {
            // Bounds have literal linker names. ObjectModule would otherwise
            // give them the C symbol prefix on Mach-O.
            for name in hirsel_core_ir::coverage::bounds(format) {
                let id = product
                    .object
                    .symbol_id(name.as_bytes())
                    .expect("coverage bound");
                product.object.symbol_mut(id).name = name.as_bytes().to_vec();
            }
        }
        unwind.write(&mut product, format).map_err(error)?;
        crate::native_debug::add_source_note(&mut product.object, source_identity);
        sites::define(&mut product.object, sources)?;
        sites::define_release(&mut product.object, sources, self.release_sites)?;
        product.object.write().map_err(error)
    }
}

impl Plan<'_> {
    /// The entry function's symbol.
    pub(crate) fn symbol(&self) -> &str {
        &self.symbol
    }

    /// The digest of the entry's lowered signature: its result's layout, where an
    /// `Exit` status sits in it, and its provider parameters.
    pub(crate) fn signature(&self) -> std::io::Result<hirsel_core_ids::ContentHash> {
        crate::native::key(
            "hirsel.mod-entry-signature.v1",
            &(self.layout, self.exit_offset, &self.parameters),
        )
    }

    /// What a `.hirsel-mod` link takes in place of an entry object: the site words'
    /// absolute symbols and each `revisions` row's carrier, as [`Plan::emit`] defines
    /// them, and no launch code.
    pub(crate) fn emit_module_support(
        &self,
        target: Target,
        sources: &SourceMap,
        revisions: &[(String, [u8; 32])],
    ) -> Result<Vec<u8>> {
        let mut object = object(target)?;
        for (symbol, revision) in revisions {
            data::carrier(&mut object, symbol, revision)?;
        }
        let mut product = object.finish();
        sites::define(&mut product.object, sources)?;
        sites::define_release(&mut product.object, sources, self.release_sites)?;
        product.object.write().map_err(error)
    }
}

fn object(target: Target) -> Result<ObjectModule> {
    let page = target.page_log2().to_string();
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "true"),
        ("enable_verifier", "false"),
        ("opt_level", "none"),
        ("regalloc_algorithm", "single_pass"),
        ("enable_probestack", "true"),
        ("probestack_strategy", "inline"),
        ("probestack_size_log2", page.as_str()),
    ] {
        flags.set(name, value).map_err(error)?;
    }
    // The launcher must reach the CPU check even on a pre-v2 x86-64 host.
    let isa = cranelift_codegen::isa::lookup_by_name(target.cranelift_triple())
        .map_err(error)?
        .finish(settings::Flags::new(flags))
        .map_err(error)?;
    let mut builder = ObjectBuilder::new(
        isa,
        b"hirsel-link".to_vec(),
        cranelift_module::default_libcall_names(),
    )
    .map_err(error)?;
    // A revision carrier is live only through a surviving reference.
    builder.per_data_object_section(true);
    Ok(ObjectModule::new(builder))
}

fn signature(
    object: &ObjectModule,
    parameters: &[ir::Type],
    result: Option<ir::Type>,
) -> ir::Signature {
    let mut signature = object.make_signature();
    signature
        .params
        .extend(parameters.iter().map(|&ty| ir::AbiParam::new(ty)));
    signature.returns.extend(result.map(ir::AbiParam::new));
    signature
}

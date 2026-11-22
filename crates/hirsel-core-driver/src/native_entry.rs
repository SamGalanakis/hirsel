//! The fresh link object: C-ABI startup, entry binding and current source data.
//! Both program engines use this one Object/Cranelift writer on both targets.
use crate::native::{EntryAbi, NativeOptions, NativeStop, grant_kind};
use cranelift_codegen::{
    ir,
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hirsel_core_ir::source::SourceMap;
use hirsel_core_layout::{Fixed, Layout};
use hirsel_cranelift::unwind::Unwind;
use serde::Serialize;
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;

mod adapter;
mod code;
mod data;
mod foreign;
mod report;
mod sites;
mod start;
use code::{Code, I8, I32, I64};
pub(crate) use data::source_site;
pub(crate) use sites::{check_linked_sites, live_site_words, retain_referenced};
type Result<T> = std::result::Result<T, String>;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
struct Requirement<'a> {
    kind: u8,
    index: u64,
    name: &'a str,
}

/// A cheap link-key input. Machine code is emitted only after a link-cache miss.
#[derive(Serialize)]
pub(crate) struct Plan<'a> {
    symbol: String,
    identity: String,
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
    directories: &'a [String],
    charge_report: Option<Vec<u8>>,
    recording: bool,
    replay: Option<&'a crate::native::ReplayPlan>,
    foreign: Vec<foreign::Function<'a>>,
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
    &[hirsel_core_net::SecretGrant],
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
        &options.secret_grants,
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
        let mut ordinals = [0u64; 14];
        let mut grant_mask = options.grant_mask;
        let parameters = binding
            .parameters
            .iter()
            .map(|p| {
                let kind = if let Some(interface) =
                    p.ty.strip_prefix("core@1/core/Foreign[")
                        .and_then(|ty| ty.strip_suffix(']'))
                {
                    if !options.foreign_grants.contains(interface) {
                        return Err(NativeStop::Entry(format!(
                            "not granted: Foreign[{interface}]"
                        )));
                    }
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
        if let Some(grant) = options
            .exec_grants
            .get(ordinals[grant_kind("Exec").unwrap() as usize] as usize)
        {
            return Err(NativeStop::Entry(format!(
                "--grant Exec={} has no matching parameter of {identity}",
                grant.request.text()
            )));
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
            directories: &options.directories,
            recording: options.recording,
            replay: options.replay.as_ref(),
            foreign: vec![],
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
    ) -> Self {
        self.foreign = foreign::Function::collect(leaves);
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

    /// The entry object of the executable. It also defines
    /// `hirsel_code_revision`, the entry's code revision, which a Run tape's
    /// header carries (REPLAY-SPEC v2.3 R10): a rebuild or another engine keeps
    /// it, a changed reachable body changes it. And it defines each `revisions`
    /// row's carrier, a Bytes `{bytes, 32, 0}` over the revision, under its
    /// symbol: these override the empty carriers the units define (PROG-9).
    pub(crate) fn emit(
        &self,
        target: &str,
        sources: &SourceMap,
        source_identity: hirsel_core_ids::ContentHash,
        revision: &[u8; 32],
        revisions: &[(String, [u8; 32])],
        (live_sites, spawns_units): (Option<&BTreeSet<u64>>, bool),
    ) -> Result<Vec<u8>> {
        let mut object = object(target)?;
        let mut unwind = Unwind::new(object.isa()).map_err(error)?;
        let waits = foreign::emit(&mut object, &mut unwind, &self.foreign)?;
        let data = data::write(&mut object, self, sources, live_sites)?;
        data::bytes(
            &mut object,
            Some("hirsel_code_revision"),
            revision.to_vec(),
            8,
        )?;
        for (symbol, revision) in revisions {
            data::carrier(&mut object, symbol, revision)?;
        }
        let signature = signature(&object, &[I64, I64, I64], Some(I8));
        let entry = object
            .declare_function("__hirsel_entry", Linkage::Local, &signature)
            .map_err(error)?;
        adapter::emit(&mut object, &mut unwind, self, entry, signature)?;
        start::emit(
            &mut object,
            &mut unwind,
            self,
            &data,
            entry,
            waits,
            spawns_units,
        )?;
        let mut product = object.finish();
        unwind.write(&mut product).map_err(error)?;
        crate::native_debug::add_source_note(&mut product.object, source_identity);
        sites::define(&mut product.object, sources)?;
        sites::define_release(&mut product.object, sources, self.release_sites)?;
        product.object.write().map_err(error)
    }
}

fn object(target: &str) -> Result<ObjectModule> {
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "true"),
        ("enable_verifier", "false"),
        ("opt_level", "none"),
        ("regalloc_algorithm", "single_pass"),
        ("enable_probestack", "true"),
        ("probestack_strategy", "inline"),
        ("probestack_size_log2", "12"),
    ] {
        flags.set(name, value).map_err(error)?;
    }
    // The launcher must reach the CPU check even on a pre-v2 x86-64 host.
    let isa = cranelift_codegen::isa::lookup_by_name(target)
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

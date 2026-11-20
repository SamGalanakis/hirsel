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
use serde::Serialize;
use std::os::unix::ffi::OsStrExt;

mod adapter;
mod code;
mod data;
mod report;
mod sites;
mod start;
use code::{Code, I8, I32, I64};
pub(crate) use sites::{check_linked_sites, retain_referenced};
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
    parameters: Vec<Requirement<'a>>,
    cleanup: Option<String>,
    launch_digest_unused: bool,
    release_sites: &'a [hirsel_core_ir::source::ReleaseSite],
    root_limit: u64,
    grant_mask: u64,
    net_rules: &'a [hirsel_core_net::GrantRule],
    secret_grants: Vec<Option<&'a hirsel_core_net::SecretGrant>>,
    directories: &'a [String],
    charge_report: Option<Vec<u8>>,
    tape: Option<&'a crate::native::RunTape>,
}
/// Every link-plan input the environment does not cover, canonically: the
/// root limit, grant mask, explicit Net rules, ordered directories, charge-report
/// path and optional Run tape mode/path. The plan's other fields are the entry's:
/// `symbol` is the entry's
/// identity, `layout`, `exit_offset` and `parameters` are read from the entry
/// body (its record's address and the declaration part), `cleanup` is the
/// cleanup instance's identity. The request's source map is the link's, and
/// the executable key takes its digest.
pub(crate) fn policy(
    options: &NativeOptions,
) -> (
    u64,
    u64,
    &[hirsel_core_net::GrantRule],
    &[hirsel_core_net::SecretGrant],
    &[String],
    Option<&[u8]>,
    Option<&crate::native::RunTape>,
) {
    (
        options.root_limit,
        options.grant_mask,
        &options.net_rules,
        &options.secret_grants,
        &options.directories,
        options
            .logical_charge_report
            .as_ref()
            .map(|p| p.as_os_str().as_bytes()),
        options.tape.as_ref(),
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
        let mut ordinals = [0u64; 12];
        let parameters = binding
            .parameters
            .iter()
            .map(|p| {
                let kind =
                    p.ty.strip_prefix("core@1/core/")
                        .and_then(grant_kind)
                        .ok_or_else(|| {
                            NativeStop::Entry(format!("unsupported provider parameter {}", p.ty))
                        })?;
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
        Ok(Self {
            symbol,
            identity,
            layout,
            exit_offset: binding.exit_offset,
            parameters,
            launch_digest_unused: launch_digest_unused && cleanup.is_none(),
            release_sites: &[],
            cleanup,
            root_limit: options.root_limit,
            grant_mask: options.grant_mask,
            net_rules: &options.net_rules,
            secret_grants,
            directories: &options.directories,
            tape: options.tape.as_ref(),
            charge_report: options
                .logical_charge_report
                .as_ref()
                .map(|p| p.as_os_str().as_bytes().to_vec()),
        })
    }

    pub(crate) fn with_release_sites(
        mut self,
        sites: &'a [hirsel_core_ir::source::ReleaseSite],
    ) -> Self {
        self.release_sites = sites;
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
        revision: &[u8; 32],
        revisions: &[(String, [u8; 32])],
    ) -> Result<Vec<u8>> {
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
        let mut object = ObjectModule::new(
            ObjectBuilder::new(
                isa,
                b"hirsel-link".to_vec(),
                cranelift_module::default_libcall_names(),
            )
            .map_err(error)?,
        );
        let data = data::write(&mut object, self, sources)?;
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
        adapter::emit(&mut object, self, entry, signature)?;
        start::emit(&mut object, self, &data, entry)?;
        let mut product = object.finish();
        sites::define(&mut product.object, sources)?;
        sites::define_release(&mut product.object, sources, self.release_sites)?;
        product.object.write().map_err(error)
    }
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

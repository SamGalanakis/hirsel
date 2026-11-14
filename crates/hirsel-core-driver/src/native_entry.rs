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
mod start;
use code::{Code, I8, I32, I64};
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
    layout: &'a Fixed,
    exit_offset: Option<u64>,
    parameters: Vec<Requirement<'a>>,
    cleanup: Option<String>,
    root_limit: u64,
    grant_mask: u64,
    directories: &'a [String],
    charge_report: Option<Vec<u8>>,
    sources: &'a SourceMap,
}
impl<'a> Plan<'a> {
    pub(crate) fn new(
        symbol: String,
        binding: &'a EntryAbi,
        options: &'a NativeOptions,
        sources: &'a SourceMap,
    ) -> std::result::Result<Self, NativeStop> {
        let Layout::Fixed(layout) = &binding.recipe.layout else {
            return Err(NativeStop::Entry("Result needs fixed layout".into()));
        };
        let mut ordinals = [0u64; 11];
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
            .collect::<std::result::Result<_, NativeStop>>()?;
        Ok(Self {
            symbol,
            layout,
            exit_offset: binding.exit_offset,
            parameters,
            cleanup: binding
                .cleanup
                .as_ref()
                .map(|m| hirsel_cranelift::core::symbol(m.instances.last().unwrap())),
            root_limit: options.root_limit,
            grant_mask: options.grant_mask,
            directories: &options.directories,
            charge_report: options
                .logical_charge_report
                .as_ref()
                .map(|p| p.as_os_str().as_bytes().to_vec()),
            sources,
        })
    }

    pub(crate) fn emit(&self, target: &str) -> Result<Vec<u8>> {
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
        let data = data::write(&mut object, self)?;
        let signature = signature(&object, &[I64, I64, I64], Some(I8));
        let entry = object
            .declare_function("__hirsel_entry", Linkage::Local, &signature)
            .map_err(error)?;
        adapter::emit(&mut object, self, entry, signature)?;
        start::emit(&mut object, self, &data, entry)?;
        object.finish().object.write().map_err(error)
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

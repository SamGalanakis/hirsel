//! CoreIR emission.
//!
//! Scalars, checked failure and explicit cleanup edges use the frozen CoreIR
//! function ABI. Native memory calls use the separately declared core runtime C
//! ABI.

use cranelift_codegen::ir::{self, AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hirsel_core_ir::{self as c, *};
use hirsel_core_layout::{CAbi, Target};

/// Frozen native memory layout, shared with hirsel-core-rt.
pub mod memory_layout {
    include!("../../hirsel-core-rt/src/abi_layout.rs");
}

mod abi;
pub use abi::{Signature as NativeSignature, c_param};
mod calls;
mod case;
mod coverage;
mod debug;
mod drop;
mod failure;
mod fast;
mod function;
mod growth;
mod law;
mod memory;
mod place;
pub mod profile;
mod scalar;
mod simd;
mod sites;
mod task_entry;
mod tasks;
#[cfg(test)]
mod tests;

/// Why a CoreIR module could not be emitted.
#[derive(Debug)]
pub enum Error {
    Target(String),
    Unsupported(String),
    Backend(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(s) | Self::Unsupported(s) | Self::Backend(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

/// Whether this engine implements the requested target's native ABI and
/// object format: every Hirsel target, ELF and Mach-O alike.
pub fn supports_target(triple: &str) -> bool {
    Target::from_triple(triple).is_some()
}

/// Target and optimization policy for CoreIR emission.
pub struct Options<'a> {
    pub target: &'a str,
    pub optimize: bool,
    /// Emit source-independent line and subprogram rows for link-time binding.
    pub debug_lines: bool,
}

/// A relocatable object. Its symbols use CoreIR instance identities.
pub use c::source::NativeObject as Object;

fn unsupported(message: impl Into<String>) -> Error {
    Error::Unsupported(message.into())
}
fn error(e: impl std::fmt::Display) -> Error {
    Error::Backend(e.to_string())
}

fn ty(t: Ty) -> ir::Type {
    match t {
        Ty::Ptr | Ty::Scalar(Scalar::I64 | Scalar::U64) => types::I64,
        Ty::Scalar(Scalar::Bool | Scalar::I8 | Scalar::U8) => types::I8,
        Ty::Scalar(Scalar::I16 | Scalar::U16) => types::I16,
        Ty::Scalar(Scalar::Char | Scalar::I32 | Scalar::U32) => types::I32,
        Ty::Scalar(Scalar::F32) => types::F32,
        Ty::Scalar(Scalar::F64) => types::F64,
        Ty::Vector(_) => types::I8X16,
    }
}

pub(super) struct Compilation {
    codegen: cranelift_codegen::Context,
    frontend: FunctionBuilderContext,
}

pub(super) struct Tables<'a> {
    input: &'a c::Module,
    layouts: &'a [hirsel_core_layout::RecipeRow],
    signatures: &'a [abi::Signature],
    call_layouts: &'a c::native_layout::CallLayouts,
    functions: &'a [cranelift_module::FuncId],
    statics: &'a [cranelift_module::DataId],
    /// By function, its static descriptor when a body of this unit describes it.
    descriptors: &'a [Option<cranelift_module::DataId>],
    glues: &'a drop::Glues,
    task_entries: &'a task_entry::Glues,
    trace_drops: bool,
    observe_charge: bool,
    debug: bool,
    c_abi: CAbi,
    unwind: std::cell::RefCell<crate::unwind::Unwind>,
    site_symbols: std::cell::RefCell<std::collections::HashMap<ItemId, cranelift_module::DataId>>,
    site_constants: std::cell::RefCell<sites::Constants>,
    runtimes: std::cell::RefCell<calls::Imports>,
}

impl Tables<'_> {
    fn define(
        &self,
        observer: &impl profile::Observer,
        object: &mut ObjectModule,
        function: cranelift_module::FuncId,
        context: &mut cranelift_codegen::Context,
    ) -> Result<()> {
        observer.define(object, function, context).map_err(error)?;
        self.unwind
            .borrow_mut()
            .record(object.isa(), function, context)
    }
}

fn isa(target: Target, optimize: bool) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    let refuse = |message: String| Error::Target(format!("{}: {message}", target.triple()));
    // Inline probes touch every page of a large frame; the stride is the page.
    let probe = target.page_log2().to_string();
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "true"),
        ("enable_verifier", "false"),
        // A scalar pair and the internal failure status can occupy three GPRs.
        ("enable_llvm_abi_extensions", "true"),
        ("opt_level", if optimize { "speed" } else { "none" }),
        // Step slow paths split otherwise sequential live ranges. The single-
        // pass allocator gives each spilled value its own permanent stack home;
        // backtracking reuses those homes across disjoint ranges in dev too.
        ("regalloc_algorithm", "backtracking"),
    ]
    .into_iter()
    .chain([
        ("enable_probestack", "true"),
        ("probestack_strategy", "inline"),
        ("probestack_size_log2", probe.as_str()),
    ]) {
        flags.set(name, value).map_err(|e| refuse(e.to_string()))?;
    }
    let mut isa = cranelift_codegen::isa::lookup_by_name(target.cranelift_triple())
        .map_err(|e| refuse(e.to_string()))?;
    match target.arch() {
        hirsel_core_layout::Arch::X86_64 => {
            for feature in hirsel_core_layout::isa::X86_64_BASE.cranelift_features() {
                isa.enable(feature).map_err(|e| refuse(e.to_string()))?;
            }
        }
        hirsel_core_layout::Arch::Aarch64 => {
            if let Some(apple) = target.apple() {
                for feature in apple.cranelift_features {
                    isa.enable(feature).map_err(|e| refuse(e.to_string()))?;
                }
            }
        }
    }
    isa.finish(settings::Flags::new(flags))
        .map_err(|e| refuse(e.to_string()))
}

/// Emit scalar-only CoreIR directly to a Linux x86-64 or AArch64 C-ABI object. Modules with
/// declared types use [`emit_object_with_layouts`] and producer layout recipes. CoreIR is a trusted compiler product;
/// this entry does not invoke its well-formedness checker or the old pipeline.
pub fn emit_object(
    input: &c::Module,
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
) -> Result<Object> {
    emit_object_with_layouts(input, &[], options, object_key)
}

/// Emit with the producer's already-computed layout recipes. The recipe table
/// corresponds to `input.types`; the backend reads it without recomputing layouts.
pub fn emit_object_with_layouts(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
) -> Result<Object> {
    emit(
        input,
        &input.link_symbols(),
        layouts,
        options,
        object_key,
        false,
        false,
        &(),
    )
}

/// Native fixture entry. Trace instrumentation is absent from ordinary emission.
#[doc(hidden)]
pub fn emit_object_for_test(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
) -> Result<Object> {
    emit_object_for_test_observed(input, layouts, options, object_key, false)
}

/// Native observation fixture, retaining the admission injector and cleanup trace.
#[doc(hidden)]
pub fn emit_object_for_test_observed(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
    observe_charge: bool,
) -> Result<Object> {
    emit(
        input,
        &input.link_symbols(),
        layouts,
        options,
        object_key,
        true,
        observe_charge,
        &(),
    )
}
/// Emit with diagnostic stage observation. Ordinary emission uses a zero-sized
/// observer whose hooks disappear during Rust compilation.
#[doc(hidden)]
pub fn emit_object_profiled(
    input: &c::Module,
    layouts: &[hirsel_core_layout::RecipeRow],
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
    observer: &impl profile::Observer,
) -> Result<Object> {
    emit(
        input,
        &input.link_symbols(),
        layouts,
        options,
        object_key,
        false,
        false,
        observer,
    )
}
/// Optional diagnostic emission; timings of these objects are not performance samples.
/// `symbols` are `input`'s [`c::Module::link_symbols`], which a caller that already
/// holds its rows' identities names without recomputing their closures.
pub fn emit_object_with_charge_observation(
    input: &c::Module,
    symbols: &[String],
    layouts: &[hirsel_core_layout::RecipeRow],
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
    observe_charge: bool,
) -> Result<Object> {
    emit(
        input,
        symbols,
        layouts,
        options,
        object_key,
        false,
        observe_charge,
        &(),
    )
}
/// Each function a body of this unit describes (`Inst::DescriptorRef`), its
/// descriptor `{code, length, revision}` then the identity bytes, defined weak
/// beside the described instance: every unit that describes it defines
/// it, and the linker keeps one per program, as it keeps one body. `revision`
/// addresses its revision root's carrier (`c::Module::revision_root`): an
/// item's this unit defines weak as `{0, 0, 0}` and a link that knows the
/// reach defines over it; an intrinsic's holds its revision.
fn descriptors(
    object: &mut ObjectModule,
    input: &c::Module,
    symbols: &[String],
    functions: &[cranelift_module::FuncId],
) -> Result<Vec<Option<cranelift_module::DataId>>> {
    let mut descriptors = vec![None; input.functions.len()];
    let mut defined = std::collections::HashSet::new();
    let mut revisions = std::collections::HashSet::new();
    let mut spawnable = std::collections::HashSet::new();
    for body in input.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            let c::Inst::DescriptorRef {
                function,
                identity,
                spawn,
                ..
            } = *inst
            else {
                continue;
            };
            let name = input.descriptor_symbol(function, &symbols[function.index()]);
            // A function made a plain `fn(take X) -> R` value: the link joins
            // its descriptor to the spawn entry of its rows by this marker's
            // name ([`c::spawnable_symbol`]).
            if let Some(spawn) = spawn {
                let marker = c::spawnable_symbol(&name, input.spawn_key(spawn));
                if spawnable.insert(marker.clone()) {
                    let id = object
                        .declare_data(&marker, Linkage::Preemptible, false, false)
                        .map_err(error)?;
                    let mut data = DataDescription::new();
                    data.define_zeroinit(1);
                    object.define_data(id, &data).map_err(error)?;
                }
            }
            if descriptors[function.index()].is_some() {
                continue;
            }
            let id = object
                .declare_data(&name, Linkage::Preemptible, false, false)
                .map_err(error)?;
            descriptors[function.index()] = Some(id);
            // Two rows of one code identity name one descriptor.
            if !defined.insert(id) {
                continue;
            }
            let root = input.revision_root(function);
            let revision = object
                .declare_data(
                    &c::revision_symbol(root.identity().digest()),
                    Linkage::Preemptible,
                    false,
                    false,
                )
                .map_err(error)?;
            if revisions.insert(revision) {
                let mut carrier = DataDescription::new();
                carrier.set_align(8);
                match &root {
                    c::RevisionRoot::Item(_) => carrier.define_zeroinit(24),
                    c::RevisionRoot::Intrinsic(identity) => {
                        let bytes = object.declare_anonymous_data(false, false).map_err(error)?;
                        let mut data = DataDescription::new();
                        data.define(
                            c::intrinsic_revision(identity)
                                .0
                                .to_vec()
                                .into_boxed_slice(),
                        );
                        object.define_data(bytes, &data).map_err(error)?;
                        let mut row = vec![0; 24];
                        row[8..16].copy_from_slice(&32u64.to_le_bytes());
                        carrier.define(row.into_boxed_slice());
                        let bytes = object.declare_data_in_data(bytes, &mut carrier);
                        carrier.write_data_addr(0, bytes, 0);
                    }
                }
                object.define_data(revision, &carrier).map_err(error)?;
            }
            let bytes = input.statics[identity.index()].literal_bytes().unwrap();
            let mut image = Vec::with_capacity(24 + bytes.len());
            image.extend_from_slice(&[0; 8]);
            image.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            image.extend_from_slice(&[0; 8]);
            image.extend_from_slice(bytes);
            let mut data = DataDescription::new();
            data.define(image.into_boxed_slice());
            data.set_align(8);
            let code = object.declare_func_in_data(functions[function.index()], &mut data);
            data.write_function_addr(0, code);
            let carrier = object.declare_data_in_data(revision, &mut data);
            data.write_data_addr(16, carrier, 0);
            object.define_data(id, &data).map_err(error)?;
        }
    }
    Ok(descriptors)
}

fn emit(
    input: &c::Module,
    symbols: &[String],
    layouts: &[hirsel_core_layout::RecipeRow],
    options: &Options<'_>,
    object_key: hirsel_core_ids::ContentHash,
    trace_drops: bool,
    observe_charge: bool,
    observer: &impl profile::Observer,
) -> Result<Object> {
    let setup = observer.enter("module_setup");
    let native = c::native_layout::resolve(std::borrow::Cow::Borrowed(input), layouts);
    let input = native.as_ref();
    let target = Target::from_triple(options.target)
        .ok_or_else(|| Error::Target(format!("{}: not a Hirsel target", options.target)))?;
    let c_abi = target.c_abi();
    let isa = {
        let _span = observer.enter("isa_setup");
        isa(target, options.optimize)?
    };
    let object_setup = observer.enter("object_setup");
    let builder = ObjectBuilder::new(
        isa,
        "hirsel-core",
        cranelift_module::default_libcall_names(),
    )
    .map_err(error)?;
    let mut object = ObjectModule::new(builder);
    drop(object_setup);
    let call_layouts = c::native_layout::CallLayouts::new(&input.types, layouts);
    let signatures = {
        let _span = observer.enter("signature_setup");
        input
            .signatures
            .iter()
            .map(|s| abi::Signature::new(s, &object, &call_layouts, c_abi))
            .collect::<Vec<_>>()
    };
    let declarations = observer.enter("declarations");
    let mut functions = Vec::with_capacity(input.functions.len());
    debug_assert_eq!(symbols.len(), input.functions.len());
    for (f, name) in input.functions.iter().zip(symbols) {
        functions.push(
            object
                .declare_function(
                    name,
                    // Every unit that carries a body defines it weak by its code
                    // identity; the linker keeps one.
                    if f.body.is_some() {
                        Linkage::Preemptible
                    } else {
                        Linkage::Import
                    },
                    &signatures[f.signature.index()].clif,
                )
                .map_err(error)?,
        );
    }
    drop(declarations);
    let static_setup = observer.enter("static_setup");
    let mut statics = Vec::new();
    // A static's local name carries its object's key, so the linked image
    // holds each once and the resident link can place an edited static's
    // section by it (incremental_link::locate). A rebuilt object renames
    // them at the same length.
    let unit = object_key.to_string();
    for (i, data) in input.statics.iter().enumerate() {
        let id = object
            .declare_data(
                &format!("hirsel.core.static.{unit}.{i}"),
                Linkage::Local,
                false,
                false,
            )
            .map_err(error)?;
        let mut desc = DataDescription::new();
        let (align, bytes) = data.native(&input.types, layouts);
        desc.define(bytes.into_owned().into_boxed_slice());
        desc.set_align(u64::from(align));
        object.define_data(id, &desc).map_err(error)?;
        statics.push(id);
    }
    let descriptors = descriptors(&mut object, input, symbols, &functions)?;
    drop(static_setup);
    let glues = {
        let _span = observer.enter("glue_declarations");
        drop::Glues::declare(&mut object, input, layouts, c_abi)?
    };
    let mut task_entries = task_entry::Glues::declare(&mut object, input, c_abi)?;
    task_entries.define_units(&mut object, input, layouts, &functions, &glues)?;
    let tables = Tables {
        input,
        layouts,
        signatures: &signatures,
        call_layouts: &call_layouts,
        functions: &functions,
        statics: &statics,
        descriptors: &descriptors,
        glues: &glues,
        task_entries: &task_entries,
        trace_drops,
        observe_charge,
        c_abi,
        debug: options.debug_lines,
        unwind: std::cell::RefCell::new(crate::unwind::Unwind::new(object.isa())?),
        site_symbols: Default::default(),
        site_constants: Default::default(),
        runtimes: Default::default(),
    };
    drop(setup);
    let mut scratch = Compilation {
        codegen: object.make_context(),
        frontend: FunctionBuilderContext::new(),
    };
    let mut debug_functions = Vec::new();
    let mut fragment = c::coverage::Fragment::default();
    for (index, f) in input.functions.iter().enumerate() {
        if f.body.is_none() {
            continue;
        }
        let build = observer.enter("build_clif");
        let sig = &signatures[f.signature.index()];
        let context = &mut scratch.codegen;
        let builder_context = &mut scratch.frontend;
        object.clear_context(context);
        context.func.signature = sig.clif.clone();
        let builder = FunctionBuilder::new(&mut context.func, builder_context);
        let record = coverage::declare(&mut object, input, c::FuncId(index as u32))?;
        function::emit(&mut object, builder, f, sig, &tables, record.as_ref())?;
        if let Some(record) = record {
            fragment.add(input, c::FuncId(index as u32), &record.sites);
        }
        drop(build);
        tables.define(observer, &mut object, functions[index], context)?;
        if options.debug_lines {
            debug_functions.push(debug::function(
                input,
                f,
                symbols[f.instance.index()].clone(),
                functions[index],
                context.compiled_code().unwrap(),
            ));
        }
    }
    glues.emit(&mut object, &tables, observer, &mut scratch)?;
    task_entries.emit(&mut object, &tables, observer, &mut scratch)?;
    let _write = observer.enter("object_write");
    let format = target.object_format();
    let origin = tables.site_constants.borrow().define(&mut object, format)?;
    let mut product = object.finish();
    if let Some(origin) = origin {
        tables
            .site_constants
            .borrow()
            .measure(&mut product, origin)?;
    }
    tables.unwind.into_inner().write(&mut product, format)?;
    let debug = options
        .debug_lines
        .then(|| debug::finish(input, &mut product, debug_functions, object_key, format));
    Ok(Object {
        bytes: crate::object::finish(product).map_err(error)?,
        debug,
        coverage: input.coverage.is_some().then_some(fragment),
    })
}

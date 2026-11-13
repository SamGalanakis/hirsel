//! CoreIR emission, independent of the old LIR pipeline.
//!
//! Scalars, checked failure and explicit cleanup edges use the frozen CoreIR
//! function ABI. Native memory calls use the separately declared core runtime C
//! ABI. This entry never translates CoreIR into old LIR.

use cranelift_codegen::ir::{self, AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hirsel_core_ir::{self as c, *};

mod abi;
mod calls;
mod drop;
mod failure;
mod function;
mod memory;
mod place;
pub mod profile;
mod scalar;

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

/// Target and optimization policy for CoreIR emission.
pub struct Options<'a> {
    pub target: &'a str,
    pub optimize: bool,
}

/// A relocatable object. Its symbols use CoreIR instance identities.
pub struct Object {
    pub bytes: Vec<u8>,
}

/// The external name of a closed CoreIR instance.
pub fn symbol(instance: &InstanceKey) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = [0u8; 65];
    name[0] = b'h';
    for (i, byte) in instance.digest().0.into_iter().enumerate() {
        name[1 + i * 2] = HEX[(byte >> 4) as usize];
        name[2 + i * 2] = HEX[(byte & 15) as usize];
    }
    // SAFETY: the prefix and every table entry are ASCII.
    unsafe { String::from_utf8_unchecked(name.to_vec()) }
}

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
    }
}

pub(super) struct Compilation {
    codegen: cranelift_codegen::Context,
    frontend: FunctionBuilderContext,
}

pub(super) struct Tables<'a> {
    input: &'a c::Module,
    layouts: &'a [hirsel_core_layout::Recipe],
    signatures: &'a [abi::Signature],
    functions: &'a [cranelift_module::FuncId],
    statics: &'a [cranelift_module::DataId],
    glues: &'a drop::Glues,
    trace_drops: bool,
    runtimes: std::cell::RefCell<calls::Imports>,
}

pub(crate) fn isa(options: &Options<'_>) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    let refuse = |message: String| Error::Target(format!("{}: {message}", options.target));
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "true"),
        ("enable_verifier", "false"),
        ("opt_level", if options.optimize { "speed" } else { "none" }),
        (
            "regalloc_algorithm",
            if options.optimize {
                "backtracking"
            } else {
                "single_pass"
            },
        ),
    ]
    .into_iter()
    .chain(crate::module::STACK_PROBE_SETTINGS)
    {
        flags.set(name, value).map_err(|e| refuse(e.to_string()))?;
    }
    cranelift_codegen::isa::lookup_by_name(options.target)
        .map_err(|e| refuse(e.to_string()))?
        .finish(settings::Flags::new(flags))
        .map_err(|e| refuse(e.to_string()))
}

/// Emit CoreIR directly to an x86-64 System V object, using the form's C struct
/// convention for multiple scalar results. CoreIR is a trusted compiler product;
/// this entry does not invoke its well-formedness checker or the old pipeline.
pub fn emit_object(input: &c::Module, options: &Options<'_>) -> Result<Object> {
    emit_object_with_layouts(input, &[], options)
}

/// Emit with the producer's already-computed layout recipes. The recipe table
/// corresponds to `input.types`; the backend reads it without recomputing layouts.
pub fn emit_object_with_layouts(
    input: &c::Module,
    layouts: &[hirsel_core_layout::Recipe],
    options: &Options<'_>,
) -> Result<Object> {
    emit(input, layouts, options, false, &())
}

/// Native fixture entry. Trace instrumentation is absent from ordinary emission.
#[doc(hidden)]
pub fn emit_object_for_test(
    input: &c::Module,
    layouts: &[hirsel_core_layout::Recipe],
    options: &Options<'_>,
) -> Result<Object> {
    emit(input, layouts, options, true, &())
}
/// Emit with diagnostic stage observation. Ordinary emission uses a zero-sized
/// observer whose hooks disappear during Rust compilation.
#[doc(hidden)]
pub fn emit_object_profiled(
    input: &c::Module,
    layouts: &[hirsel_core_layout::Recipe],
    options: &Options<'_>,
    observer: &impl profile::Observer,
) -> Result<Object> {
    emit(input, layouts, options, false, observer)
}
fn emit(
    input: &c::Module,
    layouts: &[hirsel_core_layout::Recipe],
    options: &Options<'_>,
    trace_drops: bool,
    observer: &impl profile::Observer,
) -> Result<Object> {
    let setup = observer.enter("module_setup");
    let refuse = |message: String| Error::Target(format!("{}: {message}", options.target));
    if !options.target.starts_with("x86_64") || !options.target.contains("linux") {
        return Err(refuse(
            "CoreIR entry currently implements the x86-64 Linux C ABI".into(),
        ));
    }
    let isa = isa(options)?;
    let builder = ObjectBuilder::new(
        isa,
        "hirsel-core",
        cranelift_module::default_libcall_names(),
    )
    .map_err(error)?;
    let mut object = ObjectModule::new(builder);
    let signatures: Vec<_> = input.signatures.iter().map(abi::Signature::new).collect();
    let mut functions = Vec::new();
    for f in &input.functions {
        let name = symbol(&input.instances[f.instance.index()]);
        functions.push(
            object
                .declare_function(
                    &name,
                    if f.body.is_some() {
                        Linkage::Export
                    } else {
                        Linkage::Import
                    },
                    &signatures[f.signature.index()].clif,
                )
                .map_err(error)?,
        );
    }
    let mut statics = Vec::new();
    for (i, data) in input.statics.iter().enumerate() {
        let id = object
            .declare_data(
                &format!("hirsel.core.static.{i}"),
                Linkage::Local,
                false,
                false,
            )
            .map_err(error)?;
        let mut desc = DataDescription::new();
        desc.define(data.bytes.clone().into_boxed_slice());
        desc.set_align(u64::from(data.align));
        object.define_data(id, &desc).map_err(error)?;
        statics.push(id);
    }
    let glues = drop::Glues::declare(&mut object, input, layouts)?;
    let tables = Tables {
        input,
        layouts,
        signatures: &signatures,
        functions: &functions,
        statics: &statics,
        glues: &glues,
        trace_drops,
        runtimes: Default::default(),
    };
    drop(setup);
    let mut scratch = Compilation {
        codegen: object.make_context(),
        frontend: FunctionBuilderContext::new(),
    };
    for (index, f) in input.functions.iter().enumerate() {
        let Some(body) = &f.body else { continue };
        let build = observer.enter("build_clif");
        let sig = &signatures[f.signature.index()];
        let context = &mut scratch.codegen;
        let builder_context = &mut scratch.frontend;
        object.clear_context(context);
        context.func.signature = sig.clif.clone();
        let builder = FunctionBuilder::new(&mut context.func, builder_context);
        function::emit(&mut object, builder, body, sig, &tables)?;
        drop(build);
        observer
            .define(&mut object, functions[index], context)
            .map_err(error)?;
    }
    glues.emit(&mut object, &tables, observer, &mut scratch)?;
    let _write = observer.enter("object_write");
    Ok(Object {
        bytes: crate::object::finish(object.finish()).map_err(error)?,
    })
}

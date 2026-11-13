//! CoreIR emission, independent of the old LIR pipeline.
//!
//! This first entry implements the runtime-independent scalar subset. Operations
//! needing the task's failure chain or storage runtime return `Unsupported` until
//! the core runtime ABI is available. It never translates CoreIR into old LIR.

use cranelift_codegen::ir::{self, AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hirsel_core_ir::{self as c, *};

mod abi;
mod function;
mod scalar;
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
    let mut name = String::from("h");
    use std::fmt::Write as _;
    for byte in instance.digest().0 {
        write!(name, "{byte:02x}").unwrap();
    }
    name
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

pub(crate) fn isa(options: &Options<'_>) -> Result<cranelift_codegen::isa::OwnedTargetIsa> {
    let refuse = |message: String| Error::Target(format!("{}: {message}", options.target));
    let mut flags = settings::builder();
    for (name, value) in [
        ("is_pic", "true"),
        ("enable_verifier", "false"),
        ("opt_level", if options.optimize { "speed" } else { "none" }),
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
    if input.signatures.iter().any(|s| s.dest.is_some() || s.never) {
        return Err(unsupported(
            "destination and never signatures require the core runtime ABI",
        ));
    }
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
    for (index, f) in input.functions.iter().enumerate() {
        let Some(body) = &f.body else { continue };
        let sig = &signatures[f.signature.index()];
        let mut context = object.make_context();
        context.func.signature = sig.clif.clone();
        let mut builder_context = FunctionBuilderContext::new();
        let builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        function::emit(
            &mut object,
            builder,
            body,
            sig,
            &signatures,
            &functions,
            &statics,
        )?;
        object
            .define_function(functions[index], &mut context)
            .map_err(error)?;
    }
    Ok(Object {
        bytes: crate::object::finish(object.finish()).map_err(error)?,
    })
}

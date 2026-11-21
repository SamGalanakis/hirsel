//! FFI-3's C declaration and ABI text, derived from the checked headers.
use hirsel_core_front::{AbiParameter, AbiResult, ModuleInput, Program};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{Loan, Module, Param, PlaceTy, Signature};
use hirsel_core_layout::{Body, Carried, FieldTy, Repr, Scalar, Ty};
use hirsel_core_resolve::{CorpusManifest, ModulePath, SourceId};
use std::fmt::Write;
use std::io;

const BYTES: &str = "core@1/core/List[u8]";

#[derive(Clone, Copy)]
pub(crate) enum Argument<'a> {
    Scalar(Scalar),
    Bytes { mutable: bool },
    Handle { kind: &'a str, release: bool },
}

#[derive(Clone, Copy)]
pub(crate) enum Return<'a> {
    Unit,
    Scalar(Scalar),
    New(&'a str),
}

/// A PROG-9 header as carried by a foreign leaf. Admission and codegen must
/// interpret the same header the archive's ABI text was checked against.
pub(crate) struct Abi<'a> {
    pub interface: &'a str,
    pub waits: bool,
    arguments: &'a str,
    pub result: Return<'a>,
}

impl<'a> Abi<'a> {
    pub(crate) fn parse(header: &'a str) -> Option<Self> {
        let (parameters, result) = header.strip_prefix('(')?.split_once(")->")?;
        let (result, waits) = result
            .strip_suffix("waits")
            .map_or((result, false), |r| (r.trim_end(), true));
        let (interface, arguments) = match parameters.split_once(',') {
            Some((_, "")) => return None,
            Some(parts) => parts,
            None => (parameters, ""),
        };
        let interface = definition_path(
            interface
                .strip_prefix("core@1/core/Foreign[")?
                .strip_suffix(']')?,
        )?;
        let result = match result {
            "()" => Return::Unit,
            scalar if ffi_scalar(scalar).is_some() => Return::Scalar(ffi_scalar(scalar)?),
            result => Return::New(handle_kind(
                result
                    .strip_prefix("core@1/core/Option[")?
                    .strip_suffix(']')?,
            )?),
        };
        let mut releases = 0;
        let mut count = 0;
        if !arguments.is_empty() {
            for argument in arguments.split(',') {
                releases += usize::from(matches!(
                    argument_kind(argument)?,
                    Argument::Handle { release: true, .. }
                ));
                count += 1;
            }
        }
        if releases != 0 && (count != 1 || matches!(result, Return::New(_))) {
            return None;
        }
        Some(Self {
            interface,
            waits,
            arguments,
            result,
        })
    }

    pub(crate) fn arguments(&self) -> impl Iterator<Item = Argument<'a>> {
        self.arguments
            .split_terminator(',')
            .map(|arg| argument_kind(arg).unwrap())
    }

    pub(crate) fn line(&self, symbol: &str) -> String {
        let args: Vec<_> = self
            .arguments()
            .map(|arg| match arg {
                Argument::Scalar(s) => s.name().to_owned(),
                Argument::Bytes { mutable: false } => "bytes".into(),
                Argument::Bytes { mutable: true } => "mut bytes".into(),
                Argument::Handle { kind, release } => {
                    let mode = if release { "take" } else { "handle" };
                    format!("{mode} {kind}")
                }
            })
            .collect();
        let result = match self.result {
            Return::Unit => "()".into(),
            Return::Scalar(s) => s.name().to_owned(),
            Return::New(kind) => format!("new {kind}"),
        };
        let mode = if self.waits { "waits" } else { "stack" };
        format!("{symbol} {mode} ({}) -> {result}", args.join(", "))
    }

    pub(crate) fn matches_slot(&self, slot: &Signature, module: &Module) -> bool {
        let Some((&Param::Value(Ty::Scalar(Scalar::U64)), params)) = slot.params.split_first()
        else {
            return false;
        };
        if slot.never || params.len() != self.arguments().count() {
            return false;
        }
        if !params.iter().zip(self.arguments()).all(|(&param, arg)| match arg {
            Argument::Scalar(s) => param == Param::Value(Ty::Scalar(s)),
            Argument::Handle { .. } => param == Param::Value(Ty::Scalar(Scalar::U64)),
            Argument::Bytes { mutable } => {
                let Param::Address { place: PlaceTy::Declared(ty), loan } = param else {
                    return false;
                };
                let shape = &module.types[ty.index()];
                let items = |ty: &FieldTy| {
                    matches!(ty, FieldTy::Seq(element) if **element == FieldTy::Scalar(Scalar::U8))
                };
                loan == (if mutable { Loan::Mut } else { Loan::View })
                    && shape.name == BYTES
                    && shape.carried == Carried::Address
                    && shape.repr == Repr::Own
                    && matches!(&shape.body, Body::Record(fields) if matches!(fields.as_slice(), [field] if items(&field.ty)))
                    && shape.held.as_ref().is_some_and(items)
                    && shape.finalizer.is_none()
            }
        }) {
            return false;
        }
        match self.result {
            Return::Unit => slot.results.is_empty() && slot.dest.is_none(),
            Return::Scalar(s) => slot.results == [Ty::Scalar(s)] && slot.dest.is_none(),
            Return::New(kind) => slot.results.is_empty() && slot.dest.is_some_and(|ty| {
                let shape = &module.types[ty.index()];
                shape.name.strip_prefix("core@1/core/Option[")
                    .and_then(|name| name.strip_suffix(']'))
                    .and_then(handle_kind) == Some(kind)
                    && shape.carried == Carried::Address
                    && shape.repr == Repr::Own
                    && shape.finalizer.is_none()
                    && matches!(&shape.body, Body::Variant(cases) if matches!(cases.as_slice(),
                        [none, some] if none.name == "None" && none.fields.is_empty()
                            && some.name == "Some"
                            && matches!(some.fields.as_slice(), [field] if matches!(field.ty,
                                FieldTy::Inline(row) if hirsel_core_layout::foreign_kind(&module.types[row.index()]) == Some(kind)))))
            }),
        }
    }
}

fn argument_kind(text: &str) -> Option<Argument<'_>> {
    match text {
        BYTES => Some(Argument::Bytes { mutable: false }),
        "mut core@1/core/List[u8]" => Some(Argument::Bytes { mutable: true }),
        scalar if ffi_scalar(scalar).is_some() => Some(Argument::Scalar(ffi_scalar(scalar)?)),
        handle => {
            let (value, release) = handle
                .strip_prefix("take ")
                .map_or((handle, false), |value| (value, true));
            Some(Argument::Handle {
                kind: handle_kind(value)?,
                release,
            })
        }
    }
}

fn ffi_scalar(text: &str) -> Option<Scalar> {
    Scalar::from_name(text).filter(|s| !matches!(s, Scalar::Bool | Scalar::Char))
}

fn handle_kind(text: &str) -> Option<&str> {
    definition_path(
        text.strip_prefix("core@1/foreign/Ptr[")?
            .strip_suffix(']')?,
    )
}

fn definition_path(text: &str) -> Option<&str> {
    (text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_@/%".contains(&b))
        && DefPath::from_full(text).is_some())
    .then_some(text)
}

pub(crate) fn symbol_prefix(
    resolution: &crate::package_resolve::Resolution,
    module: &DefPath,
) -> io::Result<String> {
    let path = &module.0;
    let parts = module_parts(module);
    resolution
        .workspace
        .members
        .values()
        .map(|row| &row.content)
        .chain(resolution.libraries.values().map(|row| &row.content))
        .filter(|content| content.id.compiler_id() == path.package)
        .find_map(|content| content.foreign.get(&parts))
        .map(|foreign| foreign.symbols.clone())
        .ok_or_else(|| io::Error::other(format!("{module} is not a foreign module")))
}

fn module_parts(module: &DefPath) -> Vec<String> {
    let mut parts = module.0.modules.clone();
    parts.push(module.0.name.clone());
    parts
}

pub(crate) fn header(inputs: CorpusManifest, module: &DefPath, prefix: &str) -> io::Result<String> {
    let corpus = inputs.load()?;
    let parsed = corpus.parse();
    for (source, output) in corpus.sources.iter().zip(&parsed.files) {
        if let Some(error) = &output.resource_refusal {
            return Err(io::Error::other(format!(
                "{}: {error}",
                source.file.display()
            )));
        }
        if let Some(error) = output.diagnostics.first() {
            return Err(io::Error::other(format!(
                "{}: {}: {}",
                crate::diagnostics::render_span(
                    &source.file,
                    source.text.as_bytes(),
                    error.primary
                ),
                error.rule,
                error.message,
            )));
        }
    }
    let index = corpus.index(&parsed);
    let id = index
        .module(&ModulePath {
            package: module.0.package.clone(),
            modules: module_parts(module),
        })
        .ok_or_else(|| io::Error::other(format!("{module} is outside the package closure")))?;
    let modules = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .enumerate()
        .map(|(i, (source, output))| ModuleInput {
            id: index.module(&source.module).unwrap(),
            source: SourceId(i as u32),
            syntax: output.syntax,
        })
        .collect::<Vec<_>>();
    let program = Program::declare(&index, &modules);
    if let Some(refusal) = program.refusals.first() {
        let site = refusal.primary;
        let at = if let Some(file) = site.source {
            let file = file.0 as usize;
            let source = &corpus.sources[file];
            let base = modules[file]
                .syntax
                .items
                .get(site.item as usize)
                .map_or(0, |item| item.file_offset);
            let span = hirsel_core_syntax::Span {
                start: base + site.span.start,
                end: base + site.span.end,
            };
            crate::diagnostics::render_span(&source.file, source.text.as_bytes(), span)
        } else {
            module.to_string()
        };
        return Err(io::Error::other(format!(
            "{at}: {}: {}",
            refusal.rule, refusal.message
        )));
    }
    if let Some(gap) = program.gaps.first() {
        return Err(io::Error::other(gap.what));
    }
    if let Some(error) = program.resource_refusals.first() {
        return Err(io::Error::other(error.message.clone()));
    }
    let mut signatures = program
        .foreign_abi(id)
        .ok_or_else(|| io::Error::other(format!("{module} is not a checked foreign module")))?;
    signatures.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    let mut prototypes = String::from("#include <stdint.h>\n\n");
    let mut text = String::new();
    for signature in signatures {
        let symbol = format!("{prefix}{}", signature.name);
        let mut c_params = Vec::new();
        let mut abi_params = Vec::new();
        for (name, parameter) in &signature.params {
            match parameter {
                AbiParameter::Scalar(scalar) => {
                    c_params.push(c_scalar(*scalar).into());
                    abi_params.push(scalar.name().into());
                }
                AbiParameter::Bytes { mutable } => {
                    if *mutable {
                        c_params.push(format!("uint8_t *{name}_ptr"));
                        abi_params.push("mut bytes".into());
                    } else {
                        c_params.push(format!("const uint8_t *{name}_ptr"));
                        abi_params.push("bytes".into());
                    }
                    c_params.push(format!("uint64_t {name}_len"));
                }
                AbiParameter::Handle { kind, release } => {
                    c_params.push("void *".into());
                    let mode = if *release { "take" } else { "handle" };
                    abi_params.push(format!("{mode} {}", kind.definition_identity()));
                }
            }
        }
        if c_params.is_empty() {
            c_params.push("void".into());
        }
        let (c_result, abi_result) = match signature.result {
            AbiResult::Unit => ("void", "()".into()),
            AbiResult::Scalar(scalar) => (c_scalar(scalar), scalar.name().into()),
            AbiResult::New(kind) => ("void *", format!("new {}", kind.definition_identity())),
        };
        let mode = if signature.waits { "waits" } else { "stack" };
        writeln!(prototypes, "{c_result} {symbol}({});", c_params.join(", ")).unwrap();
        writeln!(
            text,
            "{symbol} {mode} ({}) -> {abi_result}",
            abi_params.join(", ")
        )
        .unwrap();
    }
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    write!(
        prototypes,
        "\n#define HIRSEL_ABI_TEXT \\\n    static const char hirsel_abi[sizeof(\"{escaped}\") - 1] \\\n        __attribute__((used, section(\".hirsel_abi\"))) = \"{escaped}\";\n"
    ).unwrap();
    Ok(prototypes)
}

fn c_scalar(scalar: Scalar) -> &'static str {
    match scalar {
        Scalar::I8 => "int8_t",
        Scalar::I16 => "int16_t",
        Scalar::I32 => "int32_t",
        Scalar::I64 => "int64_t",
        Scalar::U8 => "uint8_t",
        Scalar::U16 => "uint16_t",
        Scalar::U32 => "uint32_t",
        Scalar::U64 => "uint64_t",
        Scalar::F32 => "float",
        Scalar::F64 => "double",
        Scalar::Bool | Scalar::Char => unreachable!("not FFI-5 scalars"),
    }
}

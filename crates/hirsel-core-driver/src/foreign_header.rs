//! FFI-3's C declaration and ABI text, derived from the checked headers.
use hirsel_core_front::{AbiParameter, AbiResult, ModuleInput, Program};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::Scalar;
use hirsel_core_resolve::{CorpusManifest, ModulePath, SourceId};
use std::fmt::Write;
use std::io;

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

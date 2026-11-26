use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use wasmparser::{ExternalKind, FuncType, Parser, Payload, TypeRef, ValType, Validator};

/// A module admitted against its content pin, shape and reached headers.
pub struct CheckedModule<'a> {
    pub(super) bytes: &'a [u8],
}

pub struct Export<'a> {
    pub symbol: &'a str,
    pub params: &'a [ValType],
    pub results: &'a [ValType],
    pub abi_line: &'a str,
    pub site: &'a str,
}

/// FFI-11 admission in diagnostic order. The caller supplies the sealed bytes
/// and their package identity for the refusal messages.
pub fn admit<'a>(
    bytes: &'a [u8],
    pin: [u8; 32],
    file: &str,
    package: &str,
    prefix: &str,
    reached: &[Export<'_>],
    needs_loan_area: bool,
) -> Result<CheckedModule<'a>, String> {
    let module = format!("FFI-11: module {file} of {package}");
    let observed: [u8; 32] = Sha256::digest(bytes).into();
    if observed != pin {
        return Err(format!(
            "{module} does not match its pin: expected sha256 {}, found sha256 {}",
            hex(pin),
            hex(observed),
        ));
    }
    if !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return Err(format!("{module} is not a core WebAssembly module"));
    }
    let invalid = |error: wasmparser::BinaryReaderError| {
        format!("{module} is not valid with the admitted features: {error}")
    };
    let types = Validator::new_with_features(super::super::ADMITTED_FEATURES)
        .validate_all(bytes)
        .map_err(&invalid)?;
    let types = types.as_ref();
    if types.memory_count() == 0 {
        return Err(format!("{module}: no memory"));
    }

    let mut imports = Vec::new();
    let mut exports = BTreeMap::new();
    let mut start = false;
    let mut abi = None;
    let mut duplicate_abi = false;
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(&invalid)? {
            Payload::ImportSection(section) => {
                for import in section.into_imports() {
                    imports.push(import.map_err(&invalid)?);
                }
            }
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export.map_err(&invalid)?;
                    exports.insert(export.name, export);
                }
            }
            Payload::StartSection { .. } => start = true,
            Payload::CustomSection(section) if section.name() == "hirsel_abi" => {
                duplicate_abi |= abi.replace(section.data()).is_some();
            }
            _ => {}
        }
    }

    if imports
        .iter()
        .any(|entry| matches!(entry.ty, TypeRef::Memory(_)))
    {
        return Err(format!("{module}: an imported memory"));
    }
    if !exports
        .get("memory")
        .is_some_and(|export| export.kind == ExternalKind::Memory)
    {
        return Err(format!("{module}: a memory not exported as memory"));
    }
    for import in imports {
        let valid = match import.ty {
            TypeRef::Func(index) if import.module == "wasi_snapshot_preview1" => {
                let ty = types[types.core_type_at_in_module(index)].unwrap_func();
                null_import(import.name)
                    .is_some_and(|(params, results)| type_matches(ty, params, results))
            }
            _ => false,
        };
        if !valid {
            return Err(format!(
                "{module} imports {}.{}, which is not a null import",
                import.module, import.name,
            ));
        }
    }
    if start {
        return Err(format!("{module} has a start function"));
    }
    if exports.contains_key("_start") {
        return Err(format!("{module} exports _start"));
    }
    let function = |name: &str| {
        exports
            .get(name)
            .filter(|export| export.kind == ExternalKind::Func)
            .map(|export| types[types.core_function_at(export.index)].unwrap_func())
    };
    if exports.contains_key("_initialize")
        && !function("_initialize").is_some_and(|ty| type_matches(ty, &[], &[]))
    {
        return Err(format!(
            "{module} exports _initialize with a type other than [] -> []"
        ));
    }
    if let Some((name, _)) = exports.iter().find(|(name, export)| {
        export.kind == ExternalKind::Func && **name != "_initialize" && !name.starts_with(prefix)
    }) {
        return Err(format!(
            "{module} exports {name}, which does not start with the prefix {prefix}"
        ));
    }

    let check_type = |symbol: &str, params: &[ValType], results: &[ValType], site: &str| {
        let ty = function(symbol)
            .ok_or_else(|| format!("{module} does not export {symbol}, declared at {site}"))?;
        if !type_matches(ty, params, results) {
            return Err(format!(
                "{module} exports {symbol} as {}, but its header gives {}",
                signature(ty.params(), ty.results()),
                signature(params, results),
            ));
        }
        Ok(())
    };
    for entry in reached {
        check_type(entry.symbol, entry.params, entry.results, entry.site)?;
    }
    if needs_loan_area {
        check_type(
            &format!("{prefix}loan_area"),
            &[ValType::I32],
            &[ValType::I32],
            "FFI-12",
        )?;
    }
    let abi = abi
        .filter(|_| !duplicate_abi)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .ok_or_else(|| format!("{module} has no ABI text"))?;
    for entry in reached {
        let mut lines = abi.split_terminator('\n').filter(|line| {
            line.split_once(' ')
                .is_some_and(|(name, _)| name == entry.symbol)
        });
        let actual = lines.next().unwrap_or("<missing>");
        if actual != entry.abi_line || lines.next().is_some() {
            return Err(format!(
                "{module}: the ABI of {} is {actual}, but its header declares {}",
                entry.symbol, entry.abi_line,
            ));
        }
    }
    Ok(CheckedModule { bytes })
}

fn type_matches(ty: &FuncType, params: &[ValType], results: &[ValType]) -> bool {
    ty.params() == params && ty.results() == results
}

fn signature(params: &[ValType], results: &[ValType]) -> String {
    let types = |values: &[ValType]| {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    };
    format!("[{}] -> [{}]", types(params), types(results))
}

fn hex(bytes: [u8; 32]) -> String {
    use std::fmt::Write;

    let mut hex = String::with_capacity(64);
    for byte in bytes {
        write!(&mut hex, "{byte:02x}").unwrap();
    }
    hex
}

fn null_import(name: &str) -> Option<(&'static [ValType], &'static [ValType])> {
    use ValType::{I32, I64};

    let params: &[ValType] = match name {
        "args_sizes_get" | "environ_sizes_get" | "args_get" | "environ_get" | "fd_fdstat_get"
        | "fd_prestat_get" | "clock_res_get" | "random_get" => &[I32, I32],
        "fd_read" | "fd_write" | "poll_oneoff" => &[I32, I32, I32, I32],
        "fd_seek" => &[I32, I64, I32, I32],
        "fd_prestat_dir_name" => &[I32, I32, I32],
        "fd_close" => &[I32],
        "clock_time_get" => &[I32, I64, I32],
        "sched_yield" => &[],
        "proc_exit" => return Some((&[I32], &[])),
        _ => return None,
    };
    Some((params, &[I32]))
}

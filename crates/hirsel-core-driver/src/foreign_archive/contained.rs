//! FFI-10 and FFI-11: a contained module is admitted by its pin and its shape
//! before anything is compiled or linked.
use crate::code_admission::AdmissionError;
use crate::code_store::ObjectKind;
use crate::foreign_header::Abi;
use hirsel_core_contained::compile::{self, CheckedModule, Export, admit};
use hirsel_core_contained::wasmtime::Engine;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::address::{contained_address, foreign_address};
use hirsel_core_ir::release_recipe::{ForeignFunction, ForeignLeaf};
use hirsel_core_layout::Target;
use hirsel_core_resolve::{ForeignArchiveCapture, ForeignModuleCapture};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Mutex, OnceLock};

/// FFI-16: a contained function is addressed by its module's pin, a linked one
/// by its archive-set digest.
pub(crate) fn addresses<'a>(
    leaf: &'a ForeignLeaf,
    capture: &ForeignModuleCapture,
) -> impl Fn(&ForeignFunction) -> ContentHash + 'a {
    let code = capture.wasm().map(|module| module.pin).ok_or_else(|| {
        let mut rows = Vec::new();
        leaf.encode_archive_rows(&mut rows);
        ObjectKind::ForeignArchive.hash(&rows)
    });
    move |function| match code {
        Ok(pin) => contained_address(function, &leaf.symbols, pin),
        Err(archives) => foreign_address(function, &leaf.symbols, archives),
    }
}

/// Recipes, replay and Darwin images do not yet carry contained code (W1
/// sub-slice d, and W2).
pub(crate) fn not_built() -> AdmissionError {
    AdmissionError::Refused("FFI-10: containment = \"wasm\" is not built yet".into())
}

/// FFI-15: the engine that precompiles for `target`, one per target.
fn engine(target: Target) -> Result<&'static Engine, AdmissionError> {
    static ENGINES: OnceLock<Mutex<BTreeMap<&'static str, &'static Engine>>> = OnceLock::new();
    let mut engines = ENGINES.get_or_init(Default::default).lock().unwrap();
    if let Some(engine) = engines.get(target.triple()) {
        return Ok(engine);
    }
    let engine = compile::engine(target)
        .map_err(|error| AdmissionError::Refused(format!("FFI-15: {error}")))?;
    let engine = &*Box::leak(Box::new(engine));
    engines.insert(target.triple(), engine);
    Ok(engine)
}

/// Admit a reached contained module (FFI-11) in its sealed copy, then
/// precompile those sealed bytes, which are all the compiler is given, for
/// `target` (FFI-15). The artifact is the image's one copy of its code.
pub(crate) fn compile_module(
    leaf: &ForeignLeaf,
    row: &ForeignArchiveCapture,
    target: &str,
) -> Result<Vec<u8>, AdmissionError> {
    let bytes = sealed_bytes(row)?;
    compile_bytes(leaf, row.pin, &row.path, &bytes, target)
}

/// Admit and precompile a contained module's bytes, as a recipe's admission
/// loads them from the code store, for `target`. `path` names the module
/// in refusals.
pub(crate) fn compile_bytes(
    leaf: &ForeignLeaf,
    pin: [u8; 32],
    path: &str,
    bytes: &[u8],
    target: &str,
) -> Result<Vec<u8>, AdmissionError> {
    let checked = admit_checked(leaf, pin, path, bytes)?;
    let target = Target::from_triple(target)
        .ok_or_else(|| AdmissionError::Refused(format!("{target} is not a Hirsel target")))?;
    match target {
        Target::X86_64Linux | Target::Aarch64Linux => {}
        Target::Aarch64Darwin => return Err(not_built()),
    }
    compile::precompile(engine(target)?, checked)
        .map_err(|error| AdmissionError::Refused(format!("FFI-15: {error}")))
}

fn sealed_bytes(row: &ForeignArchiveCapture) -> Result<Vec<u8>, AdmissionError> {
    let mut sealed = crate::foreign_archive::seal(&row.bytes).map_err(|_| {
        AdmissionError::Refused("FFI-11: this host cannot seal staged modules".into())
    })?;
    let file = sealed.file_mut();
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(row.bytes.len());
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn admit_checked<'a>(
    leaf: &ForeignLeaf,
    pin: [u8; 32],
    path: &str,
    bytes: &'a [u8],
) -> Result<CheckedModule<'a>, AdmissionError> {
    let headers = leaf
        .functions
        .iter()
        .map(|function| {
            let abi = Abi::parse(&function.header)
                .ok_or(AdmissionError::Binding("invalid foreign function header"))?;
            let symbol = format!("{}{}", leaf.symbols, function.identity.definition.0.name);
            let (params, results) = abi.wasm_type();
            let loans = abi
                .arguments()
                .any(|argument| matches!(argument, crate::foreign_header::Argument::Bytes { .. }));
            Ok((
                symbol.clone(),
                params,
                results,
                abi.contained_line(&symbol),
                function.identity.definition.definition_identity(),
                loans,
            ))
        })
        .collect::<Result<Vec<_>, AdmissionError>>()?;
    let reached = headers
        .iter()
        .map(|(symbol, params, results, line, site, _)| Export {
            symbol,
            params,
            results,
            abi_line: line,
            site,
        })
        .collect::<Vec<_>>();
    let loans = headers.iter().any(|header| header.5);
    admit(
        bytes,
        pin,
        path,
        &leaf.module.definition_identity(),
        &leaf.symbols,
        &reached,
        loans,
    )
    .map_err(AdmissionError::Refused)
}

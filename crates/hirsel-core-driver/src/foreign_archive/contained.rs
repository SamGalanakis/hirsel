//! FFI-10 and FFI-11: a contained module is admitted by its pin and its shape
//! before anything is compiled or linked.
use crate::code_admission::AdmissionError;
use crate::code_store::ObjectKind;
use crate::foreign_header::Abi;
use hirsel_core_contained::compile::{Export, admit};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::address::{contained_address, foreign_address};
use hirsel_core_ir::release_recipe::{ForeignFunction, ForeignLeaf};
use hirsel_core_resolve::{ForeignArchiveCapture, ForeignModuleCapture};
use std::io::{Read, Seek, SeekFrom};

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

/// Sub-slice (a) admits a contained module (FFI-11) but does not yet call it.
pub(crate) fn not_built() -> AdmissionError {
    AdmissionError::Refused("FFI-10: containment = \"wasm\" is not built yet".into())
}

/// Check a reached contained module (FFI-11) in its sealed copy, and return
/// those sealed bytes, which are all the compiler is given.
pub(crate) fn admit_module(
    leaf: &ForeignLeaf,
    row: &ForeignArchiveCapture,
) -> Result<Vec<u8>, AdmissionError> {
    let mut sealed = crate::foreign_archive::seal(&row.bytes).map_err(|_| {
        AdmissionError::Refused("FFI-11: this host cannot seal staged modules".into())
    })?;
    let file = sealed.file_mut();
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(row.bytes.len());
    file.read_to_end(&mut bytes)?;
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
        &bytes,
        row.pin,
        &row.path,
        &leaf.module.definition_identity(),
        &leaf.symbols,
        &reached,
        loans,
    )
    .map_err(AdmissionError::Refused)?;
    Ok(bytes)
}

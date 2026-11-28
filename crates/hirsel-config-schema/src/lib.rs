//! Versioned toolchain-owned data schemas and their generated Rust projections.
use hirsel_data::DataError;
pub use hirsel_data::{Descriptor, Document, Edit, Limits, PathPart, quote};
use serde_json::Value;
#[cfg(test)]
hirsel_guard::install!();

/// The typed Rust projection of a root. Each implementation pins one descriptor.
pub trait Config: Sized {
    fn schema() -> &'static Descriptor;
    fn from_value(value: &Value) -> Option<Self>;
    fn read(bytes: &[u8], limits: Limits) -> Result<Self, DataError> {
        let document = hirsel_data::decode(bytes, &[Self::schema()], limits)?;
        Ok(Self::from_value(&document.value).expect("schema-generated projection"))
    }
}

fn sequence<T>(v: &Value, f: impl Fn(&Value) -> Option<T>) -> Option<Vec<T>> {
    v.as_array()?.iter().map(f).collect()
}
fn pairs<T>(v: &Value, f: impl Fn(&Value) -> Option<T>) -> Option<Vec<(String, T)>> {
    sequence(v, |pair| {
        Some((pair.get(0)?.as_str()?.to_owned(), f(pair.get(1)?)?))
    })
}
fn optional<T>(v: &Value, f: impl Fn(&Value) -> Option<T>) -> Option<Option<T>> {
    match v.get("case")?.as_str()? {
        "Some" => Some(Some(f(v.get("fields")?.get(0)?)?)),
        "None" => Some(None),
        _ => None,
    }
}
include!(concat!(env!("OUT_DIR"), "/schemas.rs"));

#[cfg(test)]
mod tests;

pub fn read_bounded(input: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    input
        .take(Limits::default().input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Read a file against one pinned, built-in descriptor.
pub fn read_file<T: Config>(path: &std::path::Path) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = read_bounded(std::fs::File::open(path)?)?;
    Ok(T::read(&bytes, Limits::default())?)
}

/// Export a checked document using only the descriptors chosen by the caller.
pub fn export(
    path: &std::path::Path,
    roots: &[&Descriptor],
    mut out: impl std::io::Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = read_bounded(std::fs::File::open(path)?)?;
    let document = hirsel_data::decode(&bytes, roots, Limits::default())?;
    serde_json::to_writer(&mut out, &document.projection())?;
    out.write_all(b"\n")?;
    Ok(())
}

pub fn package_document(
    bytes: &[u8],
    is_workspace: &mut bool,
) -> Result<Document<'static>, DataError> {
    hirsel_data::decode_with_root(
        bytes,
        &[pkg_v1::Package::schema(), pkg_v1::Workspace::schema()],
        Limits {
            diagnostics: Limits::default().collection,
            ..Limits::default()
        },
        &mut |s| *is_workspace = s.root() == "Workspace",
    )
}

//! Versioned toolchain-owned data schemas and their generated Rust projections.
use hirsel_data::{DataError, Descriptor, Limits};
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

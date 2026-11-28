//! The semantic reader retains schema names by reference. JSON projection is
//! formed only when a host caller asks for the source-preserving document.
use super::*;
use serde_json::{Value, json};

pub enum Decoded<'s> {
    Scalar(Value),
    Default(&'s Value),
    Bytes(Vec<u8>),
    Members(Members<'s>),
    Case(&'s str, Members<'s>),
    Map(Vec<(String, Decoded<'s>)>),
}

pub enum Members<'s> {
    Positional(Vec<Decoded<'s>>),
    Labelled(Vec<(&'s str, Decoded<'s>)>),
}

impl Members<'_> {
    fn projection(self) -> Value {
        match self {
            Self::Positional(xs) => Value::Array(xs.into_iter().map(Decoded::projection).collect()),
            Self::Labelled(xs) => Value::Object(
                xs.into_iter()
                    .map(|(n, v)| (n.into(), v.projection()))
                    .collect(),
            ),
        }
    }
}

impl Decoded<'_> {
    pub(crate) fn projection(self) -> Value {
        match self {
            Self::Scalar(v) => v,
            Self::Default(v) => v.clone(),
            Self::Bytes(v) => json!(v),
            Self::Members(v) => v.projection(),
            Self::Case(name, fields) => json!({"case":name,"fields":fields.projection()}),
            Self::Map(pairs) => Value::Array(
                pairs
                    .into_iter()
                    .map(|(k, v)| json!([k, v.projection()]))
                    .collect(),
            ),
        }
    }
}

pub(crate) fn host_memory() -> DataError {
    error(ErrorKind::HostMemory, Span::default())
}

pub(crate) fn storage<T>(n: usize) -> Result<Vec<T>, DataError> {
    let mut values = Vec::new();
    values.try_reserve_exact(n).map_err(|_| host_memory())?;
    Ok(values)
}

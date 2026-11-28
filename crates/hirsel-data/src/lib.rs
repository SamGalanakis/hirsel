//! Closed data schemas, typed literal documents and source-preserving edits.
mod declaration;
mod decoded;
use decoded::{Decoded, Members};
mod edit;
pub mod guest;
pub mod image;
mod read;
mod scalar;

pub use hirsel_core_syntax::Span;
use hirsel_core_syntax::{Item, NodeId, NodeKind, SyntaxFile, TokenId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use declaration::ResolvedType;
pub use edit::{Edit, PathPart};
pub use read::decode;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub input_bytes: usize,
    pub depth: usize,
    pub nodes: usize,
    pub collection: usize,
    pub decoded_bytes: usize,
    pub comparison_bytes: usize,
    pub diagnostics: usize,
}
impl From<[u64; 7]> for Limits {
    fn from(words: [u64; 7]) -> Self {
        let [
            input_bytes,
            depth,
            nodes,
            collection,
            decoded_bytes,
            comparison_bytes,
            diagnostics,
        ] = words.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        Self {
            input_bytes,
            depth,
            nodes,
            collection,
            decoded_bytes,
            comparison_bytes,
            diagnostics,
        }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            input_bytes: 1 << 20,
            depth: 64,
            nodes: 1 << 17,
            collection: 1 << 16,
            decoded_bytes: 1 << 22,
            comparison_bytes: 1 << 24,
            diagnostics: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    HostMemory,
    Capacity { requested: u64, available: u64 },
    Invalid(&'static str),
    Limit(&'static str),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataError {
    pub kind: ErrorKind,
    /// Only names supplied by the schema appear here. Map keys and input excerpts are private.
    pub path: Vec<String>,
    pub span: Span,
    pub related: Option<Span>,
}
impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} at {} (bytes {}..{})",
            self.kind,
            self.path.join("."),
            self.span.start,
            self.span.end
        )
    }
}
impl std::error::Error for DataError {}
impl DataError {
    pub fn resource(kind: ErrorKind) -> Self {
        error(kind, Span::default())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Shape {
    Scalar(Scalar),
    Unit,
    List(Box<Shape>),
    Map(Box<Shape>),
    Option(Box<Shape>),
    Tuple(Vec<Shape>),
    Named(usize),
}
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scalar {
    Bool,
    Char,
    String,
    Bytes,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub shape: Shape,
    pub default: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Fields {
    Labelled(Vec<Field>),
    Positional(Vec<Shape>),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    pub name: String,
    pub fields: Fields,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Body {
    Record(Vec<Field>),
    Sum(Vec<Case>),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nominal {
    pub module: String,
    pub name: String,
    pub body: Body,
}
#[derive(Debug, Clone, Serialize)]
pub struct Descriptor {
    module: String,
    types: Vec<Nominal>,
    #[serde(skip)]
    digest: String,
}
impl Descriptor {
    pub fn module(&self) -> &str {
        &self.module
    }
    pub fn root(&self) -> &str {
        &self.types[0].name
    }
    pub fn types(&self) -> &[Nominal] {
        &self.types
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&(1u8, &self.module, &self.types)).unwrap()
    }
}

#[derive(Debug)]
pub struct Document<'s> {
    pub schema: &'s Descriptor,
    pub value: Value,
    syntax: SyntaxFile,
    index: read::Index,
}
impl Document<'_> {
    pub fn syntax(&self) -> &SyntaxFile {
        &self.syntax
    }
    pub fn projection(&self) -> Value {
        serde_json::json!({"version": 1, "root": format!("{}.{}", self.schema.module(),self.schema.root()),
                            "schema_digest": self.schema.digest(), "value": self.value})
    }
}

#[derive(Clone, Copy)]
struct Site<'a>(Item<'a>, NodeId);
impl<'a> Site<'a> {
    fn kids(self) -> impl ExactSizeIterator<Item = Site<'a>> + Clone {
        self.0
            .tree
            .children(self.1)
            .iter()
            .map(move |&id| Self(self.0, id))
    }
    fn kind(self) -> NodeKind {
        self.0.tree.node(self.1).kind
    }
    fn token(self) -> Option<&'a str> {
        self.0
            .tree
            .node(self.1)
            .token
            .map(|t| self.0.tree.token_text(self.0.text, t))
    }
    fn span(self) -> Span {
        let relative = self.0.tree.node(self.1).origin.span;
        Span {
            start: self.0.file_offset + relative.start,
            end: self.0.file_offset + relative.end,
        }
    }
    fn words(self) -> impl Iterator<Item = &'a str> {
        self.0
            .tree
            .node(self.1)
            .origin
            .tokens
            .clone()
            .filter(move |&i| !self.0.tree.tokens[i as usize].kind.is_trivia())
            .map(move |i| self.0.tree.token_text(self.0.text, TokenId(i)))
    }
    fn error(self, msg: &'static str) -> DataError {
        error(ErrorKind::Invalid(msg), self.span())
    }
}
fn error(kind: ErrorKind, span: Span) -> DataError {
    DataError {
        kind,
        span,
        related: None,
        path: Vec::new(),
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
hirsel_guard::install!();

//! Lossless indexed syntax for the new core language, independent of semantic checking.
//! Arenas and relative origins are extracted from hirsel-syntax; grammar nodes are new.

mod grammar;
mod lexer;
mod literals;
mod parser;
mod token;
pub use lexer::{LexOutput, lex};
pub use literals::{byte_value, character_value};
pub use parser::{ParseMode, ParseOutput, ResourceRefusal, parse};
use std::ops::Range;
pub use token::*;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

macro_rules! ids { ($($name:ident),*) => {$(
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Serialize, serde::Deserialize)]
    pub struct $name(pub u32);
)*}; }
ids!(NodeId, TokenId);

/// Byte offsets relative to an item's owned text, or to the file for file diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Origin {
    pub span: Span,
    pub tokens: Range<u32>,
}

/// PROG-7: exactly one rule and primary site. The caller supplies the module identity.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    pub rule: String,
    pub primary: Span,
    pub causal: Vec<Span>,
    pub declared: Option<String>,
    pub required: Option<String>,
    pub message: String,
}
impl Diagnostic {
    pub fn error(rule: &str, primary: Span, message: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            primary,
            causal: vec![],
            declared: None,
            required: None,
            message: message.into(),
        }
    }
}

/// Node roles describe only written syntax. Modes, representations and summaries are not inferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeKind {
    Function,
    TypeDecl,
    StableType,
    ResourceType,
    ScopedType,
    Constant,
    Public,
    Opaque,
    Attribute,
    Generics,
    Generic,
    RegionGeneric,
    Bounds,
    BoundName,
    Parameters,
    Parameter,
    TakeParameter,
    MutParameter,
    ResultType,
    Uses,
    Waits,
    Outlives,
    TypePath,
    TypeArguments,
    UnitType,
    GroupType,
    TupleType,
    CallableType,
    ScopedCallableType,
    OwnCallableType,
    Brand,
    FreshBrand,
    Anchor,
    Fields,
    Field,
    Variants,
    Variant,
    Payload,
    Drop,
    Block,
    Let,
    LetMut,
    Assign,
    ExpressionStatement,
    Discard,
    Path,
    Literal,
    Unit,
    Group,
    Tuple,
    Call,
    Argument,
    LabelledArgument,
    BytesArgument,
    Index,
    Projection,
    Try,
    Placement,
    Unary,
    Binary,
    BindPattern,
    TakePattern,
    Wildcard,
    TuplePattern,
    UnitPattern,
    ConstructorPattern,
    PatternField,
    RestPattern,
    LiteralPattern,
    RangePattern,
    If,
    Match,
    Arm,
    ConditionLet,
    Loop,
    While,
    For,
    ForTake,
    Region,
    Bound,
    Return,
    Break,
    Continue,
    Labelled,
    Closure,
    ScopedClosure,
    Error,
}
/// `token` is the defining name, operator, literal or projection token, according to kind.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub kind: NodeKind,
    pub token: Option<TokenId>,
    pub origin: Origin,
    pub children: Range<u32>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tree {
    pub tokens: Vec<Token>,
    pub nodes: Vec<Node>,
    pub edges: Vec<NodeId>,
}
impl Tree {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        let r = &self.node(id).children;
        &self.edges[r.start as usize..r.end as usize]
    }
    pub fn token_text<'a>(&self, text: &'a str, token: TokenId) -> &'a str {
        let s = self.tokens[token.0 as usize].span;
        &text[s.start as usize..s.end as usize]
    }
}
/// Item-local text and arena remain identical when preceding items move.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub file_offset: u32,
    pub text: String,
    pub tree: Tree,
    pub root: NodeId,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SyntaxFile {
    pub items: Vec<Item>,
    pub trailing: String,
}
impl SyntaxFile {
    pub fn text(&self) -> String {
        self.items
            .iter()
            .map(|i| i.text.as_str())
            .chain([self.trailing.as_str()])
            .collect()
    }
}

//! Lossless indexed syntax for the new core language, independent of semantic checking.
//! Arenas and relative origins are extracted from hirsel-syntax; grammar nodes are new.

mod format;
mod grammar;
mod lexer;
mod literals;
mod parser;
mod symbols;
mod token;
pub use format::format;
pub use lexer::{LexOutput, lex};
pub use literals::{byte_value, character_value};
pub use parser::{
    PARSER_NESTING_LIMIT, ParseMode, ParseOutput, ResourceRefusal, parse, parse_bytes,
};
use std::ops::Range;
pub use symbols::{Symbol, Symbols, keyword_symbol, sym};
pub use token::*;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod source_tests;
#[cfg(test)]
mod tests;

macro_rules! ids { ($($name:ident),*) => {$(
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Serialize, serde::Deserialize)]
    pub struct $name(pub u32);
)*}; }
ids!(NodeId, TokenId);

/// Byte offsets relative to an item's owned text, or to the file for file diagnostics.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
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
    /// The written fact and the fact required by this particular rule check.
    pub fn with_comparison(
        mut self,
        declared: impl Into<String>,
        required: impl Into<String>,
    ) -> Self {
        self.declared = Some(declared.into());
        self.required = Some(required.into());
        self
    }
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

impl Span {
    pub fn is_empty(self) -> bool {
        self.start == self.end
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
    BytesParameter,
    ResultType,
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
    Anchor,
    Fields,
    Field,
    Variants,
    Variant,
    Payload,
    Drop,
    /// `clone[..](x: T) { .. }`: a core item, the own copy of core type `T` (MEM-17),
    /// declared in the core module that holds `T`'s storage kernels.
    CloneHook,
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
    Array,
    RepeatArray,
    Call,
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
/// Upper bounds for resolver tables, accumulated while nodes are appended.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResolutionSize {
    pub names: u32,
    pub path_extra: u32,
    pub exits: u32,
    pub binders: u32,
    pub scopes: u32,
    pub loops: u32,
    pub labels: u32,
}
impl ResolutionSize {
    pub(crate) fn add(&mut self, kind: NodeKind) {
        match kind {
            NodeKind::Path | NodeKind::TypePath => self.names += 1,
            NodeKind::Break | NodeKind::Continue => self.exits += 1,
            NodeKind::BindPattern => {
                self.names += 1;
                self.binders += 1;
            }
            NodeKind::TakePattern
            | NodeKind::Generic
            | NodeKind::RegionGeneric
            | NodeKind::Parameter
            | NodeKind::TakeParameter
            | NodeKind::MutParameter
            | NodeKind::BytesParameter => self.binders += 1,
            NodeKind::Region | NodeKind::Drop => {
                self.binders += 1;
                self.scopes += 1;
            }
            NodeKind::Labelled => {
                self.binders += 1;
                self.labels += 1;
            }
            NodeKind::Loop | NodeKind::While | NodeKind::For | NodeKind::ForTake => {
                self.loops += 1;
                self.scopes += 1;
            }
            NodeKind::Function
            | NodeKind::CloneHook
            | NodeKind::Closure
            | NodeKind::ScopedClosure
            | NodeKind::CallableType
            | NodeKind::ScopedCallableType
            | NodeKind::OwnCallableType
            | NodeKind::Block
            | NodeKind::TypeDecl
            | NodeKind::StableType
            | NodeKind::ResourceType
            | NodeKind::ScopedType
            | NodeKind::Arm
            | NodeKind::If => self.scopes += 1,
            _ => {}
        }
    }
}
/// One declaration's range in a file arena. Origins retain item-relative byte sites.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemRange {
    pub resolution_size: ResolutionSize,
    /// Syntax-refused items retain text but skip semantic passes.
    pub has_errors: bool,
    pub file_offset: u32,
    pub end: u32,
    pub tokens: Range<u32>,
    pub nodes: Range<u32>,
    pub root: NodeId,
}
/// A borrowed item view. Token/node IDs address the file, never temporary objects.
#[derive(Clone, Copy, Debug)]
pub struct Item<'a> {
    pub file_offset: u32,
    pub text: &'a str,
    pub tree: &'a Tree,
    pub range: &'a ItemRange,
    pub root: NodeId,
}
impl Item<'_> {
    pub fn source_text(&self) -> &str {
        &self.text[self.file_offset as usize..self.range.end as usize]
    }
    pub fn token_span(&self, t: TokenId) -> Span {
        let s = self.tree.tokens[t.0 as usize].span;
        Span {
            start: s.start - self.file_offset,
            end: s.end - self.file_offset,
        }
    }
    pub fn symbol(&self, t: TokenId) -> Symbol {
        self.tree.tokens[t.0 as usize].symbol
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SyntaxFile {
    pub source: String,
    pub tree: Tree,
    pub items: Vec<ItemRange>,
    pub trailing_start: u32,
}
impl SyntaxFile {
    /// Recover every source byte, including whitespace gaps between token spans.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.source.len());
        let mut start = 0;
        for t in &self.tree.tokens {
            out.push_str(&self.source[start..t.span.end as usize]);
            start = t.span.end as usize;
        }
        out.push_str(&self.source[start..]);
        out
    }
    pub fn trailing(&self) -> &str {
        &self.source[self.trailing_start as usize..]
    }
    pub fn item(&self, i: usize) -> Item<'_> {
        let range = &self.items[i];
        Item {
            file_offset: range.file_offset,
            text: &self.source,
            tree: &self.tree,
            range,
            root: range.root,
        }
    }
    pub fn items(&self) -> impl ExactSizeIterator<Item = Item<'_>> {
        self.items.iter().map(|range| Item {
            file_offset: range.file_offset,
            text: &self.source,
            tree: &self.tree,
            range,
            root: range.root,
        })
    }
}

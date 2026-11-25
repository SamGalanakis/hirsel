//! Recovery, significant-token cursor, precedence climbing and item-local arena/origin
//! mechanics adapted from hirsel-parser. Every grammar visitor is for the new language.
mod budget;
mod recovery;
mod subject;

use crate::{grammar::*, *};
type Nodes = smallvec::SmallVec<[NodeId; 24]>;
use smallvec::smallvec;
type Parsed<T> = Result<T, ()>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Header {
    Function,
    Callable,
    Closure,
}

/// Maximum simultaneously active recursive parser productions, not a language limit.
pub const PARSER_NESTING_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ParseMode {
    Source,
    Signatures,
    Foreign,
}
/// Incomplete compiler outcome, never a source-language diagnostic (ARCH front end).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceRefusal {
    /// A source of `bytes` bytes, past the u32-indexed byte capacity; its site is the
    /// start of the file, since no u32 span reaches the byte past the capacity.
    SourceTooLarge {
        bytes: u64,
    },
    ParserStack {
        at: Span,
        limit: usize,
    },
}
impl ResourceRefusal {
    /// File-relative byte site of the compiler's resource exhaustion.
    pub fn site(&self) -> Span {
        match self {
            Self::SourceTooLarge { .. } => Span::default(),
            Self::ParserStack { at, .. } => *at,
        }
    }
}
impl std::fmt::Display for ResourceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceTooLarge { bytes } => write!(
                f,
                "source of {bytes} bytes exceeds the u32-indexed byte capacity of {} bytes",
                u32::MAX
            ),
            Self::ParserStack { at, limit } => write!(
                f,
                "parser nesting limit of {limit} active productions reached at bytes {}..{}",
                at.start, at.end
            ),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ParseOutput {
    pub syntax: SyntaxFile,
    pub diagnostics: Vec<Diagnostic>,
    pub resource_refusal: Option<ResourceRefusal>,
}

/// Decode external source bytes. Invalid UTF-8 has a raw byte span (PROG-7).
pub fn parse_bytes(bytes: &[u8], mode: ParseMode, symbols: &mut Symbols) -> ParseOutput {
    match std::str::from_utf8(bytes) {
        Ok(text) => parse(text, mode, symbols),
        Err(error) => {
            if bytes.len() > u32::MAX as usize {
                return ParseOutput {
                    syntax: SyntaxFile::default(),
                    diagnostics: vec![],
                    resource_refusal: Some(ResourceRefusal::SourceTooLarge {
                        bytes: bytes.len() as u64,
                    }),
                };
            }
            let start = error.valid_up_to();
            let end = start + error.error_len().unwrap_or(bytes.len() - start);
            ParseOutput {
                syntax: SyntaxFile::default(),
                diagnostics: vec![Diagnostic::error(
                    "LANG-1",
                    Span {
                        start: start as u32,
                        end: end as u32,
                    },
                    "source is not UTF-8",
                )],
                resource_refusal: None,
            }
        }
    }
}

pub fn parse(text: &str, mode: ParseMode, symbols: &mut Symbols) -> ParseOutput {
    parse_derived(text, mode, symbols, usize::MAX)
}
/// Parse `text` whose bytes from `derived` on are generated (see `lexer::lex_derived`).
pub(crate) fn parse_derived(
    text: &str,
    mode: ParseMode,
    symbols: &mut Symbols,
    derived: usize,
) -> ParseOutput {
    if text.len() > u32::MAX as usize {
        return ParseOutput {
            syntax: SyntaxFile::default(),
            diagnostics: vec![],
            resource_refusal: Some(ResourceRefusal::SourceTooLarge {
                bytes: text.len() as u64,
            }),
        };
    }
    let output = crate::lexer::lex_derived(text, symbols, derived);
    let lexical_diagnostics = output.diagnostics.len();
    let significant = output.significant;
    let canonical = output.canonical;
    let canonical_offsets = output.canonical_offsets;
    let node_capacity = significant.len();
    let mut p = Parser {
        text,
        symbols,
        current: output.tokens[significant[0]],
        current_index: significant[0],
        resolution_size: ResolutionSize::default(),
        tokens: output.tokens,
        significant,
        pos: 0,
        tree: Tree {
            tokens: vec![],
            nodes: Vec::with_capacity(node_capacity),
            edges: Vec::with_capacity(node_capacity),
            canonical,
            canonical_offsets,
        },
        base: 0,
        whitespace: 0,
        depth: 0,
        delimiter_pos: 0,
        delimiters: smallvec![],
        delimiter_openings: [Vec::new(), Vec::new(), Vec::new()],
        diagnostics: output.diagnostics,
        resource: None,
        mode,
    };
    let mut syntax = SyntaxFile::default();
    let mut owned = 0;
    let mut byte_owned = 0;
    while !p.eof() && p.resource.is_none() {
        if p.eat(sym::SEMICOLON) {
            continue;
        }
        let start = p.pos;
        p.delimiter_pos = start;
        p.clear_delimiters();
        let diagnostic_start = p.diagnostics.len();
        let file_offset = p.tokens[p.current_index].span.start;
        p.base = file_offset;
        let nodes_start = p.tree.nodes.len() as u32;
        p.resolution_size = ResolutionSize::default();
        let root = match p.item() {
            Ok(root) => {
                if p.end(false).is_err() {
                    p.recover(start, true);
                }
                root
            }
            Err(()) => {
                p.recover(start, true);
                p.node(start, NodeKind::Error, None, &[])
            }
        };
        let mut end = p.index();
        let consumed = p
            .significant
            .get(p.pos.saturating_sub(1))
            .copied()
            .map_or(owned, |i| i + 1);
        let mut byte_end = p.tokens[end].span.start;
        // Split the first line break after the item, retaining following documentation.
        // Whitespace occupies source gaps instead of rows in the token table.
        for i in consumed..=end {
            let gap_start = if i == 0 { 0 } else { p.tokens[i - 1].span.end } as usize;
            let next_start = p.tokens[i].span.start as usize;
            if let Some(newline) = text[gap_start..next_start].find('\n') {
                end = i;
                byte_end = (gap_start + newline + 1) as u32;
                break;
            }
        }
        let has_errors = p.resource.is_some()
            || p.diagnostics.len() != diagnostic_start
            || p.diagnostics[..lexical_diagnostics].iter().any(|d| {
                d.primary.start >= byte_owned
                    && (d.primary.start < byte_end
                        || byte_end as usize == text.len() && d.primary.start == byte_end)
            });
        syntax.items.push(ItemRange {
            resolution_size: p.resolution_size,
            has_errors,
            owned: byte_owned..byte_end,
            file_offset,
            end: byte_end,
            tokens: owned as u32..end as u32,
            nodes: nodes_start..p.tree.nodes.len() as u32,
            root,
        });
        owned = end;
        byte_owned = byte_end;
    }
    syntax.trailing_start = byte_owned;
    syntax.source = text.to_owned();
    p.tree.tokens = p.tokens;
    syntax.tree = p.tree;
    ParseOutput {
        syntax,
        diagnostics: p.diagnostics,
        resource_refusal: p.resource,
    }
}

struct Parser<'a> {
    text: &'a str,
    symbols: &'a Symbols,
    tokens: Vec<Token>,
    significant: Vec<usize>,
    current: Token,
    current_index: usize,
    resolution_size: ResolutionSize,
    pos: usize,
    tree: Tree,
    base: u32,
    whitespace: usize,
    depth: usize,
    delimiter_pos: usize,
    delimiters: smallvec::SmallVec<[(usize, Symbol); 8]>,
    delimiter_openings: [Vec<usize>; 3],
    diagnostics: Vec<Diagnostic>,
    resource: Option<ResourceRefusal>,
    mode: ParseMode,
}

impl Parser<'_> {
    fn index(&self) -> usize {
        self.current_index
    }
    fn token(&self, n: usize) -> &Token {
        if n == 0 {
            &self.current
        } else {
            &self.tokens[self.significant[(self.pos + n).min(self.significant.len() - 1)]]
        }
    }
    fn word(&self, n: usize) -> Symbol {
        self.token(n).symbol
    }
    fn at(&self, word: Symbol) -> bool {
        self.current.symbol == word
    }
    fn eof(&self) -> bool {
        self.token(0).kind == TokenKind::Eof
    }
    #[inline(always)]
    fn bump(&mut self) -> TokenId {
        let id = TokenId(self.index() as u32);
        if !self.eof() {
            self.pos += 1;
            self.current_index = self.significant[self.pos];
            self.current = self.tokens[self.current_index];
        }
        id
    }
    #[inline(always)]
    fn eat(&mut self, word: Symbol) -> bool {
        if self.at(word) {
            self.bump();
            true
        } else {
            false
        }
    }
    #[inline]
    fn expect(&mut self, word: Symbol) -> Parsed<()> {
        if self.eat(word) {
            Ok(())
        } else {
            self.expected(word)
        }
    }
    fn fail_expected<T>(
        &mut self,
        rule: &str,
        required: &str,
        message: impl Into<String>,
    ) -> Parsed<T> {
        if self.newline() && line_continuation(self.word(0)) {
            return self.fail("LANG-4", "this position cannot continue across a newline");
        }
        let declared = if self.eof() {
            "end of file"
        } else {
            self.text_token(self.index())
        };
        let d = Diagnostic::error(rule, self.token(0).span, message)
            .with_comparison(declared, required);
        self.diagnostics.push(d);
        Err(())
    }
    fn fail<T>(&mut self, rule: &str, message: impl Into<String>) -> Parsed<T> {
        self.diagnostics
            .push(Diagnostic::error(rule, self.token(0).span, message));
        Err(())
    }
    #[inline]
    fn name(&mut self) -> Parsed<TokenId> {
        if self.token(0).kind == TokenKind::Ident && !self.at(sym::UNDERSCORE) {
            Ok(self.bump())
        } else {
            self.name_error()
        }
    }
    #[cold]
    fn name_error(&mut self) -> Parsed<TokenId> {
        self.fail_expected(
            if self.token(0).kind.is_keyword() {
                "LANG-2"
            } else {
                "LANG-5"
            },
            "non-keyword identifier",
            self.name_message(),
        )
    }
    fn newline(&self) -> bool {
        self.token(0).line_before
    }
    fn continuing(&self) -> bool {
        self.whitespace > 0 || !self.newline()
    }
    fn bare_exit(&self, node: NodeId) -> bool {
        matches!(
            self.tree.node(node).kind,
            NodeKind::Return | NodeKind::Break | NodeKind::Continue
        ) && self.tree.children(node).is_empty()
    }
    #[inline(always)]
    fn node(
        &mut self,
        start: usize,
        kind: NodeKind,
        token: Option<TokenId>,
        children: &[NodeId],
    ) -> NodeId {
        self.resolution_size.add(kind);
        let first = self.significant[start];
        let last = if self.pos > start {
            self.significant[self.pos - 1] + 1
        } else {
            first
        };
        let span = Span {
            start: self.tokens[first].span.start - self.base,
            end: if last > first {
                self.tokens[last - 1].span.end - self.base
            } else {
                self.tokens[first].span.start - self.base
            },
        };
        let edge = self.tree.edges.len() as u32;
        self.tree.edges.extend_from_slice(children);
        let id = NodeId(self.tree.nodes.len() as u32);
        self.tree.nodes.push(Node {
            kind,
            token,
            origin: Origin {
                span,
                tokens: first as u32..last as u32,
            },
            children: edge..self.tree.edges.len() as u32,
        });
        id
    }
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Parsed<T>) -> Parsed<T> {
        // A finite compiler stack allocation, not a language nesting limit. The incomplete
        // result is separate from refusals and callers must not publish it as parsed source.
        if self.depth >= PARSER_NESTING_LIMIT {
            self.resource = Some(ResourceRefusal::ParserStack {
                at: self.token(0).span,
                limit: PARSER_NESTING_LIMIT,
            });
            return Err(());
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
    }
    fn required<T>(&mut self, f: impl FnOnce(&mut Self) -> Parsed<T>) -> Parsed<T> {
        f(self)
    }
    fn list(&mut self, close: Symbol, f: impl FnMut(&mut Self) -> Parsed<NodeId>) -> Parsed<Nodes> {
        let mut nodes = smallvec![];
        self.list_into(close, &mut nodes, f)?;
        Ok(nodes)
    }
    fn list_into(
        &mut self,
        close: Symbol,
        nodes: &mut Nodes,
        mut f: impl FnMut(&mut Self) -> Parsed<NodeId>,
    ) -> Parsed<()> {
        self.whitespace += 1;
        let result = (|p: &mut Self| {
            while !p.eat(close) {
                if p.eof() {
                    return p.expected(close);
                }
                nodes.push(f(p)?);
                if p.eat(close) {
                    break;
                }
                p.expect(sym::COMMA)?;
            }
            Ok(())
        })(self);
        self.whitespace -= 1;
        result
    }
    fn entry_end(&mut self, close: Symbol) -> Parsed<()> {
        if self.at(close) || self.eat(sym::COMMA) || self.newline() {
            Ok(())
        } else {
            self.fail_expected("LANG-4", "comma or newline", "expected comma or newline")
        }
    }
    fn end(&mut self, implicit: bool) -> Parsed<bool> {
        if self.eat(sym::SEMICOLON) {
            Ok(true)
        } else if self.eof() || self.at(sym::RBRACE) || self.newline() || implicit {
            Ok(false)
        } else {
            self.fail_expected(
                "LANG-4",
                "newline or semicolon",
                "expected newline or semicolon",
            )
        }
    }
    fn item_start(&self) -> bool {
        (ITEM_START.contains(&self.word(0)) || (self.at(sym::COLD) && self.word(1) == sym::FN))
            && !(self.at(sym::FN) && self.word(1) == sym::LPAREN)
            && !(self.at(sym::SCOPED) && self.word(1) == sym::FN)
    }
    fn text_token(&self, i: usize) -> &str {
        let s = self.tokens[i].span;
        &self.text[s.start as usize..s.end as usize]
    }
    fn item(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        let mut c: Nodes = smallvec![];
        if self.mode == ParseMode::Foreign {
            if self.at(sym::AT) {
                return self.fail("FFI-2", "a foreign function has no attribute");
            }
            if !self.at(sym::PUB) || self.word(1) != sym::FN {
                return self.fail("FFI-2", "a foreign module contains only pub fn headers");
            }
        }
        while self.eat(sym::AT) {
            let a = self.pos - 1;
            let n = self.name()?;
            let word = self.tokens[n.0 as usize].symbol;
            if word == sym::COMPACT {
                self.expect(sym::LPAREN)?;
                if !matches!(self.word(0), sym::U16 | sym::U32 | sym::U64) {
                    return self.fail_expected(
                        "LANG-5",
                        "u16, u32 or u64 compact width",
                        "expected compact width",
                    );
                }
                self.bump();
                self.expect(sym::RPAREN)?;
            } else if word != sym::TEST {
                self.diagnostics.push(
                    Diagnostic::error(
                        "LANG-5",
                        self.tokens[n.0 as usize].span,
                        "unknown attribute",
                    )
                    .with_comparison(self.symbols.text(word), "test or compact attribute"),
                );
                return Err(());
            }
            c.push(self.node(a, NodeKind::Attribute, Some(n), &[]));
        }
        if self.eat(sym::PUB) {
            c.push(self.node(self.pos - 1, NodeKind::Public, None, &[]));
            if self.eat(sym::OPAQUE) {
                c.push(self.node(self.pos - 1, NodeKind::Opaque, None, &[]));
            }
        }
        // `cold` is contextual: it marks the function item that follows (LANG-8).
        if self.at(sym::COLD) {
            if self.word(1) != sym::FN {
                return self.fail(
                    "LANG-5",
                    "`cold` stands only before `fn` in a function item",
                );
            }
            self.bump();
            c.push(self.node(self.pos - 1, NodeKind::Cold, None, &[]));
        }
        if self.eat(sym::FN) {
            let name = self.name()?;
            c.extend(self.header(Header::Function)?);
            if self.at(sym::LBRACE) {
                if self.mode == ParseMode::Foreign {
                    return self.fail("FFI-2", "a foreign function has no body");
                }
                c.push(self.block()?);
            } else if self.mode == ParseMode::Source {
                let end = self.tokens[self.significant[self.pos - 1]].span.end;
                self.diagnostics.push(
                    Diagnostic::error(
                        "LANG-5",
                        Span { start: end, end },
                        "function requires a body",
                    )
                    .with_comparison("function header without a body", "function body"),
                );
                return Err(());
            }
            return Ok(self.node(s, NodeKind::Function, Some(name), &c));
        }
        // `clone` is contextual: an item starting `clone [` or `clone (` is a clone body.
        if self.at(sym::CLONE) && matches!(self.word(1), sym::LBRACKET | sym::LPAREN) {
            let name = self.name()?;
            c.extend(self.header(Header::Function)?);
            c.push(self.block()?);
            return Ok(self.node(s, NodeKind::CloneHook, Some(name), &c));
        }
        if self.eat(sym::CONST) {
            let name = self.name()?;
            self.expect(sym::COLON)?;
            c.push(self.ty()?);
            self.expect(sym::EQ)?;
            c.push(self.required(|p| p.expr(0))?);
            return Ok(self.node(s, NodeKind::Constant, Some(name), &c));
        }
        let kind = if self.eat(sym::RESOURCE) {
            NodeKind::ResourceType
        } else if self.eat(sym::SCOPED) {
            NodeKind::ScopedType
        } else {
            NodeKind::TypeDecl
        };
        self.expect(sym::TYPE)?;
        let name = self.name()?;
        if self.at(sym::LBRACKET) {
            c.push(self.generics()?);
        }
        if self.eat(sym::COLON) {
            c.push(self.bounds(false)?);
            if self.eat(sym::VIA) {
                let start = self.pos - 1;
                let ty = self.ty()?;
                self.expect(sym::LPAREN)?;
                self.expect(sym::TO)?;
                self.expect(sym::COLON)?;
                let start_to = self.pos;
                let to_name = self.name()?;
                let to = self.node(start_to, NodeKind::Path, Some(to_name), &[]);
                self.expect(sym::COMMA)?;
                self.expect(sym::FROM)?;
                self.expect(sym::COLON)?;
                let start_from = self.pos;
                let from_name = self.name()?;
                let from = self.node(start_from, NodeKind::Path, Some(from_name), &[]);
                self.expect(sym::RPAREN)?;
                c.push(self.node(start, NodeKind::CodecVia, None, &[ty, to, from]));
            }
        }
        if self.eat(sym::GRANT) {
            let st = self.pos - 1;
            self.expect(sym::BYTES)?;
            if self.token(0).kind != TokenKind::Integer {
                return self.fail_expected(
                    "LANG-5",
                    "integer literal grant size",
                    "expected grant size",
                );
            }
            let t = self.bump();
            c.push(self.node(st, NodeKind::Literal, Some(t), &[]));
        }
        if self.eat(sym::LBRACE) {
            let st = self.pos - 1;
            let mut fields: Nodes = smallvec![];
            while !self.eat(sym::RBRACE) {
                if self.eof() {
                    return self.expected(sym::RBRACE);
                }
                let fs = self.pos;
                let removed = self.at(sym::REMOVED) && self.word(1) != sym::COLON;
                if removed {
                    self.bump();
                }
                let n = self.name()?;
                self.expect(sym::COLON)?;
                let t = self.ty()?;
                let mut children: Nodes = smallvec![t];
                if self.eat(sym::EQ) {
                    children.push(self.field_default()?);
                }
                fields.push(self.node(
                    fs,
                    if removed {
                        NodeKind::RemovedField
                    } else {
                        NodeKind::Field
                    },
                    Some(n),
                    &children,
                ));
                self.entry_end(sym::RBRACE)?;
            }
            c.push(self.node(st, NodeKind::Fields, None, &fields));
        } else if self.eat(sym::EQ) {
            let st = self.pos;
            if self.at(sym::PIPE) {
                return self.fail(
                    "LANG-4",
                    "a variant list needs its first variant before `|`",
                );
            }
            // LANG-9: a single bare name aliases; payload parentheses or | makes a variant.
            if (self.at(sym::REMOVED) && self.token(1).kind == TokenKind::Ident)
                || (self.token(0).kind == TokenKind::Ident
                    && (self.word(1) == sym::PIPE
                        || (self.word(1) == sym::LPAREN && !self.token(1).line_before)))
            {
                let mut vs: Nodes = smallvec![self.variant()?];
                while self.eat(sym::PIPE) {
                    vs.push(self.variant()?);
                }
                c.push(self.node(st, NodeKind::Variants, None, &vs));
            } else {
                c.push(self.ty()?);
            }
        }
        if self.at(sym::DROP) {
            let st = self.pos;
            self.bump();
            self.expect(sym::LPAREN)?;
            let n = self.name()?;
            self.expect(sym::RPAREN)?;
            let body = self.block()?;
            c.push(self.node(st, NodeKind::Drop, Some(n), &[body]));
        }

        Ok(self.node(s, kind, Some(name), &c))
    }
    /// SEM-10 uses the scalar grammar, not a constant expression.
    fn field_default(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.at(sym::MINUS) {
            let op = self.bump();
            if self.token(0).kind != TokenKind::Integer {
                return self.fail_expected(
                    "LANG-5",
                    "integer literal",
                    "a negative field default is an integer literal",
                );
            }
            let value = self.primary()?;
            return Ok(self.node(s, NodeKind::Unary, Some(op), &[value]));
        }
        if matches!(
            self.token(0).kind,
            TokenKind::Integer
                | TokenKind::Float
                | TokenKind::Character
                | TokenKind::Byte
                | TokenKind::String
                | TokenKind::Bytes
                | TokenKind::KwTrue
                | TokenKind::KwFalse
        ) {
            self.primary()
        } else {
            self.fail_expected("LANG-5", "literal", "a field default is a literal")
        }
    }
    fn variant(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        let removed = self.at(sym::REMOVED) && self.token(1).kind == TokenKind::Ident;
        if removed {
            self.bump();
        }
        let n = self.name()?;
        let mut c: Nodes = smallvec![];
        let mut labelled = None;
        if self.continuing() && self.eat(sym::LPAREN) {
            c = self.list(sym::RPAREN, |p| {
                let st = p.pos;
                let label = if p.token(0).kind == TokenKind::Ident && p.word(1) == sym::COLON {
                    let n = p.name()?;
                    p.expect(sym::COLON)?;
                    Some(n)
                } else {
                    None
                };
                if labelled.is_some_and(|old| old != label.is_some()) {
                    p.diagnostics.push(
                        Diagnostic::error("LANG-9", p.token(0).span, "mixed payload labels")
                            .with_comparison(
                                if label.is_some() {
                                    "labelled payload"
                                } else {
                                    "positional payload"
                                },
                                if labelled == Some(true) {
                                    "labelled payload"
                                } else {
                                    "positional payload"
                                },
                            ),
                    );
                    return Err(());
                }
                labelled = Some(label.is_some());
                let ty = p.ty()?;
                Ok(p.node(st, NodeKind::Payload, label, &[ty]))
            })?;
        }
        Ok(self.node(
            s,
            if removed {
                NodeKind::RemovedVariant
            } else {
                NodeKind::Variant
            },
            Some(n),
            &c,
        ))
    }
    fn generics(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        self.expect(sym::LBRACKET)?;
        let c = self.list(sym::RBRACKET, |p| {
            let st = p.pos;
            let region = p.eat(sym::IN);
            let n = p.name()?;
            let mut c: Nodes = smallvec![];
            if p.eat(sym::COLON) {
                c.push(p.bounds(false)?);
            }
            Ok(p.node(
                st,
                if region {
                    NodeKind::RegionGeneric
                } else {
                    NodeKind::Generic
                },
                Some(n),
                &c,
            ))
        })?;
        if c.is_empty() {
            return self.fail("LANG-5", "generic list is nonempty");
        }
        Ok(self.node(s, NodeKind::Generics, None, &c))
    }
    fn bounds(&mut self, callable: bool) -> Parsed<NodeId> {
        let s = self.pos;
        let mut c: Nodes = smallvec![];
        loop {
            let st = self.pos;
            if !BOUNDS.contains(&self.word(0)) && !(callable && self.at(sym::VIEW)) {
                let required = BOUNDS
                    .iter()
                    .map(|&b| self.symbols.text(b))
                    .collect::<Vec<_>>()
                    .join(", ");
                return self.fail_expected("LANG-5", &required, "unknown bound");
            }
            let n = self.bump();
            c.push(self.node(st, NodeKind::BoundName, Some(n), &[]));
            if !self.continuing() || !self.eat(sym::PLUS) {
                break;
            }
        }
        Ok(self.node(s, NodeKind::Bounds, None, &c))
    }
    fn header(&mut self, kind: Header) -> Parsed<Nodes> {
        let mut c: Nodes = smallvec![];
        if kind == Header::Function && self.at(sym::LBRACKET) {
            c.push(self.generics()?);
        }
        let s = self.pos;
        self.expect(sym::LPAREN)?;
        let ps = self.list(sym::RPAREN, |p| p.parameter(kind))?;
        c.push(self.node(s, NodeKind::Parameters, None, &ps));
        if self.eat(sym::ARROW) {
            let st = self.pos - 1;
            let t = self.ty()?;
            c.push(self.node(st, NodeKind::ResultType, None, &[t]));
        }
        if self.eat(sym::WAITS) {
            c.push(self.node(self.pos - 1, NodeKind::Waits, None, &[]));
        }
        if kind == Header::Function && self.eat(sym::WHERE) {
            let st = self.pos - 1;
            let mut cs: Nodes = smallvec![];
            loop {
                cs.push(self.path(NodeKind::Path)?);
                self.expect(sym::COLON)?;
                cs.push(self.path(NodeKind::Path)?);
                if !self.eat(sym::COMMA) {
                    break;
                }
            }
            c.push(self.node(st, NodeKind::Outlives, None, &cs));
        }
        Ok(c)
    }
    fn parameter(&mut self, header: Header) -> Parsed<NodeId> {
        let s = self.pos;
        let k = if self.eat(sym::TAKE) {
            NodeKind::TakeParameter
        } else if self.eat(sym::MUT) {
            NodeKind::MutParameter
        } else if self.eat(sym::BYTES) {
            NodeKind::BytesParameter
        } else if header != Header::Closure
            && self.at(sym::STEPS)
            && self.token(1).kind == TokenKind::Ident
        {
            self.bump();
            NodeKind::StepsParameter
        } else {
            NodeKind::Parameter
        };
        if matches!(self.word(0), sym::TAKE | sym::MUT | sym::BYTES) {
            return self.fail("LANG-5", "a parameter has at most one mode");
        }
        let n = if header != Header::Callable
            || (self.token(0).kind == TokenKind::Ident && self.word(1) == sym::COLON)
        {
            let n = self.name()?;
            if header == Header::Closure && !self.at(sym::COLON) {
                return Ok(self.node(s, k, Some(n), &[]));
            }
            self.expect(sym::COLON)?;
            Some(n)
        } else {
            None
        };
        let t = self.ty()?;
        Ok(self.node(s, k, n, &[t]))
    }
    fn ty(&mut self) -> Parsed<NodeId> {
        self.nested(|p| p.ty_inner())
    }
    fn ty_inner(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if matches!(self.word(0), sym::TAKE | sym::MUT | sym::BYTES) {
            return self.fail(
                "LANG-5",
                "a parameter has at most one mode; a type has none",
            );
        }
        let mut n = if self.at(sym::FN)
            || self.at(sym::SCOPED)
            || (self.at(sym::OWN) && matches!(self.word(1), sym::FN | sym::LBRACKET))
        {
            let scoped = self.eat(sym::SCOPED);
            let own = self.eat(sym::OWN);
            let mut c: Nodes = smallvec![];
            if (own || scoped) && self.eat(sym::LBRACKET) {
                c.push(self.bounds(true)?);
                self.expect(sym::RBRACKET)?;
            }
            self.expect(sym::FN)?;
            c.extend(self.header(Header::Callable)?);
            self.node(
                s,
                if scoped {
                    NodeKind::ScopedCallableType
                } else if own {
                    NodeKind::OwnCallableType
                } else {
                    NodeKind::CallableType
                },
                None,
                &c,
            )
        } else if self.eat(sym::LPAREN) {
            let c = self.list(sym::RPAREN, |p| p.ty())?;
            let k = if c.is_empty() {
                NodeKind::UnitType
            } else if c.len() == 1 && self.word_token_before() != sym::COMMA {
                NodeKind::GroupType
            } else {
                NodeKind::TupleType
            };
            self.node(s, k, None, &c)
        } else {
            let path = self.path(NodeKind::TypePath)?;
            if self.eat(sym::LBRACKET) {
                let mut c: Nodes = smallvec![path];
                let args = self.list(sym::RBRACKET, |p| {
                    if p.eat(sym::IN) {
                        let st = p.pos - 1;
                        let path = p.path(NodeKind::Path)?;
                        Ok(p.node(st, NodeKind::Anchor, None, &[path]))
                    } else if p.token(0).kind == TokenKind::Integer {
                        let st = p.pos;
                        let token = p.bump();
                        Ok(p.node(st, NodeKind::Literal, Some(token), &[]))
                    } else {
                        p.ty()
                    }
                })?;
                if args.is_empty() {
                    return self.fail("LANG-5", "type arguments are nonempty");
                }
                c.extend(args);
                self.node(s, NodeKind::TypeArguments, None, &c)
            } else {
                path
            }
        };
        if self.eat(sym::IN) {
            let b = self.path(NodeKind::Path)?;
            n = self.node(s, NodeKind::Anchor, None, &[n, b]);
        }
        Ok(n)
    }
    fn word_token_before(&self) -> Symbol {
        if self.pos >= 2 {
            self.tokens[self.significant[self.pos - 2]].symbol
        } else {
            sym::NONE
        }
    }
    #[inline(always)]
    fn path(&mut self, kind: NodeKind) -> Parsed<NodeId> {
        self.path_inner(kind, false)
    }
    #[inline(always)]
    fn path_inner(&mut self, kind: NodeKind, expression: bool) -> Parsed<NodeId> {
        let s = self.pos;
        let n = self.name()?;
        while self.at(sym::DOT)
            && (expression || self.continuing())
            && self.token(1).kind == TokenKind::Ident
        {
            self.resolution_size.path_extra += 1;
            self.bump();
            self.name()?;
        }
        Ok(self.node(s, kind, Some(n), &[]))
    }
    fn block(&mut self) -> Parsed<NodeId> {
        self.nested(|p| p.block_inner())
    }
    fn block_inner(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        self.expect(sym::LBRACE)?;
        let old = self.whitespace;
        self.whitespace = 0;
        let mut c: Nodes = smallvec![];
        let result = (|| {
            while !self.eat(sym::RBRACE) {
                if self.eof() || self.item_start() {
                    return self.expected(sym::RBRACE);
                }
                if self.eat(sym::SEMICOLON) {
                    continue;
                }
                let st = self.pos;
                match self.statement() {
                    Ok(n) => c.push(n),
                    Err(()) => {
                        self.recover(st, false);
                        c.push(self.node(st, NodeKind::Error, None, &[]));
                    }
                }
                if self.resource.is_some() {
                    return Err(());
                }
            }
            Ok(self.node(s, NodeKind::Block, None, &c))
        })();
        self.whitespace = old;
        result
    }
    fn statement(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.eat(sym::LET) {
            let mutable = self.eat(sym::MUT);
            let mut c: Nodes = smallvec![self.pattern()?];
            let typed = self.eat(sym::COLON);
            if typed {
                let mut ty = self.ty()?;
                // A slot declaration may freeze more than one written anchor:
                // `let mut p: T in n, m = ...`.  Ordinary type positions keep
                // the single `in place` form; this loop is deliberately scoped
                // to the `let` grammar and nests the anchor nodes so all
                // existing type lowering and anchor walks remain unchanged.
                while self.tree.node(ty).kind == NodeKind::Anchor && self.eat(sym::COMMA) {
                    let root = self.path(NodeKind::Path)?;
                    ty = self.node(s, NodeKind::Anchor, None, &[ty, root]);
                }
                c.push(ty);
            }
            let initialized = self.eat(sym::EQ);
            if initialized {
                c.push(self.required(|p| p.expr(0))?);
                if self.eat(sym::ELSE) {
                    self.subject(*c.last().unwrap())?;
                    c.push(self.block()?);
                }
            }
            if !initialized
                && (!typed
                    || !matches!(
                        self.tree.node(c[0]).kind,
                        NodeKind::BindPattern | NodeKind::TakePattern
                    ))
            {
                self.diagnostics.push(Diagnostic::error(
                    "LANG-5",
                    self.tokens[self.significant[s]].span,
                    "an uninitialized slot needs a name pattern and a type",
                ));
                return Err(());
            }
            self.end(false)?;
            return Ok(self.node(
                s,
                if mutable {
                    NodeKind::LetMut
                } else {
                    NodeKind::Let
                },
                None,
                &c,
            ));
        }
        if line_continuation(self.word(0)) || ASSIGN.contains(&self.word(0)) {
            return self.fail("LANG-4", "a continuation token cannot start a statement");
        }
        let e = self.expr(0)?;
        if ASSIGN.contains(&self.word(0)) {
            if self.newline() && self.bare_exit(e) {
                return self.fail("LANG-4", "a bare exit is complete at its newline");
            }
            let op = self.bump();
            let v = self.required(|p| p.expr(0))?;
            self.end(false)?;
            return Ok(self.node(s, NodeKind::Assign, Some(op), &[e, v]));
        }
        let discard = self.end(false)?;
        Ok(self.node(
            s,
            if discard {
                NodeKind::Discard
            } else {
                NodeKind::ExpressionStatement
            },
            None,
            &[e],
        ))
    }
    fn expr(&mut self, minimum: u8) -> Parsed<NodeId> {
        self.nested(|p| p.expr_inner(minimum))
    }
    fn expr_inner(&mut self, minimum: u8) -> Parsed<NodeId> {
        let s = self.pos;
        let mut left = self.primary()?;
        let mut nonassoc = None;
        // LANG-18: a mapping propagate ends its postfix chain.
        let mut mapped = false;
        loop {
            let word = self.word(0);
            if word == sym::IN && (self.whitespace > 0 || !self.newline()) {
                return self.fail("MEM-2", "region literals are written in dest.put or emit");
            }
            let Some(next) = continuation(word) else {
                if self.whitespace > 0 && self.newline() && self.bare_exit(left) {
                    return self.fail("LANG-4", "a bare exit is complete at its newline");
                }
                break;
            };
            if self.newline() {
                if self.whitespace == 0
                    && (word == sym::MINUS
                        || matches!(next, Continuation::Call | Continuation::Index))
                {
                    break;
                }
                if self.bare_exit(left) {
                    return self.fail("LANG-4", "a bare exit is complete at its newline");
                }
            }
            if mapped && minimum <= 12 && !matches!(next, Continuation::Binary(..)) {
                return self.fail(
                    "LANG-5",
                    "`?=>` ends its postfix chain; write `(e ?=> m)` to continue it",
                );
            }
            match next {
                Continuation::Call if minimum <= 12 => {
                    self.bump();
                    let mut c: Nodes = smallvec![left];
                    self.list_into(sym::RPAREN, &mut c, Self::call_argument)?;
                    left = self.node(s, NodeKind::Call, None, &c);
                }
                Continuation::Index if minimum <= 12 => {
                    self.bump();
                    let i = self.required(|p| p.expr(0))?;
                    self.expect(sym::RBRACKET)?;
                    left = self.node(s, NodeKind::Index, None, &[left, i]);
                }
                Continuation::Projection if minimum <= 12 => {
                    self.bump();
                    if self.tree.node(left).kind == NodeKind::Literal && self.eof() {
                        let start = self.base + self.tree.node(left).origin.span.start;
                        let end = self.tokens[self.significant[self.pos - 1]].span.end;
                        self.diagnostics.push(Diagnostic::error(
                            "LANG-3",
                            Span { start, end },
                            "fractional point requires a following digit",
                        ));
                        return Err(());
                    }
                    let n = if self.token(0).kind == TokenKind::Integer {
                        let raw = self.text_token(self.index());
                        if !raw.bytes().all(|b| b.is_ascii_digit()) {
                            return self
                                .fail("LANG-5", "tuple projection is an unsuffixed decimal index");
                        }
                        self.bump()
                    } else {
                        self.name()?
                    };
                    left = self.node(s, NodeKind::Projection, Some(n), &[left]);
                }
                Continuation::Try if minimum <= 12 => {
                    self.bump();
                    left = self.node(s, NodeKind::Try, None, &[left]);
                }
                Continuation::TryMap if minimum <= 12 => {
                    let op = self.bump();
                    let mapper = self.required(|p| p.mapper())?;
                    left = self.node(s, NodeKind::Try, Some(op), &[left, mapper]);
                    mapped = true;
                }
                Continuation::Binary(prec, na) if prec >= minimum => {
                    if na && nonassoc == Some(prec) {
                        return self.fail("LANG-5", "nonassociative operators do not chain");
                    }
                    let op = self.bump();
                    let right = self.required(|p| p.expr(prec + 1))?;
                    left = self.node(s, NodeKind::Binary, Some(op), &[left, right]);
                    if na {
                        nonassoc = Some(prec);
                    }
                }
                _ => break,
            }
        }
        Ok(left)
    }
    /// LANG-18's mapper: a path, or an inline arm `|p| v` over the error payload.
    fn mapper(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.eat(sym::PIPE) {
            let pattern = self.pattern()?;
            self.expect(sym::PIPE)?;
            let value = self.required(|p| p.expr(11))?;
            return Ok(self.node(s, NodeKind::Arm, None, &[pattern, value]));
        }
        if self.token(0).kind == TokenKind::Ident {
            return self.path_inner(NodeKind::Path, true);
        }
        self.fail(
            "LANG-5",
            "a `?=>` mapper is a variant or callable path, or an arm `|p| v`",
        )
    }
    #[inline(always)]
    fn primary(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if matches!(
            self.token(0).kind,
            TokenKind::Integer
                | TokenKind::Float
                | TokenKind::Character
                | TokenKind::Byte
                | TokenKind::String
                | TokenKind::Bytes
                | TokenKind::KwTrue
                | TokenKind::KwFalse
        ) {
            let t = self.bump();
            return Ok(self.node(s, NodeKind::Literal, Some(t), &[]));
        }
        if self.token(0).kind == TokenKind::Ident {
            return self.path_inner(NodeKind::Path, true);
        }

        self.compound_primary()
    }
    fn compound_primary(&mut self) -> Parsed<NodeId> {
        self.nested(|p| p.compound_primary_inner())
    }
    fn compound_primary_inner(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.at(sym::BYTES) && self.word(1) == sym::LBRACKET {
            return self.bytes_of();
        }
        if PREFIX.contains(&self.word(0)) {
            let op = self.bump();
            let e = self.required(|p| p.expr(11))?;
            return Ok(self.node(s, NodeKind::Unary, Some(op), &[e]));
        }
        if self.token(0).kind == TokenKind::Label {
            let t = self.bump();
            self.expect(sym::COLON)?;
            let e = self.primary()?;
            if !matches!(
                self.tree.node(e).kind,
                NodeKind::Block
                    | NodeKind::Loop
                    | NodeKind::While
                    | NodeKind::For
                    | NodeKind::ForTake
            ) {
                return self.fail("LANG-5", "label needs block or loop");
            }
            return Ok(self.node(s, NodeKind::Labelled, Some(t), &[e]));
        }
        if self.eat(sym::RETURN) || self.eat(sym::BREAK) || self.eat(sym::CONTINUE) {
            let k = match self.tokens[self.significant[s]].symbol {
                sym::RETURN => NodeKind::Return,
                sym::BREAK => NodeKind::Break,
                _ => NodeKind::Continue,
            };
            let mut label = None;
            let mut c: Nodes = smallvec![];
            if !self.newline() && self.token(0).kind == TokenKind::Label {
                if k == NodeKind::Return {
                    return self.fail("LANG-5", "return has no label");
                }
                label = Some(self.bump());
            }
            if k != NodeKind::Continue
                && !self.newline()
                && !self.eof()
                && !matches!(
                    self.word(0),
                    sym::RBRACE
                        | sym::SEMICOLON
                        | sym::COMMA
                        | sym::COLON
                        | sym::RPAREN
                        | sym::RBRACKET
                        | sym::FATARROW
                )
            {
                c.push(self.expr(0)?);
            }
            return Ok(self.node(s, k, label, &c));
        }
        if self.at(sym::LBRACE) {
            return self.block();
        }
        if self.eat(sym::LBRACKET) {
            self.whitespace += 1;
            let result = (|| {
                let mut children: Nodes = smallvec![];
                if self.eat(sym::RBRACKET) {
                    return Ok(self.node(s, NodeKind::Array, None, &children));
                }
                children.push(self.expr(0)?);
                if self.eat(sym::SEMICOLON) {
                    if self.token(0).kind != TokenKind::Integer {
                        return self.fail_expected(
                            "LANG-5",
                            "integer literal repeat count",
                            "array repeat count is an integer literal",
                        );
                    }
                    let st = self.pos;
                    let token = self.bump();
                    children.push(self.node(st, NodeKind::Literal, Some(token), &[]));
                    self.expect(sym::RBRACKET)?;
                    return Ok(self.node(s, NodeKind::RepeatArray, None, &children));
                }
                while !self.eat(sym::RBRACKET) {
                    self.expect(sym::COMMA)?;
                    if self.eat(sym::RBRACKET) {
                        break;
                    }
                    children.push(self.expr(0)?);
                }
                Ok(self.node(s, NodeKind::Array, None, &children))
            })();
            self.whitespace -= 1;
            return result;
        }
        if self.eat(sym::LPAREN) {
            self.whitespace += 1;
            let result = self.parenthesized(s);
            self.whitespace -= 1;
            return result;
        }
        if self.eat(sym::IF) {
            let condition = self.condition()?;
            let yes = self.block()?;
            let mut c: Nodes = smallvec![condition, yes];
            if self.eat(sym::ELSE) {
                c.push(if self.at(sym::IF) {
                    self.primary()?
                } else {
                    self.block()?
                });
            }
            return Ok(self.node(s, NodeKind::If, None, &c));
        }
        if self.eat(sym::MATCH) {
            let subject = self.expr(0)?;
            self.subject(subject)?;
            self.expect(sym::LBRACE)?;
            let mut c: Nodes = smallvec![subject];
            while !self.eat(sym::RBRACE) {
                if self.eof() {
                    return self.expected(sym::RBRACE);
                }
                let st = self.pos;
                let mut a: Nodes = smallvec![self.pattern()?];
                if self.eat(sym::IF) {
                    a.push(self.expr(0)?);
                }
                self.expect(sym::FATARROW)?;
                a.push(self.required(|p| p.expr(0))?);
                c.push(self.node(st, NodeKind::Arm, None, &a));
                self.entry_end(sym::RBRACE)?;
            }
            return Ok(self.node(s, NodeKind::Match, None, &c));
        }
        if self.eat(sym::LOOP) {
            let b = self.block()?;
            return Ok(self.node(s, NodeKind::Loop, None, &[b]));
        }
        if self.eat(sym::WHILE) {
            let c = self.condition()?;
            let b = self.block()?;
            return Ok(self.node(s, NodeKind::While, None, &[c, b]));
        }
        if self.eat(sym::FOR) {
            let take = self.eat(sym::TAKE);
            let pat = self.pattern()?;
            self.expect(sym::IN)?;
            let value = self.expr(0)?;
            let body = self.block()?;
            return Ok(self.node(
                s,
                if take {
                    NodeKind::ForTake
                } else {
                    NodeKind::For
                },
                None,
                &[pat, value, body],
            ));
        }
        if self.eat(sym::REGION) {
            let n = self.name()?;
            let b = self.block()?;
            return Ok(self.node(s, NodeKind::Region, Some(n), &[b]));
        }
        if self.eat(sym::BOUND) {
            let unit = if self.at(sym::STEPS) {
                Some(self.bump())
            } else {
                self.expect(sym::BYTES)?;
                None
            };
            let value = self.expr(0)?;
            let body = self.block()?;
            return Ok(self.node(s, NodeKind::Bound, unit, &[value, body]));
        }
        if self.at(sym::FN) && self.word(1) != sym::LPAREN {
            return self.fail("LANG-5", "expected expression before next function");
        }
        if self.at(sym::FN) || self.at(sym::SCOPED) {
            let scoped = self.eat(sym::SCOPED);
            self.expect(sym::FN)?;
            let mut c = self.header(Header::Closure)?;
            c.push(self.block()?);
            return Ok(self.node(
                s,
                if scoped {
                    NodeKind::ScopedClosure
                } else {
                    NodeKind::Closure
                },
                None,
                &c,
            ));
        }
        self.path(NodeKind::Path)
    }
    fn parenthesized(&mut self, start: usize) -> Parsed<NodeId> {
        if self.eat(sym::RPAREN) {
            return Ok(self.node(start, NodeKind::Unit, None, &[]));
        }
        let value = self.expr(0)?;
        if self.at(sym::COLON) {
            let colon = self.bump();
            let ty = self.ty()?;
            self.expect(sym::RPAREN)?;
            return Ok(self.node(start, NodeKind::Ascription, Some(colon), &[value, ty]));
        }
        if self.eat(sym::RPAREN) {
            return Ok(self.node(start, NodeKind::Group, None, &[value]));
        }
        self.expect(sym::COMMA)?;
        let mut values = smallvec![value];
        self.list_into(sym::RPAREN, &mut values, |p| p.expr(0))?;
        Ok(self.node(start, NodeKind::Tuple, None, &values))
    }
    fn condition(&mut self) -> Parsed<NodeId> {
        if self.eat(sym::LET) {
            let s = self.pos - 1;
            let p = self.pattern()?;
            self.expect(sym::EQ)?;
            let e = self.expr(0)?;
            Ok(self.node(s, NodeKind::ConditionLet, None, &[p, e]))
        } else {
            let n = self.expr(0)?;
            self.subject(n)?;
            Ok(n)
        }
    }
    fn pattern(&mut self) -> Parsed<NodeId> {
        self.nested(|p| p.pattern_inner())
    }
    fn pattern_inner(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.eat(sym::UNDERSCORE) {
            return Ok(self.node(s, NodeKind::Wildcard, None, &[]));
        }
        if self.eat(sym::TAKE) {
            let n = self.name()?;
            return Ok(self.node(s, NodeKind::TakePattern, Some(n), &[]));
        }
        if self.eat(sym::MUT) {
            let n = self.name()?;
            return Ok(self.node(s, NodeKind::MutPattern, Some(n), &[]));
        }
        if self.eat(sym::LPAREN) {
            let c = self.list(sym::RPAREN, |p| p.pattern())?;
            return Ok(self.node(
                s,
                if c.is_empty() {
                    NodeKind::UnitPattern
                } else {
                    NodeKind::TuplePattern
                },
                None,
                &c,
            ));
        }
        if self.eat(sym::MINUS)
            || matches!(
                self.token(0).kind,
                TokenKind::Integer
                    | TokenKind::Float
                    | TokenKind::Character
                    | TokenKind::Byte
                    | TokenKind::String
                    | TokenKind::Bytes
                    | TokenKind::KwTrue
                    | TokenKind::KwFalse
            )
        {
            let t = self.bump();
            let left = self.node(s, NodeKind::LiteralPattern, Some(t), &[]);
            if self.continuing() && self.eat(sym::DOTDOTEQ) {
                let right = self.pattern()?;
                return Ok(self.node(s, NodeKind::RangePattern, None, &[left, right]));
            }
            return Ok(left);
        }
        let p = self.path(NodeKind::Path)?;
        let name = self.tree.node(p).token;
        if self.continuing() && self.eat(sym::LPAREN) {
            let mut rest = false;
            let mut c: Nodes = smallvec![p];
            c.extend(self.list(sym::RPAREN, |p| {
                let st = p.pos;
                if rest {
                    return p.fail("LANG-5", "rest pattern must be last");
                }
                if p.eat(sym::DOTDOT) {
                    rest = true;
                    return Ok(p.node(st, NodeKind::RestPattern, None, &[]));
                }
                let label = if p.token(0).kind == TokenKind::Ident && p.word(1) == sym::COLON {
                    let n = p.bump();
                    p.expect(sym::COLON)?;
                    Some(n)
                } else {
                    None
                };
                let pat = p.pattern()?;
                Ok(p.node(st, NodeKind::PatternField, label, &[pat]))
            })?);
            Ok(self.node(s, NodeKind::ConstructorPattern, None, &c))
        } else {
            let path = &self.tree.node(p).origin.tokens;
            if path.end - path.start > 1 {
                Ok(self.node(s, NodeKind::ConstructorPattern, None, &[p]))
            } else {
                Ok(self.node(s, NodeKind::BindPattern, name, &[]))
            }
        }
    }
}

//! Recovery, significant-token cursor, precedence climbing and item-local arena/origin
//! mechanics adapted from hirsel-parser. Every grammar visitor is for the new language.
use crate::{grammar::*, *};
type Parsed<T> = Result<T, ()>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseMode {
    Source,
    Signatures,
}
/// Incomplete compiler outcome, never a source-language diagnostic (ARCH front end).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceRefusal {
    SourceTooLarge,
    ParserStack { at: Span },
}
#[derive(Clone, Debug)]
pub struct ParseOutput {
    pub syntax: SyntaxFile,
    pub diagnostics: Vec<Diagnostic>,
    pub resource_refusal: Option<ResourceRefusal>,
}

/// Decode external source bytes. Invalid UTF-8 has a raw byte span (PROG-7).
pub fn parse_bytes(bytes: &[u8], mode: ParseMode) -> ParseOutput {
    match std::str::from_utf8(bytes) {
        Ok(text) => parse(text, mode),
        Err(error) => {
            if bytes.len() > u32::MAX as usize {
                return ParseOutput {
                    syntax: SyntaxFile::default(),
                    diagnostics: vec![],
                    resource_refusal: Some(ResourceRefusal::SourceTooLarge),
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

pub fn parse(text: &str, mode: ParseMode) -> ParseOutput {
    if text.len() > u32::MAX as usize {
        return ParseOutput {
            syntax: SyntaxFile::default(),
            diagnostics: vec![],
            resource_refusal: Some(ResourceRefusal::SourceTooLarge),
        };
    }
    let output = lex(text);
    let significant = output
        .tokens
        .iter()
        .enumerate()
        .filter_map(|(i, t)| (!matches!(t.kind, TokenKind::Trivia(_))).then_some(i))
        .collect();
    let mut p = Parser {
        text,
        tokens: output.tokens,
        significant,
        pos: 0,
        tree: Tree::default(),
        first: 0,
        base: 0,
        whitespace: 0,
        depth: 0,
        diagnostics: output.diagnostics,
        resource: None,
        mode,
    };
    let mut syntax = SyntaxFile::default();
    let mut owned = 0;
    while !p.eof() && p.resource.is_none() {
        if p.eat(";") {
            continue;
        }
        let start = p.pos;
        p.first = owned;
        p.base = p.tokens[owned].span.start;
        p.tree = Tree::default();
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
        // Own through the last item's line break; leading documentation belongs to the next item.
        let consumed = p
            .significant
            .get(p.pos.saturating_sub(1))
            .copied()
            .map_or(owned, |i| i + 1);
        for i in consumed..end {
            if matches!(p.tokens[i].kind, TokenKind::Trivia(TriviaKind::Newline)) {
                end = i + 1;
                break;
            }
        }
        if end <= owned {
            end = p.index();
        }
        let byte_end = p
            .tokens
            .get(end)
            .map_or(text.len() as u32, |t| t.span.start);
        p.tree.tokens = p.tokens[owned..end]
            .iter()
            .map(|t| Token {
                kind: t.kind,
                span: Span {
                    start: t.span.start - p.base,
                    end: t.span.end - p.base,
                },
            })
            .collect();
        syntax.items.push(Item {
            file_offset: p.base,
            text: text[p.base as usize..byte_end as usize].into(),
            tree: std::mem::take(&mut p.tree),
            root,
        });
        owned = end;
    }
    let trailing_start = p
        .tokens
        .get(owned)
        .map_or(text.len() as u32, |t| t.span.start);
    syntax.trailing = text[trailing_start as usize..].into();
    ParseOutput {
        syntax,
        diagnostics: p.diagnostics,
        resource_refusal: p.resource,
    }
}

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    significant: Vec<usize>,
    pos: usize,
    tree: Tree,
    first: usize,
    base: u32,
    whitespace: usize,
    depth: usize,
    diagnostics: Vec<Diagnostic>,
    resource: Option<ResourceRefusal>,
    mode: ParseMode,
}
impl Parser<'_> {
    fn index(&self) -> usize {
        self.significant[self.pos.min(self.significant.len() - 1)]
    }
    fn token(&self, n: usize) -> &Token {
        &self.tokens[self.significant[(self.pos + n).min(self.significant.len() - 1)]]
    }
    fn word(&self, n: usize) -> &str {
        let s = self.token(n).span;
        &self.text[s.start as usize..s.end as usize]
    }
    fn at(&self, word: &str) -> bool {
        !self.eof() && self.word(0) == word
    }
    fn eof(&self) -> bool {
        self.token(0).kind == TokenKind::Eof
    }
    fn bump(&mut self) -> TokenId {
        let id = TokenId((self.index() - self.first) as u32);
        if !self.eof() {
            self.pos += 1;
        }
        id
    }
    fn eat(&mut self, word: &str) -> bool {
        if self.at(word) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, word: &str) -> Parsed<()> {
        if self.eat(word) {
            Ok(())
        } else {
            self.fail("LANG-5", format!("expected `{word}`"))
        }
    }
    fn fail<T>(&mut self, rule: &str, message: impl Into<String>) -> Parsed<T> {
        self.diagnostics
            .push(Diagnostic::error(rule, self.token(0).span, message));
        Err(())
    }
    fn name(&mut self) -> Parsed<TokenId> {
        if self.token(0).kind == TokenKind::Ident && !self.at("_") {
            Ok(self.bump())
        } else {
            self.fail(
                if matches!(self.token(0).kind, TokenKind::Keyword(_)) {
                    "LANG-2"
                } else {
                    "LANG-5"
                },
                "expected a non-keyword name",
            )
        }
    }
    fn newline(&self) -> bool {
        if self.pos == 0 {
            return false;
        }
        let previous = self.significant[self.pos - 1];
        self.tokens[previous + 1..self.index()]
            .iter()
            .any(|t| self.text[t.span.start as usize..t.span.end as usize].contains('\n'))
    }
    fn continuing(&self) -> bool {
        self.whitespace > 0 || !self.newline()
    }
    fn node(
        &mut self,
        start: usize,
        kind: NodeKind,
        token: Option<TokenId>,
        children: &[NodeId],
    ) -> NodeId {
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
                tokens: (first - self.first) as u32..(last - self.first) as u32,
            },
            children: edge..self.tree.edges.len() as u32,
        });
        id
    }
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Parsed<T>) -> Parsed<T> {
        // A finite compiler stack allocation, not a language nesting limit. The incomplete
        // result is separate from refusals and callers must not publish it as parsed source.
        if self.depth >= 384 {
            self.resource = Some(ResourceRefusal::ParserStack {
                at: self.token(0).span,
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
    fn list(
        &mut self,
        close: &str,
        mut f: impl FnMut(&mut Self) -> Parsed<NodeId>,
    ) -> Parsed<Vec<NodeId>> {
        self.whitespace += 1;
        let result = (|p: &mut Self| {
            let mut v = vec![];
            while !p.eat(close) {
                if p.eof() {
                    return p.fail("LANG-5", format!("expected `{close}`"));
                }
                v.push(f(p)?);
                if p.eat(close) {
                    break;
                }
                p.expect(",")?;
            }
            Ok(v)
        })(self);
        self.whitespace -= 1;
        result
    }
    fn entry_end(&mut self, close: &str) -> Parsed<()> {
        if self.at(close) || self.eat(",") || self.newline() {
            Ok(())
        } else {
            self.fail("LANG-4", "expected comma or newline")
        }
    }
    fn end(&mut self, implicit: bool) -> Parsed<bool> {
        if self.eat(";") {
            Ok(true)
        } else if self.eof() || self.at("}") || self.newline() || implicit {
            Ok(false)
        } else {
            self.fail("LANG-4", "expected newline or semicolon")
        }
    }
    fn item_start(&self) -> bool {
        ITEM_START.contains(&self.word(0)) && !(self.at("fn") && self.word(1) == "(")
    }
    fn recover(&mut self, start: usize, item: bool) {
        let mut braces = 0usize;
        for i in start..self.pos {
            match self.text_token(self.significant[i]) {
                "{" => braces += 1,
                "}" => braces = braces.saturating_sub(1),
                _ => {}
            }
        }
        if self.pos == start && !self.eof() && (item || !self.at("}")) {
            self.bump();
        }
        while !self.eof() {
            if self.item_start() && self.newline() {
                break;
            }
            if braces == 0 {
                if !item && (self.at("}") || self.newline()) {
                    break;
                }
                if self.eat(";") {
                    break;
                }
            }
            match self.word(0) {
                "{" => braces += 1,
                "}" => braces = braces.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
    }
    fn text_token(&self, i: usize) -> &str {
        let s = self.tokens[i].span;
        &self.text[s.start as usize..s.end as usize]
    }
    fn item(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        let mut c = vec![];
        while self.eat("@") {
            let a = self.pos - 1;
            let n = self.name()?;
            let word = self.text_token(self.first + n.0 as usize);
            if word == "compact" {
                self.expect("(")?;
                if !matches!(self.word(0), "u16" | "u32" | "u64") {
                    return self.fail("LANG-5", "expected compact width");
                }
                self.bump();
                self.expect(")")?;
            } else if word == "twin" {
                self.expect("(")?;
                let path = self.path(NodeKind::TypePath)?;
                self.expect(")")?;
                c.push(self.node(a, NodeKind::Attribute, Some(n), &[path]));
                continue;
            } else if word != "test" {
                return self.fail("LANG-5", "unknown attribute");
            }
            c.push(self.node(a, NodeKind::Attribute, Some(n), &[]));
        }
        if self.eat("pub") {
            c.push(self.node(self.pos - 1, NodeKind::Public, None, &[]));
            if self.eat("opaque") {
                c.push(self.node(self.pos - 1, NodeKind::Opaque, None, &[]));
            }
        }
        if self.eat("fn") {
            let name = self.name()?;
            c.extend(self.header(true)?);
            if self.at("{") {
                c.push(self.block()?);
            } else if self.mode != ParseMode::Signatures {
                return self.fail("LANG-5", "function requires a body");
            }
            return Ok(self.node(s, NodeKind::Function, Some(name), &c));
        }
        if self.eat("const") {
            let name = self.name()?;
            self.expect(":")?;
            c.push(self.ty()?);
            self.expect("=")?;
            c.push(self.required(|p| p.expr(0))?);
            return Ok(self.node(s, NodeKind::Constant, Some(name), &c));
        }
        let kind = if self.eat("stable") {
            NodeKind::StableType
        } else if self.eat("resource") {
            NodeKind::ResourceType
        } else if self.eat("scoped") {
            NodeKind::ScopedType
        } else {
            NodeKind::TypeDecl
        };
        self.expect("type")?;
        let name = self.name()?;
        if self.at("[") {
            c.push(self.generics()?);
        }
        if self.eat(":") {
            c.push(self.bounds()?);
        }
        if self.eat("uses") {
            let st = self.pos - 1;
            let mut us = vec![self.path(NodeKind::TypePath)?];
            while self.eat("+") {
                us.push(self.path(NodeKind::TypePath)?);
            }
            c.push(self.node(st, NodeKind::Uses, None, &us));
        }
        if self.eat("of") {
            c.push(self.brand()?);
        }
        if self.eat("grant") {
            let st = self.pos - 1;
            self.expect("bytes")?;
            if self.token(0).kind != TokenKind::Integer {
                return self.fail("LANG-5", "expected grant size");
            }
            let t = self.bump();
            c.push(self.node(st, NodeKind::Literal, Some(t), &[]));
        }
        if self.eat("{") {
            let st = self.pos - 1;
            let mut fields = vec![];
            while !self.eat("}") {
                if self.eof() {
                    return self.fail("LANG-5", "unclosed fields");
                }
                let fs = self.pos;
                let n = self.name()?;
                self.expect(":")?;
                let t = self.ty()?;
                fields.push(self.node(fs, NodeKind::Field, Some(n), &[t]));
                self.entry_end("}")?;
            }
            c.push(self.node(st, NodeKind::Fields, None, &fields));
        } else if self.eat("=") {
            let st = self.pos;
            // LANG-9: a single bare name aliases; payload parentheses or | makes a variant.
            if self.token(0).kind == TokenKind::Ident && matches!(self.word(1), "(" | "|") {
                let mut vs = vec![self.variant()?];
                while self.eat("|") {
                    vs.push(self.variant()?);
                }
                c.push(self.node(st, NodeKind::Variants, None, &vs));
            } else {
                c.push(self.ty()?);
            }
        }
        if self.at("drop") {
            let st = self.pos;
            self.bump();
            self.expect("(")?;
            let n = self.name()?;
            let mut drop_children = vec![];
            if self.eat(",") {
                self.expect("mut")?;
                let ds = self.pos;
                let context = self.name()?;
                drop_children.push(self.node(ds, NodeKind::MutParameter, Some(context), &[]));
            }
            self.expect(")")?;
            let body = self.block()?;
            drop_children.push(body);
            c.push(self.node(st, NodeKind::Drop, Some(n), &drop_children));
        }
        Ok(self.node(s, kind, Some(name), &c))
    }
    fn variant(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        let n = self.name()?;
        let mut c = vec![];
        let mut labelled = None;
        if self.eat("(") {
            c = self.list(")", |p| {
                let st = p.pos;
                let label = if p.token(0).kind == TokenKind::Ident && p.word(1) == ":" {
                    let n = p.name()?;
                    p.expect(":")?;
                    Some(n)
                } else {
                    None
                };
                if labelled.is_some_and(|old| old != label.is_some()) {
                    return p.fail("LANG-9", "mixed payload labels");
                }
                labelled = Some(label.is_some());
                let ty = p.ty()?;
                Ok(p.node(st, NodeKind::Payload, label, &[ty]))
            })?;
        }
        Ok(self.node(s, NodeKind::Variant, Some(n), &c))
    }
    fn generics(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        self.expect("[")?;
        let c = self.list("]", |p| {
            let st = p.pos;
            let region = p.eat("in");
            let n = p.name()?;
            let mut c = vec![];
            if p.eat(":") {
                c.push(p.bounds()?);
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
    fn bounds(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        let mut c = vec![];
        loop {
            let st = self.pos;
            if !BOUNDS.contains(&self.word(0)) {
                return self.fail("LANG-5", "unknown bound");
            }
            let n = self.bump();
            c.push(self.node(st, NodeKind::BoundName, Some(n), &[]));
            if !self.eat("+") {
                break;
            }
        }
        Ok(self.node(s, NodeKind::Bounds, None, &c))
    }
    fn header(&mut self, top: bool) -> Parsed<Vec<NodeId>> {
        let mut c = vec![];
        if self.at("[") {
            c.push(self.generics()?);
        }
        let s = self.pos;
        self.expect("(")?;
        let ps = self.list(")", |p| p.parameter(top))?;
        c.push(self.node(s, NodeKind::Parameters, None, &ps));
        if self.eat("->") {
            let st = self.pos - 1;
            let t = self.ty()?;
            c.push(self.node(st, NodeKind::ResultType, None, &[t]));
        }
        if self.eat("uses") {
            let st = self.pos - 1;
            let mut us = vec![self.path(NodeKind::TypePath)?];
            while self.eat("+") {
                us.push(self.path(NodeKind::TypePath)?);
            }
            c.push(self.node(st, NodeKind::Uses, None, &us));
        }
        if self.eat("waits") {
            c.push(self.node(self.pos - 1, NodeKind::Waits, None, &[]));
        }
        if self.eat("where") {
            let st = self.pos - 1;
            let mut cs = vec![];
            loop {
                cs.push(self.path(NodeKind::Path)?);
                self.expect(":")?;
                cs.push(self.path(NodeKind::Path)?);
                if !self.eat(",") {
                    break;
                }
            }
            c.push(self.node(st, NodeKind::Outlives, None, &cs));
        }
        Ok(c)
    }
    fn parameter(&mut self, named: bool) -> Parsed<NodeId> {
        let s = self.pos;
        let k = if self.eat("take") {
            NodeKind::TakeParameter
        } else if self.eat("mut") {
            NodeKind::MutParameter
        } else {
            NodeKind::Parameter
        };
        let n = if named || (self.token(0).kind == TokenKind::Ident && self.word(1) == ":") {
            let n = self.name()?;
            self.expect(":")?;
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
        let mut n = if self.at("fn")
            || self.at("scoped")
            || (self.at("own") && matches!(self.word(1), "fn" | "["))
        {
            let scoped = self.eat("scoped");
            let own = self.eat("own");
            let mut c = vec![];
            if own && self.eat("[") {
                c.push(self.bounds()?);
                self.expect("]")?;
            }
            self.expect("fn")?;
            c.extend(self.header(false)?);
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
        } else if self.eat("(") {
            let c = self.list(")", |p| p.ty())?;
            let k = if c.is_empty() {
                NodeKind::UnitType
            } else if c.len() == 1 && self.word_token_before() != "," {
                NodeKind::GroupType
            } else {
                NodeKind::TupleType
            };
            self.node(s, k, None, &c)
        } else {
            let path = self.path(NodeKind::TypePath)?;
            if self.eat("[") {
                let mut c = vec![path];
                let args = self.list("]", |p| {
                    if p.eat("in") {
                        let st = p.pos - 1;
                        let path = p.path(NodeKind::Path)?;
                        Ok(p.node(st, NodeKind::Anchor, None, &[path]))
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
        if self.eat("of") {
            let b = self.brand()?;
            n = self.node(s, NodeKind::Brand, None, &[n, b]);
        }
        if self.eat("in") {
            let b = self.path(NodeKind::Path)?;
            n = self.node(s, NodeKind::Anchor, None, &[n, b]);
        }
        Ok(n)
    }
    fn word_token_before(&self) -> &str {
        if self.pos >= 2 {
            self.text_token(self.significant[self.pos - 2])
        } else {
            ""
        }
    }
    fn brand(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.eat(".") {
            let n = self.name()?;
            Ok(self.node(s, NodeKind::FreshBrand, Some(n), &[]))
        } else {
            let p = self.path(NodeKind::Path)?;
            Ok(self.node(s, NodeKind::Brand, None, &[p]))
        }
    }
    fn path(&mut self, kind: NodeKind) -> Parsed<NodeId> {
        let s = self.pos;
        let n = self.name()?;
        while self.continuing() && self.at(".") && self.token(1).kind == TokenKind::Ident {
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
        self.expect("{")?;
        let old = self.whitespace;
        self.whitespace = 0;
        let mut c = vec![];
        let result = (|| {
            while !self.eat("}") {
                if self.eof() || self.item_start() {
                    return self.fail("LANG-5", "unclosed block");
                }
                if self.eat(";") {
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
        if self.eat("let") {
            let mutable = self.eat("mut");
            let mut c = vec![self.pattern()?];
            if self.eat(":") {
                c.push(self.ty()?);
            }
            if self.eat("=") {
                c.push(self.required(|p| p.expr(0))?);
                if self.eat("else") {
                    self.subject(*c.last().unwrap())?;
                    c.push(self.block()?);
                }
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
        if matches!(self.word(0), "+" | "*" | "/" | "%" | "." | "?") {
            return self.fail("LANG-4", "continuation after a complete statement");
        }
        let e = self.expr(0)?;
        if self.continuing() && ASSIGN.contains(&self.word(0)) {
            let op = self.bump();
            let v = self.required(|p| p.expr(0))?;
            self.end(false)?;
            return Ok(self.node(s, NodeKind::Assign, Some(op), &[e, v]));
        }
        let implicit = matches!(
            self.tree.node(e).kind,
            NodeKind::If
                | NodeKind::Match
                | NodeKind::Block
                | NodeKind::Loop
                | NodeKind::While
                | NodeKind::For
                | NodeKind::ForTake
                | NodeKind::Region
                | NodeKind::Bound
                | NodeKind::Labelled
        );
        let discard = self.end(implicit)?;
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
        let mut placed = false;
        while self.continuing() {
            if minimum <= 12 && !placed {
                if self.eat("(") {
                    let mut c = vec![left];
                    c.extend(self.list(")", |p| {
                        let st = p.pos;
                        let mut token = None;
                        let kind = if p.token(0).kind == TokenKind::Ident && p.word(1) == ":" {
                            token = Some(p.bump());
                            p.expect(":")?;
                            NodeKind::LabelledArgument
                        } else if p.eat("bytes") {
                            NodeKind::BytesArgument
                        } else {
                            NodeKind::Argument
                        };
                        let v = p.expr(0)?;
                        Ok(p.node(st, kind, token, &[v]))
                    })?);
                    left = self.node(s, NodeKind::Call, None, &c);
                    continue;
                }
                if self.eat("[") {
                    let i = self.required(|p| p.expr(0))?;
                    self.expect("]")?;
                    left = self.node(s, NodeKind::Index, None, &[left, i]);
                    continue;
                }
                if self.eat(".") {
                    if self.tree.node(left).kind == NodeKind::Literal && self.eof() {
                        return self.fail("LANG-3", "fractional point requires a following digit");
                    }
                    let n = if self.token(0).kind == TokenKind::Integer {
                        let raw = self.word(0);
                        if !raw.bytes().all(|b| b.is_ascii_digit()) {
                            return self
                                .fail("LANG-5", "tuple projection is an unsuffixed decimal index");
                        }
                        self.bump()
                    } else {
                        self.name()?
                    };
                    left = self.node(s, NodeKind::Projection, Some(n), &[left]);
                    continue;
                }
                if self.eat("?") {
                    left = self.node(s, NodeKind::Try, None, &[left]);
                    continue;
                }
            }
            if minimum <= 11 && !placed && self.eat("in") {
                let p = self.path(NodeKind::Path)?;
                left = self.node(s, NodeKind::Placement, None, &[left, p]);
                placed = true;
                continue;
            }
            let Some(&(_, prec, na)) = BINARY.iter().find(|(word, _, _)| *word == self.word(0))
            else {
                break;
            };
            if prec < minimum {
                break;
            }
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
        Ok(left)
    }
    fn primary(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if PREFIX.contains(&self.word(0)) {
            let op = self.bump();
            let e = self.required(|p| p.expr(11))?;
            return Ok(self.node(s, NodeKind::Unary, Some(op), &[e]));
        }
        if self.token(0).kind == TokenKind::Label {
            let t = self.bump();
            self.expect(":")?;
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
        if self.eat("return") || self.eat("break") || self.eat("continue") {
            let k = match self.text_token(self.significant[s]) {
                "return" => NodeKind::Return,
                "break" => NodeKind::Break,
                _ => NodeKind::Continue,
            };
            let mut label = None;
            let mut c = vec![];
            if !self.newline() && self.token(0).kind == TokenKind::Label {
                if k == NodeKind::Return {
                    return self.fail("LANG-5", "return has no label");
                }
                label = Some(self.bump());
            }
            if k != NodeKind::Continue
                && !self.newline()
                && !self.eof()
                && !matches!(self.word(0), "}" | ";" | "," | ")" | "]" | "=>")
            {
                c.push(self.expr(0)?);
            }
            return Ok(self.node(s, k, label, &c));
        }
        if self.at("{") {
            return self.block();
        }
        if self.eat("(") {
            let c = self.list(")", |p| p.expr(0))?;
            let k = if c.is_empty() {
                NodeKind::Unit
            } else if c.len() == 1 && self.word_token_before() != "," {
                NodeKind::Group
            } else {
                NodeKind::Tuple
            };
            return Ok(self.node(s, k, None, &c));
        }
        if self.eat("if") {
            let condition = self.condition()?;
            let yes = self.block()?;
            let mut c = vec![condition, yes];
            if self.eat("else") {
                c.push(if self.at("if") {
                    self.primary()?
                } else {
                    self.block()?
                });
            }
            return Ok(self.node(s, NodeKind::If, None, &c));
        }
        if self.eat("match") {
            let subject = self.expr(0)?;
            self.subject(subject)?;
            self.expect("{")?;
            let mut c = vec![subject];
            while !self.eat("}") {
                if self.eof() {
                    return self.fail("LANG-5", "unclosed match");
                }
                let st = self.pos;
                let mut a = vec![self.pattern()?];
                if self.eat("if") {
                    a.push(self.expr(0)?);
                }
                self.expect("=>")?;
                a.push(self.required(|p| p.expr(0))?);
                c.push(self.node(st, NodeKind::Arm, None, &a));
                self.entry_end("}")?;
            }
            return Ok(self.node(s, NodeKind::Match, None, &c));
        }
        if self.eat("loop") {
            let b = self.block()?;
            return Ok(self.node(s, NodeKind::Loop, None, &[b]));
        }
        if self.eat("while") {
            let c = self.condition()?;
            let b = self.block()?;
            return Ok(self.node(s, NodeKind::While, None, &[c, b]));
        }
        if self.eat("for") {
            let take = self.eat("take");
            let pat = self.pattern()?;
            self.expect("in")?;
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
        if self.eat("region") {
            let n = self.name()?;
            let b = self.block()?;
            return Ok(self.node(s, NodeKind::Region, Some(n), &[b]));
        }
        if self.eat("bound") {
            self.expect("bytes")?;
            let value = self.expr(0)?;
            let body = self.block()?;
            return Ok(self.node(s, NodeKind::Bound, None, &[value, body]));
        }
        if self.at("fn") && self.word(1) != "(" {
            return self.fail("LANG-5", "expected expression before next function");
        }
        if self.at("fn") || self.at("scoped") {
            let scoped = self.eat("scoped");
            self.expect("fn")?;
            let mut c = self.header(false)?;
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
        if matches!(
            self.token(0).kind,
            TokenKind::Integer
                | TokenKind::Float
                | TokenKind::Character
                | TokenKind::Byte
                | TokenKind::String
                | TokenKind::Bytes
                | TokenKind::Keyword(Keyword::True | Keyword::False)
        ) {
            let t = self.bump();
            return Ok(self.node(s, NodeKind::Literal, Some(t), &[]));
        }
        self.path(NodeKind::Path)
    }
    fn condition(&mut self) -> Parsed<NodeId> {
        if self.eat("let") {
            let s = self.pos - 1;
            let p = self.pattern()?;
            self.expect("=")?;
            let e = self.expr(0)?;
            Ok(self.node(s, NodeKind::ConditionLet, None, &[p, e]))
        } else {
            let n = self.expr(0)?;
            self.subject(n)?;
            Ok(n)
        }
    }
    fn subject(&mut self, mut n: NodeId) -> Parsed<()> {
        loop {
            let node = self.tree.node(n);
            match node.kind {
                NodeKind::Unary | NodeKind::Binary | NodeKind::Return | NodeKind::Break
                    if !self.tree.children(n).is_empty() =>
                {
                    n = *self.tree.children(n).last().unwrap()
                }
                NodeKind::Block
                | NodeKind::If
                | NodeKind::Match
                | NodeKind::Loop
                | NodeKind::While
                | NodeKind::For
                | NodeKind::ForTake
                | NodeKind::Region
                | NodeKind::Bound
                | NodeKind::Closure
                | NodeKind::ScopedClosure
                | NodeKind::Labelled => {
                    let span = node.origin.span;
                    self.diagnostics.push(Diagnostic::error(
                        "LANG-5",
                        Span {
                            start: self.base + span.start,
                            end: self.base + span.end,
                        },
                        "subject cannot end in an unparenthesized block",
                    ));
                    return Err(());
                }
                _ => return Ok(()),
            }
        }
    }
    fn pattern(&mut self) -> Parsed<NodeId> {
        self.nested(|p| p.pattern_inner())
    }
    fn pattern_inner(&mut self) -> Parsed<NodeId> {
        let s = self.pos;
        if self.eat("_") {
            return Ok(self.node(s, NodeKind::Wildcard, None, &[]));
        }
        if self.eat("take") {
            let n = self.name()?;
            return Ok(self.node(s, NodeKind::TakePattern, Some(n), &[]));
        }
        if self.eat("(") {
            let c = self.list(")", |p| p.pattern())?;
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
        if self.eat("-")
            || matches!(
                self.token(0).kind,
                TokenKind::Integer
                    | TokenKind::Float
                    | TokenKind::Character
                    | TokenKind::Byte
                    | TokenKind::String
                    | TokenKind::Bytes
                    | TokenKind::Keyword(Keyword::True | Keyword::False)
            )
        {
            let t = self.bump();
            let left = self.node(s, NodeKind::LiteralPattern, Some(t), &[]);
            if self.eat("..=") {
                let right = self.pattern()?;
                return Ok(self.node(s, NodeKind::RangePattern, None, &[left, right]));
            }
            return Ok(left);
        }
        let p = self.path(NodeKind::Path)?;
        let name = self.tree.node(p).token;
        if self.eat("(") {
            let mut rest = false;
            let mut c = vec![p];
            c.extend(self.list(")", |p| {
                let st = p.pos;
                if rest {
                    return p.fail("LANG-5", "rest pattern must be last");
                }
                if p.eat("..") {
                    rest = true;
                    return Ok(p.node(st, NodeKind::RestPattern, None, &[]));
                }
                let label = if p.token(0).kind == TokenKind::Ident && p.word(1) == ":" {
                    let n = p.bump();
                    p.expect(":")?;
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

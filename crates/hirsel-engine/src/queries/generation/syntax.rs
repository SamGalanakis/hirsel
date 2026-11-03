use super::*;

pub(crate) struct Literal {
    pub(crate) syntax: Arc<ItemSyntax>,
    pub(crate) header: Arc<ItemSyntax>,
    pub(crate) header_span: Span,
    tokens: Vec<hirsel_syntax::Token>,
    body_start: usize,
    parameters: Vec<(Option<Symbol>, Span)>,
    context: FileContext,
}

pub(crate) fn parse_literal(
    definition: &DefPath,
    snapshot: &Arc<ItemSyntax>,
    generated: FileId,
    sources: &mut SourceMap,
    lexer: &dyn LexerPhase,
    parser: &dyn ParserPhase,
) -> Result<Literal, QueryError> {
    let source = &snapshot.item;
    let ItemKind::Function {
        signature: source_signature,
        ..
    } = &source.kind
    else {
        return Err(QueryError::NotImplemented);
    };
    let context = FileContext {
        package: definition.0.package.clone(),
        modules: definition.0.modules.clone(),
    };
    let lexed = lexer.lex(generated, sources)?;
    if has_errors(&lexed.diagnostics) {
        return Err(QueryError::Diagnostics(lexed.diagnostics));
    }
    let tokens = lexed.value;
    let significant = |t: &hirsel_syntax::Token| !matches!(t.kind, TokenKind::Trivia(_));
    let first = tokens.iter().position(significant);
    let Some(fn_index) = first.filter(|&i| tokens[i].kind == TokenKind::Keyword(Keyword::Fn))
    else {
        let span = first.map_or(
            Span::new(sources.file_anchor(generated), 0, 0).unwrap(),
            |i| tokens[i].span,
        );
        return Err(diagnostic(
            "CT-5",
            span,
            "generated text is not one function literal `fn(..) .. { .. }`",
        ));
    };
    // The literal as a declaration: `fn <declared name>(..) .. { .. }`.
    let name = snapshot
        .tree
        .tokens
        .iter()
        .find(|t| t.span == source.name_span);
    let Some(name) = name.cloned() else {
        return Err(QueryError::NotImplemented);
    };
    let mut tape = tokens[..=fn_index].to_vec();
    tape.push(name);
    tape.extend_from_slice(&tokens[fn_index + 1..]);
    let parsed = parser.parse(generated, tape, &context, sources)?;
    if has_errors(&parsed.diagnostics) {
        return Err(QueryError::Diagnostics(parsed.diagnostics));
    }
    let not_literal = || {
        diagnostic(
            "CT-5",
            tokens[fn_index].span,
            "generated text is not one function literal `fn(..) .. { .. }`",
        )
    };
    let mut literal = match &parsed.value.items[..] {
        [literal] => (**literal).clone(),
        _ => return Err(not_literal()),
    };
    let (literal_signature, literal_body) = match &literal.item.kind {
        ItemKind::Function {
            signature,
            body: Some(body),
            abi: None,
        } if literal.item.attributes.is_empty() => (signature.clone(), *body),
        _ => return Err(not_literal()),
    };
    let literal_origin = literal.item.origin;
    // The literal's tokens are its tape's: the first item owns the leading trivia.
    let body_start = literal.tree.expressions[literal_body.0 as usize]
        .origin
        .tokens
        .start as usize;
    let header_span = Span::new(
        tokens[fn_index].span.anchor,
        tokens[fn_index].span.start,
        sources.resolve(literal.tree.tokens[body_start].span).start,
    )
    .unwrap();
    literal.item.definition = renamed(definition, "literal");
    // The literal and the header are compared without uniqueness: the body
    // is checked against the declared header, which alone decides it.
    let mut header = (**snapshot).clone();
    header.item.definition = renamed(definition, "header");
    header.occurrence = 0;
    for (tree, signature) in [
        (&mut header.tree, source_signature),
        (&mut literal.tree, &literal_signature),
    ] {
        for ty in signature
            .parameters
            .iter()
            .map(|p| p.ty)
            .chain(signature.result)
        {
            without_uniqueness(tree, ty);
        }
    }
    let header = Arc::new(header);
    let literal = Arc::new(literal);
    let parameters: Vec<_> = literal_signature
        .parameters
        .iter()
        .map(|p| {
            let span = literal
                .tree
                .parameter_name_span(literal_origin, p)
                .unwrap_or(header_span);
            (p.name, span)
        })
        .collect();
    Ok(Literal {
        syntax: literal,
        header,
        header_span,
        tokens,
        body_start,
        parameters,
        context,
    })
}

impl Literal {
    pub(crate) fn finish(
        self,
        snapshot: &Arc<ItemSyntax>,
        body: ExprId,
        file: FileId,
        text: &str,
        sources: &mut SourceMap,
        parser: &dyn ParserPhase,
    ) -> Result<Arc<ItemSyntax>, QueryError> {
        let source = &snapshot.item;
        let significant = |t: &hirsel_syntax::Token| !matches!(t.kind, TokenKind::Trivia(_));
        // The written header up to its `=`, then the literal's body: one item.
        let written = &snapshot.tree.tokens;
        let start = source.origin.tokens.start as usize;
        // The body's `comptime` token, after the `=` that ends the header.
        let mut end = snapshot.tree.expressions[body.0 as usize]
            .origin
            .tokens
            .start as usize;
        let mut equals = false;
        while end > start && !(equals && significant(&written[end - 1])) {
            equals |= significant(&written[end - 1]);
            end -= 1;
        }
        let mut tape: Vec<_> = written[start..end].to_vec();
        // literal token i >= fn_index + 2 is generated token i - 1.
        tape.extend_from_slice(&self.tokens[self.body_start - 1..]);
        // Neither part is relative to the declaring file's own anchor, so the
        // parse keeps every span where it stands: the header in the
        // declaration, the body in the generated source.
        let parsed = parser.parse(file, tape, &self.context, sources)?;
        if has_errors(&parsed.diagnostics) {
            return Err(QueryError::Diagnostics(parsed.diagnostics));
        }
        let mut syntax = match &parsed.value.items[..] {
            [syntax] => (**syntax).clone(),
            _ => return Err(QueryError::NotImplemented),
        };
        syntax.item.origin.span = source.origin.span;
        syntax.item.generated = Some(Generated::Body(self.parameters));
        syntax.anchor = snapshot.anchor;
        syntax.occurrence = snapshot.occurrence;
        syntax.text_hash = ContentHash::of_parts(
            "hirsel.generated-item.v1",
            [snapshot.text_hash.0.as_slice(), text.as_bytes()],
        );
        Ok(Arc::new(syntax))
    }
}
pub(crate) fn normalize_headers(
    literal: &Literal,
    types: &hirsel_types::TypeInterner,
    mut signature: impl FnMut(Arc<ItemSyntax>) -> Result<QueryOutput<Signature>, QueryError>,
) -> Result<(), QueryError> {
    let normalized = signature(literal.syntax.clone())?;
    if has_errors(&normalized.diagnostics) {
        return Err(QueryError::Diagnostics(normalized.diagnostics));
    }
    let declared = signature(literal.header.clone())?;
    if has_errors(&declared.diagnostics) {
        return Err(QueryError::Diagnostics(declared.diagnostics));
    }
    if !same_function(types, &declared.value, &normalized.value) {
        return Err(diagnostic(
            "LANG-7",
            literal.header_span,
            "the generated function literal's type differs from the declared header",
        ));
    }
    Ok(())
}

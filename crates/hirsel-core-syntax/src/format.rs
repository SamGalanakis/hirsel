//! Token-role layout adapted from Goose format.h and Hirsel's doc/trivia printer.
//! AST statement boundaries, as in Go's printer, replace source-dependent breaks.
use crate::*;
use std::io;

/// Canonical layout of a complete parse. Invalid source has no formatted result.
/// Prints every non-whitespace token verbatim, including comments and semicolons.
/// Four-space blocks and statement/field/arm lines are structural. A trailing
/// comma gives parenthesized and bracketed lists one entry per line.
/// No resolution, second parse or validation of our output is run.
pub fn format(parsed: &ParseOutput) -> Option<String> {
    if !parsed.diagnostics.is_empty() || parsed.resource_refusal.is_some() {
        return None;
    }
    let file = &parsed.syntax;
    let tokens = &file.tree.tokens;
    let mut roles = default_roles(tokens.len());
    let mut out = String::new();
    for item in file.items() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&item_text(&item, &mut roles));
    }
    let start = tokens.partition_point(|t| t.span.start < file.trailing_start);
    let tail = print(&file.source, &tokens[start..], &roles[start..]);
    out.push_str(&tail);
    Some(out)
}

/// Format a complete parse and prove that the result preserves its syntax.
/// `mode` must be the mode used to parse the input. `None` means the input
/// already carried a diagnostic or resource refusal. An error means the
/// formatter produced a result it could not prove equivalent.
pub fn format_verified(
    parsed: &ParseOutput,
    mode: ParseMode,
    symbols: &mut Symbols,
) -> io::Result<Option<String>> {
    let Some(output) = format(parsed) else {
        return Ok(None);
    };
    if output != parsed.syntax.source {
        verify_format(parsed, &output, mode, symbols)?;
    }
    Ok(Some(output))
}

pub(crate) fn verify_format(
    original: &ParseOutput,
    output: &str,
    mode: ParseMode,
    symbols: &mut Symbols,
) -> io::Result<()> {
    let reparsed = parse(output, mode, symbols);
    let failure = |reason| io::Error::other(format!("internal formatter error: {reason}"));
    if !reparsed.diagnostics.is_empty() || reparsed.resource_refusal.is_some() {
        return Err(failure("formatted source did not parse cleanly"));
    }
    let before = &original.syntax;
    let after = &reparsed.syntax;
    fn tokens(file: &SyntaxFile) -> impl Iterator<Item = (TokenKind, &str)> {
        file.tree
            .tokens
            .iter()
            .zip(0..)
            .map(move |(token, i)| (token.kind, file.tree.token_text(&file.source, TokenId(i))))
    }
    if !tokens(before).eq(tokens(after)) {
        return Err(failure("formatted source changed tokens or comments"));
    }
    if !same_ast(before, after) {
        return Err(failure("formatted source changed the syntax tree"));
    }
    // Identical token streams make comment indices directly comparable. The
    // parser assigns leading and trailing comments to items or to the file tail.
    fn comments(item: Item<'_>) -> impl Iterator<Item = u32> {
        item.range
            .tokens
            .clone()
            .filter(move |&i| item.tree.tokens[i as usize].kind.is_trivia())
    }
    if !before
        .items()
        .zip(after.items())
        .all(|(a, b)| comments(a).eq(comments(b)))
    {
        return Err(failure("formatted source changed item comment attachment"));
    }
    Ok(())
}

pub(crate) fn same_ast(before: &SyntaxFile, after: &SyntaxFile) -> bool {
    fn nodes(item: Item<'_>) -> impl Iterator<Item = NodeId> {
        item.range.nodes.clone().map(NodeId)
    }
    fn token<'a>(item: Item<'a>, node: &Node) -> Option<&'a str> {
        node.token.map(|id| item.tree.token_text(item.text, id))
    }
    before.items.len() == after.items.len()
        && before.items().zip(after.items()).all(|(a, b)| {
            a.root.0 - a.range.nodes.start == b.root.0 - b.range.nodes.start
                && a.range.nodes.len() == b.range.nodes.len()
                && nodes(a).zip(nodes(b)).all(|(aid, bid)| {
                    let left = a.tree.node(aid);
                    let right = b.tree.node(bid);
                    left.kind == right.kind
                        && token(a, left) == token(b, right)
                        && a.tree
                            .children(aid)
                            .iter()
                            .map(|id| id.0 - a.range.nodes.start)
                            .eq(b
                                .tree
                                .children(bid)
                                .iter()
                                .map(|id| id.0 - b.range.nodes.start))
                })
        })
}

#[derive(Clone, Copy, Default)]
struct Role {
    line_before: bool,
    line_after: bool,
    open: bool,
    close: bool,
    unary: bool,
    attach: bool,
    spaced_colon: bool,
    labelled_list: bool,
    inline_semicolon: bool,
}

fn default_roles(len: usize) -> Vec<Role> {
    #[cfg(test)]
    ROLE_SLOTS_INITIALIZED.with(|slots| slots.set(slots.get() + len));
    vec![Role::default(); len]
}

#[cfg(test)]
thread_local! {
    static ROLE_SLOTS_INITIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_role_slots_initialized() -> usize {
    ROLE_SLOTS_INITIALIZED.with(|slots| slots.replace(0))
}

fn item_text(item: &Item, roles: &mut [Role]) -> String {
    let tree = &item.tree;
    for i in item.range.nodes.start as usize..item.range.nodes.end as usize {
        let node = &tree.nodes[i];
        let children = tree.children(NodeId(i as u32));
        let range = node.origin.tokens.start as usize..node.origin.tokens.end as usize;
        match node.kind {
            NodeKind::Block | NodeKind::Fields => {
                indent_between(roles, range.start, range.end - 1);
                for &child in children {
                    roles[tree.node(child).origin.tokens.start as usize].line_before = true;
                }
            }
            NodeKind::Match => {
                let subject = tree.node(children[0]);
                let open = (subject.origin.tokens.end as usize..range.end)
                    .find(|&t| tree.tokens[t].kind == TokenKind::LBrace)
                    .unwrap();
                indent_between(roles, open, range.end - 1);
                for &child in &children[1..] {
                    roles[tree.node(child).origin.tokens.start as usize].line_before = true;
                }
            }
            NodeKind::Attribute => roles[range.end - 1].line_after = true,
            NodeKind::Parameters => roles[range.start].attach = true,
            NodeKind::LabelledArgument => {
                let value = tree.node(children[0]);
                roles[value.origin.tokens.start as usize].labelled_list =
                    matches!(value.kind, NodeKind::Array | NodeKind::RepeatArray);
            }
            NodeKind::Ascription => roles[node.token.unwrap().0 as usize].spaced_colon = true,
            NodeKind::BytesOf => {
                let open = (range.start + 1..range.end)
                    .find(|&t| !tree.tokens[t].kind.is_trivia())
                    .unwrap();
                roles[open].attach = true;
            }
            NodeKind::RepeatArray => {
                let end = tree.node(children[0]).origin.tokens.end as usize;
                let semicolon = (end..range.end)
                    .find(|&i| tree.tokens[i].kind == TokenKind::Semicolon)
                    .unwrap();
                roles[semicolon].inline_semicolon = true;
            }
            NodeKind::Unary => {
                if let Some(token) = node.token {
                    roles[token.0 as usize].unary = true;
                }
            }
            // `e ?=> |p| v`: the arm's pipes hug its pattern.
            NodeKind::Try
                if children.len() == 2 && tree.node(children[1]).kind == NodeKind::Arm =>
            {
                let pattern = tree.node(tree.children(children[1])[0]);
                roles[pattern.origin.tokens.start as usize].attach = true;
                let close = (pattern.origin.tokens.end as usize..range.end)
                    .find(|&t| !tree.tokens[t].kind.is_trivia())
                    .unwrap();
                roles[close].attach = true;
            }
            NodeKind::Call | NodeKind::Index => {
                let callee_end = tree.node(children[0]).origin.tokens.end as usize;
                let open = (callee_end..range.end)
                    .find(|&t| !tree.tokens[t].kind.is_trivia())
                    .unwrap();
                roles[open].attach = true;
            }
            _ => {}
        }
        let entries = match node.kind {
            NodeKind::Parameters
            | NodeKind::Generics
            | NodeKind::Tuple
            | NodeKind::TupleType
            | NodeKind::TuplePattern
            | NodeKind::Array
            | NodeKind::Variant
            | NodeKind::RemovedVariant => children,
            NodeKind::Call | NodeKind::TypeArguments | NodeKind::ConstructorPattern => {
                &children[1..]
            }
            _ => continue,
        };
        trailing_comma_list(tree, roles, range.start, range.end - 1, entries);
    }
    print(
        item.text,
        &tree.tokens[item.range.tokens.start as usize..item.range.tokens.end as usize],
        &roles[item.range.tokens.start as usize..item.range.tokens.end as usize],
    )
}

fn trailing_comma_list(
    tree: &Tree,
    roles: &mut [Role],
    start: usize,
    close: usize,
    entries: &[NodeId],
) {
    let (Some(&first), Some(&last)) = (entries.first(), entries.last()) else {
        return;
    };
    let last_end = tree.node(last).origin.tokens.end as usize;
    let trailing = tree.tokens[last_end..close]
        .iter()
        .find(|token| !token.kind.is_trivia());
    if !trailing.is_some_and(|token| token.kind == TokenKind::Comma) {
        return;
    }
    let first_start = tree.node(first).origin.tokens.start as usize;
    let open = (start..first_start)
        .rfind(|&t| !tree.tokens[t].kind.is_trivia())
        .unwrap();
    indent_between(roles, open, close);
    for &entry in entries {
        roles[tree.node(entry).origin.tokens.start as usize].line_before = true;
    }
}

fn indent_between(roles: &mut [Role], open: usize, close: usize) {
    if close > open + 1 {
        roles[open].open = true;
        roles[open].line_after = true;
        roles[close].close = true;
        roles[close].line_before = true;
    }
}

fn print(text: &str, tokens: &[Token], roles: &[Role]) -> String {
    let mut out = String::new();
    let mut indent = 0;
    let mut line = false;
    let mut previous: Option<(TokenKind, Role)> = None;
    for (token, &role) in tokens.iter().zip(roles) {
        let raw = &text[token.span.start as usize..token.span.end as usize];
        let original_newline = previous.is_none() || token.line_before;
        if token.kind == TokenKind::Eof {
            continue;
        }
        if role.close {
            indent -= 1;
        }
        let comment = token.kind.is_trivia();
        let trailing = comment && !original_newline;
        // A trailing comment belongs before the previous token's pending break.
        // An indented group opener always puts its contents on a new line.
        let after_open = previous.is_some_and(|(_, r)| r.open);
        let break_here = role.line_before
            || (line && (!trailing || after_open))
            || (comment && original_newline);
        if break_here && !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if out.is_empty() || out.ends_with('\n') {
            let continued = !role.line_before
                && (grammar::line_continuation(token.symbol)
                    || grammar::ASSIGN.contains(&token.symbol));
            out.push_str(&"    ".repeat(indent + usize::from(continued)));
        } else if previous.is_some_and(|(kind, before)| {
            comment || kind.is_trivia() || space(kind, before, token.kind, role)
        }) {
            out.push(' ');
        }
        out.push_str(raw);
        if role.open {
            indent += 1;
        }
        // Every comment runs to its line end (LANG-1), and no token spans a line.
        line = (trailing && line)
            || role.line_after
            || (token.kind == TokenKind::Semicolon && !role.inline_semicolon)
            || comment;
        previous = Some((token.kind, role));
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn space(a: TokenKind, before: Role, b: TokenKind, role: Role) -> bool {
    use TokenKind::*;
    if role.attach {
        return false;
    }
    if role.spaced_colon {
        return true;
    }
    if matches!(b, Comma | Colon | Semicolon | RParen | RBracket | Question) {
        return false;
    }
    if b == Dot {
        // Joining a numeric literal to a projection dot could form a float.
        return matches!(a, TokenKind::Integer | TokenKind::Float);
    }
    if matches!(a, LParen | LBracket | Dot | At) {
        return false;
    }
    if a == LBrace && b == RBrace {
        return false;
    }
    if b == LBracket {
        return role.labelled_list;
    }
    if before.unary && a.is_punctuation() {
        return false;
    }
    if b == LParen {
        return !matches!(a, TokenKind::Ident | KwFn);
    }
    true
}

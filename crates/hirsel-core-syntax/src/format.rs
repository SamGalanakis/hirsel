//! Token-role layout adapted from Goose format.h and Hirsel's doc/trivia printer.
//! AST statement boundaries, as in Go's printer, replace source-dependent breaks.
use crate::*;

/// Canonical layout of a complete parse. Invalid source has no formatted result.
/// Prints every non-whitespace token verbatim, including comments and semicolons.
/// Four-space blocks and statement/field/arm lines are structural; other lists
/// stay inline. No resolution, second parse or validation of our output is run.
pub fn format(parsed: &ParseOutput) -> Option<String> {
    if !parsed.diagnostics.is_empty() || parsed.resource_refusal.is_some() {
        return None;
    }
    let mut out = String::new();
    for item in &parsed.syntax.items {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&item_text(item));
    }
    let trailing = lex(&parsed.syntax.trailing);
    let tail = print(
        &parsed.syntax.trailing,
        &trailing.tokens,
        &vec![Role::default(); trailing.tokens.len()],
    );
    out.push_str(&tail);
    Some(out)
}

#[derive(Clone, Copy, Default)]
struct Role {
    line_before: bool,
    line_after: bool,
    open: bool,
    close: bool,
    unary: bool,
    attach: bool,
}

fn item_text(item: &Item) -> String {
    let tree = &item.tree;
    let mut roles = vec![Role::default(); tree.tokens.len()];
    for (i, node) in tree.nodes.iter().enumerate() {
        let children = tree.children(NodeId(i as u32));
        let range = node.origin.tokens.start as usize..node.origin.tokens.end as usize;
        match node.kind {
            NodeKind::Block | NodeKind::Fields => {
                braces(tree, &mut roles, range.start, range.end - 1);
                for &child in children {
                    roles[tree.node(child).origin.tokens.start as usize].line_before = true;
                }
            }
            NodeKind::Match => {
                let subject = tree.node(children[0]);
                let open = (subject.origin.tokens.end as usize..range.end)
                    .find(|&t| tree.tokens[t].kind == TokenKind::Punct(Punct::LBrace))
                    .unwrap();
                braces(tree, &mut roles, open, range.end - 1);
                for &child in &children[1..] {
                    roles[tree.node(child).origin.tokens.start as usize].line_before = true;
                }
            }
            NodeKind::Attribute => roles[range.end - 1].line_after = true,
            NodeKind::Parameters => roles[range.start].attach = true,
            NodeKind::Unary => {
                if let Some(token) = node.token {
                    roles[token.0 as usize].unary = true;
                }
            }
            NodeKind::Call | NodeKind::Index => {
                let callee_end = tree.node(children[0]).origin.tokens.end as usize;
                let open = (callee_end..range.end)
                    .find(|&t| !matches!(tree.tokens[t].kind, TokenKind::Trivia(_)))
                    .unwrap();
                roles[open].attach = true;
            }
            _ => {}
        }
    }
    print(&item.text, &tree.tokens, &roles)
}

fn whitespace(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Trivia(TriviaKind::Space | TriviaKind::Newline)
    )
}

fn braces(tree: &Tree, roles: &mut [Role], open: usize, close: usize) {
    if tree.tokens[open + 1..close]
        .iter()
        .any(|t| !whitespace(t.kind))
    {
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
    let mut original_newline = true;
    let mut previous: Option<(TokenKind, Role)> = None;
    for (token, &role) in tokens.iter().zip(roles) {
        let raw = &text[token.span.start as usize..token.span.end as usize];
        if whitespace(token.kind) {
            original_newline |= raw.contains('\n');
            continue;
        }
        if token.kind == TokenKind::Eof {
            continue;
        }
        if role.close {
            indent -= 1;
        }
        let comment = matches!(token.kind, TokenKind::Trivia(_));
        let trailing = comment && !original_newline && previous.is_some();
        // A trailing comment belongs before the previous token's pending break.
        // A block opener always puts its contents on an indented line.
        let after_open = previous.is_some_and(|(_, r)| r.open);
        let break_here = role.line_before
            || (line && (!trailing || after_open))
            || (comment && original_newline);
        if break_here && !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if out.is_empty() || out.ends_with('\n') {
            out.push_str(&"    ".repeat(indent));
        } else if previous.is_some_and(|(kind, before)| {
            comment || matches!(kind, TokenKind::Trivia(_)) || space(kind, before, token.kind, role)
        }) {
            out.push(' ');
        }
        out.push_str(raw);
        if role.open {
            indent += 1;
        }
        let line_comment = matches!(
            token.kind,
            TokenKind::Trivia(TriviaKind::LineComment | TriviaKind::DocLine)
        );
        line = (trailing && line)
            || role.line_after
            || token.kind == TokenKind::Punct(Punct::Semicolon)
            || line_comment
            || (comment && original_newline);
        previous = Some((token.kind, role));
        original_newline = raw.contains('\n');
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn space(a: TokenKind, before: Role, b: TokenKind, role: Role) -> bool {
    use Punct::*;
    use TokenKind::{Keyword as K, Punct as P};
    if role.attach {
        return false;
    }
    if matches!(
        b,
        P(Comma | Colon | Semicolon | RParen | RBracket | Question)
    ) {
        return false;
    }
    if b == P(Dot) {
        // Joining a numeric literal to a projection dot could form a float.
        return matches!(a, TokenKind::Integer | TokenKind::Float);
    }
    if matches!(a, P(LParen | LBracket | Dot | At)) {
        return false;
    }
    if a == P(LBrace) && b == P(RBrace) {
        return false;
    }
    if b == P(LBracket) {
        return false;
    }
    if before.unary && matches!(a, P(_)) {
        return false;
    }
    if b == P(LParen) {
        return !matches!(a, TokenKind::Ident | K(Keyword::Fn));
    }
    true
}

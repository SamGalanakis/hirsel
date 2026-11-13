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
    for item in parsed.syntax.items() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&item_text(&item));
    }
    let start = parsed
        .syntax
        .tree
        .tokens
        .partition_point(|t| t.span.start < parsed.syntax.trailing_start);
    let tokens = &parsed.syntax.tree.tokens[start..];
    let tail = print(
        &parsed.syntax.source,
        tokens,
        &vec![Role::default(); tokens.len()],
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
    inline_semicolon: bool,
}

fn item_text(item: &Item) -> String {
    let tree = &item.tree;
    let mut roles = vec![Role::default(); tree.tokens.len()];
    for i in item.range.nodes.start as usize..item.range.nodes.end as usize {
        let node = &tree.nodes[i];
        let children = tree.children(NodeId(i as u32));
        let range = node.origin.tokens.start as usize..node.origin.tokens.end as usize;
        match node.kind {
            NodeKind::Block | NodeKind::Fields => {
                braces(&mut roles, range.start, range.end - 1);
                for &child in children {
                    roles[tree.node(child).origin.tokens.start as usize].line_before = true;
                }
            }
            NodeKind::Match => {
                let subject = tree.node(children[0]);
                let open = (subject.origin.tokens.end as usize..range.end)
                    .find(|&t| tree.tokens[t].kind == TokenKind::LBrace)
                    .unwrap();
                braces(&mut roles, open, range.end - 1);
                for &child in &children[1..] {
                    roles[tree.node(child).origin.tokens.start as usize].line_before = true;
                }
            }
            NodeKind::Attribute => roles[range.end - 1].line_after = true,
            NodeKind::Parameters => roles[range.start].attach = true,
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
            NodeKind::Call | NodeKind::Index => {
                let callee_end = tree.node(children[0]).origin.tokens.end as usize;
                let open = (callee_end..range.end)
                    .find(|&t| !tree.tokens[t].kind.is_trivia())
                    .unwrap();
                roles[open].attach = true;
            }
            _ => {}
        }
    }
    print(
        item.text,
        &tree.tokens[item.range.tokens.start as usize..item.range.tokens.end as usize],
        &roles[item.range.tokens.start as usize..item.range.tokens.end as usize],
    )
}

fn braces(roles: &mut [Role], open: usize, close: usize) {
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
    let mut original_newline = true;
    let mut previous: Option<(TokenKind, Role)> = None;
    for (token, &role) in tokens.iter().zip(roles) {
        let raw = &text[token.span.start as usize..token.span.end as usize];
        original_newline |= token.line_before;
        if token.kind == TokenKind::Eof {
            continue;
        }
        if role.close {
            indent -= 1;
        }
        let comment = token.kind.is_trivia();
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
            comment || kind.is_trivia() || space(kind, before, token.kind, role)
        }) {
            out.push(' ');
        }
        out.push_str(raw);
        if role.open {
            indent += 1;
        }
        let line_comment = matches!(token.kind, TokenKind::LineComment | TokenKind::DocLine);
        line = (trailing && line)
            || role.line_after
            || (token.kind == TokenKind::Semicolon && !role.inline_semicolon)
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
    use TokenKind::*;
    if role.attach {
        return false;
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
        return false;
    }
    if before.unary && a.is_punctuation() {
        return false;
    }
    if b == LParen {
        return !matches!(a, TokenKind::Ident | KwFn);
    }
    true
}

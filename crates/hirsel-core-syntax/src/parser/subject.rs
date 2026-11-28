//! LANG-5: an `if`, `while` or `match` subject that ends in a block.
use super::*;

impl<const BORROWED: bool> Parser<'_, BORROWED> {
    pub(super) fn subject(&mut self, mut n: NodeId) -> Parsed<()> {
        let whole = n;
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
                    // The primary site is the whole subject, so its fix wraps that span.
                    let span = self.tree.node(whole).origin.span;
                    if BORROWED {
                        return self.data_refusal(Span {
                            start: self.base + span.start,
                            end: self.base + span.end,
                        });
                    }
                    let message = if self.two_key_comparison(whole) {
                        "subject cannot end in an unparenthesized block; wrap the subject in \
                         parentheses, or compare tuples: `(k1, v1) < (k2, v2)`"
                    } else {
                        "subject cannot end in an unparenthesized block; wrap the subject in \
                         parentheses"
                    };
                    self.diagnostics.push(Diagnostic::error(
                        "LANG-5",
                        Span {
                            start: self.base + span.start,
                            end: self.base + span.end,
                        },
                        message,
                    ));
                    return Err(());
                }
                _ => return Ok(()),
            }
        }
    }
    /// `if k1 != k2 { k1 < k2 } else { v1 < v2 }`: an order on two keys, which a
    /// tuple comparison states directly.
    fn two_key_comparison(&self, n: NodeId) -> bool {
        let op = |n: NodeId| {
            let node = self.tree.node(n);
            (node.kind == NodeKind::Binary)
                .then_some(node.token)
                .flatten()
                .map(|t| self.text_token(t.0 as usize))
        };
        let tail = |n: NodeId| {
            let mut n = n;
            while matches!(
                self.tree.node(n).kind,
                NodeKind::Block | NodeKind::ExpressionStatement
            ) {
                n = *self.tree.children(n).last()?;
            }
            Some(n)
        };
        let ordered = |n: NodeId| {
            tail(n)
                .and_then(op)
                .is_some_and(|o| matches!(o, "<" | "<=" | ">" | ">="))
        };
        if self.tree.node(n).kind != NodeKind::If {
            return false;
        }
        let [condition, yes, no] = self.tree.children(n) else {
            return false;
        };
        op(*condition) == Some("!=") && ordered(*yes) && ordered(*no)
    }
}

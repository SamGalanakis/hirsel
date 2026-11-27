use super::*;

impl Resolver<'_, '_> {
    /// SEM-9: an `Inline` length written as a name names a constant, in the value
    /// namespace; a name no value binds is looked up as a type, so the front refuses it
    /// as a length rather than as a missing name. Whether `Inline[T, n]`'s arguments
    /// `c` were resolved here.
    pub(super) fn inline_length(&mut self, c: &[NodeId]) -> bool {
        let tree = self.item.tree;
        let [head, element, n] = *c else {
            return false;
        };
        if tree.node(head).kind != NodeKind::TypePath
            || tree.node(head).origin.tokens.len() != 1
            || tree.node(head).token.map(|t| self.name(t)) != Some(sym::INLINE)
            || tree.node(n).kind != NodeKind::TypePath
        {
            return false;
        }
        self.visit(head);
        self.visit(element);
        let (names, diagnostics) = (self.out.names.len(), self.out.diagnostics.len());
        self.use_path::<false, false, false>(n);
        if self.out.names[names..]
            .iter()
            .any(|u| u.target == Target::Missing)
        {
            self.out.names.truncate(names);
            self.out.diagnostics.truncate(diagnostics);
            self.use_path::<true, false, false>(n);
        }
        true
    }

    /// PACK-16: `Held[send]` resolves `send` as a builtin bound when the head is `core.Held`.
    pub(super) fn held_send(&mut self, children: &[NodeId]) -> bool {
        let [head, bound] = *children else {
            return false;
        };
        let node = self.item.tree.node(bound);
        if node.kind != NodeKind::TypePath
            || node.origin.tokens.len() != 1
            || node.token.map(|t| self.name(t)) != Some(sym::SEND)
        {
            return false;
        }
        let before = self.out.names.len();
        self.visit(head);
        if self.out.names[before..].iter().any(|u| {
            matches!(
                u.target,
                Target::Definition(id) if self.index.definition_path(id).is_core("core.Held")
            )
        }) {
            self.out.names.push(ResolvedUse {
                node: bound,
                target: Target::Builtin(sym::SEND),
                projections: Span::default(),
            });
        } else {
            self.visit(bound);
        }
        true
    }
}

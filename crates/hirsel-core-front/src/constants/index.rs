use super::*;

impl ConstCx<'_, '_> {
    /// An index is a `u64`, or a `u8`, `u16` or `u32` zero-extended to it (LANG-21);
    /// a literal operand takes `u64`.
    pub(super) fn index(&mut self, mut n: NodeId, active: bool) -> Option<ConstValue> {
        let item = self.p.item(self.at);
        while item.tree.node(n).kind == NodeKind::Group {
            n = item.tree.children(n)[0];
        }
        let u64_ty = scalar(Scalar::U64);
        if self.literal_operand(n) {
            return self.expr(n, Some(u64_ty), active);
        }
        let value = self.expr(n, None, active)?;
        match self.p.types.kind(value.ty) {
            TyKind::Scalar(Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64) => {
                Some(ConstValue {
                    ty: u64_ty,
                    data: value.data,
                })
            }
            _ => self.mismatch(n, "an index is an unsigned integer"),
        }
    }
    /// An unsuffixed number, or `-`, `~`, a binary operator or parentheses over only
    /// those (LANG-21).
    fn literal_operand(&self, n: NodeId) -> bool {
        let item = self.p.item(self.at);
        let node = item.tree.node(n);
        let kids = item.tree.children(n);
        match node.kind {
            NodeKind::Literal => node.token.is_some_and(|t| {
                let text = item.tree.token_text(item.text, t);
                match item.tree.tokens[t.0 as usize].kind {
                    TokenKind::Integer => crate::body::int_suffix(text).1.is_none(),
                    TokenKind::Float => crate::body::float_suffix(text).1.is_none(),
                    _ => false,
                }
            }),
            NodeKind::Group => self.literal_operand(kids[0]),
            NodeKind::Unary => {
                matches!(
                    node.token.map(|t| item.tree.token_text(item.text, t)),
                    Some("-" | "~")
                ) && self.literal_operand(kids[0])
            }
            NodeKind::Binary => kids.iter().all(|&k| self.literal_operand(k)),
            _ => false,
        }
    }
}

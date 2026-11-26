use super::*;

impl Program<'_> {
    /// LANG-7 constant heads and explicit projections read the published field.
    pub(crate) fn constant_field(
        &self,
        from: ModuleId,
        value: ConstValue,
        name: &str,
    ) -> Result<ConstValue, &'static str> {
        let ConstData::Fields {
            start,
            len,
            case: None,
        } = value.data
        else {
            return Err("LANG-11");
        };
        let index = match self.types.kind(value.ty) {
            TyKind::Tuple(_) => name.parse::<usize>().map_err(|_| "LANG-11")?,
            TyKind::Adt(d, _) => {
                let decl = &self.decls[d.index()];
                if decl.opaque && decl.site.module != from {
                    return Err("PROG-2");
                }
                match &decl.body {
                    DeclBody::Record(fs) => {
                        fs.iter().position(|f| f.name == name).ok_or("LANG-11")?
                    }
                    _ => return Err("LANG-11"),
                }
            }
            _ => return Err("LANG-11"),
        };
        if index < len as usize {
            Ok(self.constant_parts[start as usize + index])
        } else {
            Err("LANG-11")
        }
    }
    pub(crate) fn constant_field_refusal(
        &self,
        rule: &str,
        site: Site,
        span: hirsel_core_syntax::Span,
        value: ConstValue,
        name: &str,
    ) -> Refusal {
        let refusal = refusal(rule, site, "constant field is absent or opaque");
        if rule == "LANG-11"
            && let TyKind::Adt(d, _) = self.types.kind(value.ty)
        {
            let path = self.decls[d.index()].path.clone();
            return refusal.with_unresolved(
                name,
                span,
                hirsel_core_resolve::NameScope::Field(path),
            );
        }
        refusal
    }
}

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

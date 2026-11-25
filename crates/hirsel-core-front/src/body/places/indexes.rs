use super::*;

impl Cx<'_, '_, '_> {
    /// LANG-21's closed index row, shared by named and temporary bases.
    pub(in crate::body) fn index_element(
        &mut self,
        collection: TyId,
        n: NodeId,
    ) -> Result<TyId, ()> {
        match self.t.kind(collection) {
            TyKind::Array(elem, _) => Ok(*elem),
            TyKind::Adt(d, args)
                if matches!(
                    self.p.decls[d.index()].role,
                    Some(Role::List | Role::FixedSpan)
                ) =>
            {
                Ok(args[0])
            }
            TyKind::Error => Err(()),
            _ => {
                let a = self.shown(collection);
                let required = "an array, list or fixed span";
                self.refuse_compared(
                    "LANG-21",
                    n,
                    a,
                    required,
                    "this type has no index operation",
                );
                Err(())
            }
        }
    }
    /// An index operand is a `u64` (LANG-11); one whose own type is `u8`, `u16` or
    /// `u32`, through parentheses, is zero-extended to it (LANG-21). No other type
    /// converts: a signed or other index is refused as any `u64` operand is.
    pub(in crate::body) fn index_operand(&mut self, mut e: NodeId) -> Option<ValueId> {
        while self.kind(e) == NodeKind::Group {
            e = self.kids(e)[0];
        }
        let u64_ty = scalar(Scalar::U64);
        if !self.synthesises(e) {
            return self.expr(e, expect(u64_ty)).1;
        }
        let (t, v) = self.operand(e, expect(u64_ty));
        if !matches!(int_of(t), Some((false, 8 | 16 | 32))) {
            if t != NEVER {
                self.coerce(t, expect(u64_ty).unwrap(), e);
            }
            return v;
        }
        let operand = v?;
        if !self.live() {
            return None;
        }
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Convert {
                result,
                flag: None,
                operand,
                mode: ConvertMode::Widen,
            },
            e,
        );
        Some(result)
    }
}

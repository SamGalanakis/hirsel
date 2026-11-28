use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn statements(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let stmts = self.kids(n);
        let mut diverged = false;
        // A statement that failed to parse may have been the block's value (PROG-8).
        let lost = stmts.iter().any(|&s| self.kind(s) == NodeKind::Error);
        for (i, &s) in stmts.iter().enumerate() {
            crate::cost_scope!("construct", format!("{:?}", self.kind(s)));
            crate::cost_scope!(
                "construct-state",
                format!(
                    "{:?}/{}",
                    self.kind(s),
                    if self.broken { "gap" } else { "live" }
                )
            );
            self.out.counts.nodes += 1;
            let last = i + 1 == stmts.len();
            let freezes = self.freezes.len();
            let owners = self.owner_mark();
            match self.kind(s) {
                // The tail's temporaries last to the end of the enclosing statement.
                NodeKind::ExpressionStatement if last => {
                    let r = self.result_expr(self.kids(s)[0], want);
                    self.tail_ty = self.synth_ty;
                    return r;
                }
                NodeKind::ExpressionStatement | NodeKind::Discard => {
                    let (t, _) = self.expr(self.kids(s)[0], None);
                    if self.result_parts(t).is_some_and(|(_, _, e)| e.is_some()) {
                        self.refuse(
                            "LANG-18",
                            self.kids(s)[0],
                            "an unused Result is discarded explicitly with `let _`",
                        );
                        return (ERROR, None);
                    }
                    diverged |= t == NEVER;
                }
                NodeKind::Let | NodeKind::LetMut => diverged |= self.let_(s),
                NodeKind::Assign => diverged |= self.assign(s),
                NodeKind::Error => self.halt(),
                _ => unreachable!("the parser produces only the handled statement kinds"),
            }
            if self.out.resource_refusal.is_some() {
                return (ERROR, None);
            }
            // A temporary view ends with its statement (OWN-8), and so does every
            // temporary nothing took (LANG-6).
            self.unfreeze(freezes, |h| (h != Hold::View(Extent::Temp)).then_some(h));
            self.end_owners(owners, None, true, s);
        }
        let t = if diverged {
            NEVER
        } else if lost {
            ERROR
        } else {
            UNIT
        };
        match want {
            Some(w) if t != NEVER => {
                self.coerce(t, w, n);
                (if w.payload == 0 { w.ty } else { t }, None)
            }
            _ => (t, None),
        }
    }
}

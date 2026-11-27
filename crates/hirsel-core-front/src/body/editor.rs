use super::*;

/// A checked type at an authored, item-relative byte span. Local type-arena
/// indices are rendered before they leave the body that owns them.
pub(super) type EditorType = (Span, String, Option<DefPath>);

impl Program<'_> {
    pub fn check_lower_editor(
        &self,
        f: usize,
        emitter: &mut Emitter,
    ) -> (CoreCheck, Vec<EditorType>) {
        emitter.editor_types = Some(Vec::new());
        let check = self.check_lower(f, emitter);
        (check, emitter.editor_types.take().unwrap())
    }

    /// Check function `f`'s body and emit it into `e` (LANG-8: one visit, reading only
    /// this body, signatures and summaries).
    pub fn check_lower(&self, f: usize, e: &mut Emitter) -> CoreCheck {
        crate::cost_scope!("body", self.fns[f].path.to_string());
        let fd = &self.fns[f];
        let Some(body) = fd.body else {
            return CoreCheck::default();
        };
        crate::cost_scope!("phase", "body typing");
        let mut cx = Cx::new(self, f, e);
        cx.run(f, body);
        let mut out = cx.finish();
        if self.entry.as_ref() == Some(&fd.path) {
            self.check_entry(fd, &mut out.refusals);
            if !out.refusals.is_empty() {
                out.gaps.clear();
            }
        }
        out
    }
}

impl Cx<'_, '_, '_> {
    pub(super) fn editor_bindings(&mut self) {
        if self.e.editor_types.is_some() {
            let binders = &self.p.items[self.at.module as usize][self.at.item as usize]
                .resolution
                .binders;
            for (id, binder) in binders.iter().enumerate() {
                if let Some(var) = self.vars[id] {
                    self.editor_type(binder.site.span, var.ty);
                }
            }
        }
    }

    pub(super) fn editor_type(&mut self, span: Span, ty: TyId) {
        if ty != ERROR && ty != HOLE && !span.is_empty() {
            let nominal = match self.t.kind(ty) {
                TyKind::Adt(decl, _) => Some(self.p.decls[decl.index()].path.clone()),
                _ => None,
            };
            let text = self.shown(ty);
            self.e
                .editor_types
                .as_mut()
                .unwrap()
                .push((span, text, nominal));
        }
    }
}

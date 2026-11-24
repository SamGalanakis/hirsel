use super::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum NamePart {
    Text(String),
    Parameter(u32),
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct NameRecipe(pub Vec<NamePart>);
impl NameRecipe {
    pub(super) fn render(&self, arguments: &[String]) -> String {
        let mut result = String::new();
        for part in &self.0 {
            match part {
                NamePart::Text(text) => result.push_str(text),
                NamePart::Parameter(i) => result.push_str(&arguments[*i as usize]),
            }
        }
        result
    }
    pub(in crate::body) fn dictionary(bound: &str, argument: &Self) -> Self {
        let mut recipe = Self::default();
        recipe.text(format!("{bound}["));
        recipe.0.extend(argument.0.iter().cloned());
        recipe.text("]");
        recipe
    }
    fn text(&mut self, text: impl Into<String>) {
        self.0.push(NamePart::Text(text.into()));
    }
    fn ty(&mut self, p: &Program, t: &impl TyRead, ty: TyId) {
        if !has_param(t, ty) {
            self.text(crate::decls::layout_name(&p.decls, t, ty));
            return;
        }
        match t.kind(ty) {
            TyKind::Param(i) => self.0.push(NamePart::Parameter(*i)),
            TyKind::Tuple(xs) => {
                self.text("(");
                for (i, &x) in xs.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.ty(p, t, x);
                }
                if xs.len() == 1 {
                    self.text(",");
                }
                self.text(")");
            }
            TyKind::Array(x, n) => {
                self.text("Inline[");
                self.ty(p, t, *x);
                self.text(format!(", {n}]"));
            }
            TyKind::View(x) => {
                self.text("View[");
                self.ty(p, t, *x);
                self.text("]");
            }
            TyKind::Adt(d, xs) => {
                self.text(p.decls[d.index()].path.to_string());
                self.text("[");
                for (i, &x) in xs.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.ty(p, t, x);
                }
                self.text("]");
            }
            TyKind::Fn(ps, r, waits, kind) => {
                self.text(match kind {
                    Callable::Plain => "fn(",
                    Callable::Own => "own fn(",
                    Callable::OwnSend => "own[send] fn(",
                    Callable::Scoped => "scoped fn(",
                    Callable::ScopedView => "scoped[view] fn(",
                });
                for (i, &(mode, ty)) in ps.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(match mode {
                        Mode::View => "",
                        Mode::Mut => "mut ",
                        Mode::Take => "take ",
                        Mode::Bytes => "bytes ",
                        Mode::Steps => "steps ",
                    });
                    self.ty(p, t, ty);
                }
                self.text(") -> ");
                self.ty(p, t, *r);
                if *waits {
                    self.text(" waits");
                }
            }
            TyKind::Anchored(x, _) => self.ty(p, t, *x),
            TyKind::Region(_) => self.text(crate::decls::layout_name(&p.decls, t, ty)),
            _ => unreachable!("has_param identifies a structural type or a parameter"),
        }
    }
    pub(in crate::body) fn of(p: &Program, t: &impl TyRead, mut ty: TyId) -> Self {
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *t.kind(ty) {
            ty = x;
        }
        let mut name = Self::default();
        name.ty(p, t, ty);
        name
    }
}

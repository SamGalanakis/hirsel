use super::*;

/// The kind of a type (OWN-3). `params` are the bounds of the enclosing definition's
/// type parameters.
pub fn kind(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> Kind {
    kind_row(decls, t, t.kind(ty), params)
}

pub(super) fn kind_row(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    row: &TyKind,
    params: &[u16],
) -> Kind {
    kind_row_with(decls, row, params, |ty| kind(decls, t, ty, params))
}

fn kind_row_with(
    decls: &crate::Rows<TypeDecl>,
    row: &TyKind,
    params: &[u16],
    mut child: impl FnMut(TyId) -> Kind,
) -> Kind {
    crate::cost_scope!("query", "kind computation");
    match row {
        TyKind::Error
        | TyKind::Hole
        | TyKind::Never
        | TyKind::Unit
        | TyKind::Scalar(_)
        | TyKind::Vector(_) => Kind::Plain,
        TyKind::Fn(_, _, _, callable) => match callable {
            Callable::Plain => Kind::Plain,
            Callable::Own | Callable::OwnSend => Kind::Own,
            Callable::Scoped | Callable::ScopedView => Kind::Scoped,
        },
        TyKind::Tuple(xs) => xs.iter().fold(Kind::Plain, |k, &x| join(k, child(x))),
        TyKind::Region(_) => Kind::Scoped,
        TyKind::Anchored(x, _) => {
            let k = child(*x);
            if k == Kind::Generic { Kind::Region } else { k }
        }
        TyKind::Array(x, _) => child(*x),
        TyKind::View(_) => Kind::Scoped,
        TyKind::Param(i) => {
            if params.get(*i as usize).is_some_and(|b| b & PLAIN != 0) {
                Kind::Plain
            } else {
                Kind::Generic
            }
        }
        TyKind::Adt(d, args) => {
            let decl = &decls[d.index()];
            if let Some(r) = decl.role {
                return r.kind();
            }
            match decl.repr {
                Repr::Region => Kind::Region,
                Repr::Resource => Kind::Resource,
                Repr::Scoped => Kind::Scoped,
                Repr::Own => {
                    let s = &decl.summary;
                    let mut k = s.kind_fixed.unwrap_or(Kind::Own);
                    if decl.structural {
                        k = Kind::Plain;
                    }
                    for (i, &a) in args.iter().enumerate() {
                        if s.inline_params & (1 << i) != 0 {
                            k = join(k, child(a));
                        }
                    }
                    k
                }
            }
        }
    }
}

/// Kind queries over one arena and declaration table, with no enclosing bounds.
#[derive(Default)]
pub(crate) struct LayoutKinds(rustc_hash::FxHashMap<TyId, Kind>);

impl LayoutKinds {
    pub(crate) fn get(
        &mut self,
        decls: &crate::Rows<TypeDecl>,
        types: &impl TyRead,
        ty: TyId,
    ) -> Kind {
        if let Some(&kind) = self.0.get(&ty) {
            return kind;
        }
        let kind = kind_row_with(decls, types.kind(ty), &[], |child| {
            self.get(decls, types, child)
        });
        self.0.insert(ty, kind);
        kind
    }
}

/// Region roots of a type. The caller supplies and reuses the output arena.
pub fn collect_roots(t: &impl TyRead, ty: TyId, out: &mut Vec<TyId>) {
    collect_roots_inner(t, ty, out, &mut rustc_hash::FxHashSet::default());
}

fn collect_roots_inner(
    t: &impl TyRead,
    ty: TyId,
    out: &mut Vec<TyId>,
    seen: &mut rustc_hash::FxHashSet<TyId>,
) {
    if !seen.insert(ty) {
        return;
    }
    match t.kind(ty) {
        TyKind::Region(_) => {
            if !out.contains(&ty) {
                out.push(ty);
            }
        }
        TyKind::Anchored(x, r) => {
            collect_roots_inner(t, *x, out, seen);
            collect_roots_inner(t, *r, out, seen);
        }
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => {
            for &x in xs {
                collect_roots_inner(t, x, out, seen);
            }
        }
        TyKind::View(x) | TyKind::Array(x, _) => collect_roots_inner(t, *x, out, seen),
        _ => {}
    }
}

pub(super) fn declaration_region_params(t: &impl TyRead, decl: &TypeDecl) -> Vec<u32> {
    let mut roots = vec![];
    let mut seen = rustc_hash::FxHashSet::default();
    let mut collect = |ty| {
        if has_param(t, ty) {
            collect_roots_inner(t, ty, &mut roots, &mut seen);
        }
    };
    match decl.body {
        DeclBody::Alias(ty) => collect(ty),
        _ => decl.all_fields().for_each(|f| collect(f.ty)),
    }
    roots
        .into_iter()
        .filter_map(|r| match t.kind(r) {
            TyKind::Region(Root::Generic(i)) => Some(*i),
            _ => None,
        })
        .collect()
}

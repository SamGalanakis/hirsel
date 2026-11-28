use super::*;

pub(super) fn fill_call_type(
    t: &mut impl TyRead,
    ty: TyId,
    found: &[Option<TyId>],
    roots: &[Option<TyId>],
) -> TyId {
    if roots.is_empty() && !has_param(t, ty) {
        return ty;
    }
    match t.kind(ty).clone() {
        TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => found[i as usize].unwrap_or(HOLE),
        TyKind::Region(Root::Parameter(i)) => {
            roots.get(i as usize).copied().flatten().unwrap_or(ty)
        }
        TyKind::Anchored(x, root) => {
            let x = fill_call_type(t, x, found, roots);
            let root = fill_call_type(t, root, found, roots);
            t.mk(TyKind::Anchored(x, root))
        }
        TyKind::Array(x, n) => {
            let x = fill_call_type(t, x, found, roots);
            t.mk(TyKind::Array(x, n))
        }
        TyKind::Tuple(xs) => {
            let xs = xs
                .into_iter()
                .map(|x| fill_call_type(t, x, found, roots))
                .collect();
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, xs) => {
            let xs = xs
                .into_iter()
                .map(|x| fill_call_type(t, x, found, roots))
                .collect();
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::View(x) => {
            let x = fill_call_type(t, x, found, roots);
            t.mk(TyKind::View(x))
        }
        TyKind::Fn(ps, r, w, k) => {
            let ps = ps
                .into_iter()
                .map(|(m, p)| (m, fill_call_type(t, p, found, roots)))
                .collect();
            let r = fill_call_type(t, r, found, roots);
            t.mk(TyKind::Fn(ps, r, w, k))
        }
        _ => ty,
    }
}

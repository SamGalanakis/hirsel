//! The encodings a declaration admits (OWN-2), derived by one walk of its fields.

use super::*;

/// One declaration's encoding walk: the declarations open on it, and the bits of each
/// type it has finished whose walk re-entered no open declaration, which are the same
/// wherever the type occurs again, so a type held at many places is walked once.
#[derive(Default)]
pub(super) struct EncodingWalk {
    stack: Vec<(DeclId, Vec<TyId>)>,
    done: rustc_hash::FxHashMap<(TyId, bool), u16>,
    reentries: usize,
}

pub(super) fn derive_encoding(
    decls: &crate::Rows<TypeDecl>,
    t: &mut Types,
    ty: TyId,
    params: &[u16],
    stack: &mut EncodingWalk,
    compact_field: bool,
) -> u16 {
    if let Some(&bits) = stack.done.get(&(ty, compact_field)) {
        return bits;
    }
    let reentries = stack.reentries;
    let bits = derive_encoding_walk(decls, t, ty, params, stack, compact_field);
    if stack.reentries == reentries {
        stack.done.insert((ty, compact_field), bits);
    }
    bits
}

fn derive_encoding_walk(
    decls: &crate::Rows<TypeDecl>,
    t: &mut Types,
    ty: TyId,
    params: &[u16],
    stack: &mut EncodingWalk,
    compact_field: bool,
) -> u16 {
    let all = ENCODED | FIXED_ENCODING;
    match t.kind(ty).clone() {
        TyKind::Scalar(_) | TyKind::Error => all,
        TyKind::Anchored(x, _) => derive_encoding(decls, t, x, params, stack, compact_field),
        TyKind::Param(i) => params.get(i as usize).copied().unwrap_or(0) & all,
        TyKind::Tuple(xs) if !xs.is_empty() && kind(decls, t, ty, params) == Kind::Plain => {
            xs.into_iter().fold(all, |b, x| {
                b & derive_encoding(decls, t, x, params, stack, false)
            })
        }
        TyKind::Adt(d, args) => {
            let decl = &decls[d.index()];
            match decl.role {
                Some(Role::Text) => ENCODED,
                Some(Role::Link) if compact_field => PACKED | FIXED_ENCODING,
                Some(Role::FixedSpan) => {
                    if derive_encoding(decls, t, args[0], params, stack, false) & FIXED_ENCODING
                        != 0
                    {
                        ENCODED
                    } else {
                        0
                    }
                }
                Some(Role::SeqSpan | Role::IndexedSpan) => {
                    derive_encoding(decls, t, args[0], params, stack, false) & ENCODED
                }
                Some(_) => 0,
                None if decl.structural
                    && compact_field
                    && is_core_prelude(&decl.path, &["Option"])
                    && matches!(t.kind(args[0]), TyKind::Adt(link, _) if decls[link.index()].role == Some(Role::Link)) =>
                {
                    PACKED | FIXED_ENCODING
                }
                None => {
                    let compact = decl.compact.is_some();
                    if compact && matches!(&decl.body, DeclBody::Record(fs) if fs.is_empty()) {
                        return 0;
                    }
                    if !compact
                        && (decl.repr != Repr::Own
                            || !matches!(decl.body, DeclBody::Record(_))
                            || kind(decls, t, ty, params) != Kind::Plain
                            || decl.all_fields().next().is_none())
                    {
                        return 0;
                    }
                    if let Some((_, previous)) = stack.stack.iter().find(|(id, _)| *id == d) {
                        stack.reentries += 1;
                        return if previous == &args { all } else { 0 };
                    }
                    stack.stack.push((d, args.clone()));
                    let mut bits = all;
                    for f in decl.all_fields() {
                        let ft = subst(t, f.ty, &args);
                        bits &= derive_encoding(decls, t, ft, params, stack, compact);
                    }
                    stack.stack.pop();
                    if compact {
                        bits &= !FIXED_ENCODING;
                    }
                    bits
                }
            }
        }
        _ => 0,
    }
}

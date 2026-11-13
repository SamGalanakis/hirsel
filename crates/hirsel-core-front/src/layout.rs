//! Layout obligations of declarations with no parameters: their shapes go to
//! `hirsel_core_layout::compute` once, with the closed types they hold inline. A
//! generic declaration's layout is its instance's, made when the instance is.

use crate::Program;
use crate::decls::{DeclBody, Repr, Role};
use crate::types::*;
use hirsel_core_layout::{
    Body, Case, Field, FieldTy, FuncRef, Handle, LayoutError, Repr as LRepr, Scalar, Shape,
    TypeRef, compute,
};
use std::collections::HashMap;

struct Shapes<'p> {
    decls: &'p crate::Rows<crate::decls::TypeDecl>,
    types: &'p mut Types,
    shapes: Vec<Shape>,
    of: HashMap<TyId, TypeRef>,
}

impl Shapes<'_> {
    /// The shape of a closed type held inline, or None when the layout crate has no
    /// field type for some part of it (recorded as a gap by the caller).
    fn shape(&mut self, ty: TyId) -> Option<TypeRef> {
        if let Some(&r) = self.of.get(&ty) {
            return Some(r);
        }
        let name = crate::decls::canonical(self.decls, self.types, ty);
        // Reserve the index first: a field reaching this type through an indirection
        // (`List[Tree]` in `Tree`) names it before its body is known.
        let r = TypeRef(self.shapes.len() as u32);
        self.shapes.push(Shape {
            name,
            repr: LRepr::Own,
            body: Body::Record(vec![]),
            finalizer: None,
            grant: 0,
        });
        self.of.insert(ty, r);
        let (repr, body, finalizer) = match self.types.kind(ty).clone() {
            TyKind::Scalar(s) => (
                LRepr::Own,
                Body::Record(vec![Field {
                    name: "0".into(),
                    ty: FieldTy::Scalar(s),
                }]),
                None,
            ),
            TyKind::Unit => (LRepr::Own, Body::Record(vec![]), None),
            TyKind::Array(x, n) => (
                LRepr::Own,
                Body::Record(vec![Field {
                    name: "0".into(),
                    ty: FieldTy::Array(Box::new(self.field(x)?), n),
                }]),
                None,
            ),
            TyKind::Tuple(xs) => {
                let mut fields = vec![];
                for (i, x) in xs.into_iter().enumerate() {
                    fields.push(Field {
                        name: i.to_string(),
                        ty: self.field(x)?,
                    });
                }
                (LRepr::Own, Body::Record(fields), None)
            }
            TyKind::Adt(d, args) => {
                let decls = self.decls;
                let decl = &decls[d.index()];
                if decl.role.is_some() || decl.summary.cycle {
                    return None;
                }
                let repr = match decl.repr {
                    Repr::Own => LRepr::Own,
                    Repr::Resource => LRepr::Resource,
                    Repr::Scoped => LRepr::Scoped,
                    Repr::Stable => return None,
                };
                let map = |fs: &[crate::decls::FieldDecl], s: &mut Self| {
                    let mut out = vec![];
                    for f in fs {
                        let fty = subst(s.types, f.ty, &args);
                        out.push(Field {
                            name: f.name.clone(),
                            ty: s.field(fty)?,
                        });
                    }
                    Some(out)
                };
                let body = match &decl.body {
                    DeclBody::Record(fs) => Body::Record(map(fs, self)?),
                    DeclBody::Variant(cs) => {
                        let mut cases = vec![];
                        for c in cs {
                            cases.push(Case {
                                name: c.name.clone(),
                                fields: map(&c.fields, self)?,
                            });
                        }
                        Body::Variant(cases)
                    }
                    DeclBody::Alias(_) => return None,
                };
                (repr, body, decl.finalizer.then_some(FuncRef(d.0)))
            }
            _ => return None,
        };
        let shape = &mut self.shapes[r.index()];
        shape.repr = repr;
        shape.body = body;
        shape.finalizer = finalizer;
        Some(r)
    }

    fn field(&mut self, ty: TyId) -> Option<FieldTy> {
        Some(match self.types.kind(ty).clone() {
            TyKind::Scalar(s) => FieldTy::Scalar(s),
            TyKind::Adt(d, args) => match self.decls[d.index()].role {
                Some(Role::Box) => FieldTy::Box(self.shape(args[0])?),
                Some(Role::Rc) => FieldTy::Rc(self.shape(args[0])?),
                Some(Role::Arc) => FieldTy::Arc(self.shape(args[0])?),
                Some(Role::List) => FieldTy::Seq(Box::new(self.field(args[0])?)),
                Some(Role::String) => FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8))),
                Some(Role::Task) => FieldTy::Handle(Handle::Task),
                Some(Role::Sender) => FieldTy::Handle(Handle::Sender),
                Some(Role::Receiver) => FieldTy::Handle(Handle::Receiver),
                Some(Role::HostFile) => FieldTy::Handle(Handle::HostFile),
                Some(_) => return None,
                None => FieldTy::Inline(self.shape(ty)?),
            },
            TyKind::Array(x, n) => {
                if !matches!(
                    crate::decls::kind(self.decls, self.types, x, &[]),
                    crate::Kind::Plain | crate::Kind::Own
                ) {
                    return None;
                }
                FieldTy::Array(Box::new(self.field(x)?), n)
            }
            TyKind::Tuple(_) | TyKind::Unit => FieldTy::Inline(self.shape(ty)?),
            _ => return None,
        })
    }
}

/// Recipes for every declaration without parameters whose fields all map.
pub(crate) fn recipes(p: &mut Program) {
    let Program { decls, types, .. } = p;
    let mut s = Shapes {
        decls,
        types,
        shapes: vec![],
        of: HashMap::new(),
    };
    let mut roots = vec![];
    for (d, decl) in s.decls.iter().enumerate().skip(s.decls.base_len()) {
        if !decl.generics.is_empty()
            || decl.role.is_some()
            || matches!(decl.body, DeclBody::Alias(_))
        {
            continue;
        }
        let ty = s.types.intern(TyKind::Adt(crate::DeclId(d as u32), vec![]));
        let mark = s.shapes.len();
        match s.shape(ty) {
            Some(r) => roots.push((d, r)),
            None => {
                // A root whose fields do not all map leaves no partial shapes behind.
                s.shapes.truncate(mark);
                s.of.retain(|_, r| (r.0 as usize) < mark);
            }
        }
    }
    match compute(&s.shapes) {
        Ok(recipes) => {
            for (d, r) in roots {
                p.decls[d].summary.recipe = Some(recipes[r.index()].clone());
            }
        }
        Err(error) => {
            let (name, field, why) = match &error {
                LayoutError::Field { ty, field, why } => (ty, Some(field.as_str()), *why),
                LayoutError::Cycle { ty } => (ty, None, "inline containment cycle"),
                LayoutError::TooLarge { ty } => (ty, None, "compact extent exceeds its width"),
                LayoutError::Empty { ty } => (ty, None, "compact encoding has zero size"),
            };
            let declaration = s.decls.iter().find(|d| d.path.to_string() == *name);
            if let Some(decl) = declaration {
                let site = field
                    .and_then(|name| decl.all_fields().find(|f| f.name == name))
                    .map_or(decl.site, |f| f.site);
                let rule = if why.contains("overflow") || why.contains("2^64") {
                    "OWN-2"
                } else if decl.repr == Repr::Stable {
                    "MEM-2"
                } else if matches!(error, LayoutError::Cycle { .. }) {
                    "OWN-2"
                } else {
                    "OWN-19"
                };
                p.refusals.push(crate::refusal(rule, site, why));
            } else if let Some(&(d, _)) = roots.first() {
                p.gaps.push(crate::Gap::new(
                    s.decls[d].site,
                    "nested layout failure is not diagnosed",
                ));
            }
        }
    }
}

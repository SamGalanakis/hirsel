//! Shapes for `hirsel_core_layout::compute`. A declaration with no parameters gets its
//! recipe once, with the closed types it holds inline; a body module's type table
//! (`Module::types`) gets one row per closed type its signatures and values use,
//! instances of generic declarations included (FORM v1b: `Option` and `Result` are
//! ordinary variant rows, one per instantiation).

use crate::Program;
use crate::decls::{DeclBody, Repr, Role};
use crate::types::*;
use hirsel_core_layout::{
    Body, Case, Field, FieldTy, FuncRef, Handle, LayoutError, Repr as LRepr, Scalar, Shape,
    TypeRef, compute,
};
use std::collections::HashMap;

/// A shape table being extended: rows are keyed by canonical text, which is also the
/// row's name, so one closed type has one row whichever body names it.
pub(crate) struct Shapes<'p, T: TyRead> {
    pub decls: &'p crate::Rows<crate::decls::TypeDecl>,
    pub types: &'p mut T,
    pub shapes: &'p mut Vec<Shape>,
    pub of: &'p mut HashMap<String, TypeRef>,
    /// A body module's table: a finalizer names a function row this table cannot name
    /// yet, so a type with one has no row here (resource types are not built).
    pub module: bool,
}

impl<T: TyRead> Shapes<'_, T> {
    /// The shape of a closed type held inline, or None when the layout crate has no
    /// field type for some part of it (recorded as a gap by the caller).
    pub fn shape(&mut self, ty: TyId) -> Option<TypeRef> {
        let name = crate::decls::canonical(self.decls, &*self.types, ty);
        if let Some(&r) = self.of.get(&name) {
            return Some(r);
        }
        if let TyKind::Anchored(x, _) = *self.types.kind(ty) {
            return self.shape(x);
        }
        // Reserve the index first: a field reaching this type through an indirection
        // (`List[Tree]` in `Tree`) names it before its body is known.
        let r = TypeRef(self.shapes.len() as u32);
        self.shapes.push(Shape {
            name: name.clone(),
            repr: LRepr::Own,
            body: Body::Record(vec![]),
            finalizer: None,
            grant: 0,
        });
        self.of.insert(name, r);
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
                match decl.role {
                    // The core collections' carriers (message.core: `own "String"`).
                    Some(Role::String) => {
                        let bytes = FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8)));
                        return self.fill(r, LRepr::Own, record(vec![("bytes", bytes)]), None);
                    }
                    Some(Role::List) => {
                        let items = FieldTy::Seq(Box::new(self.field(args[0])?));
                        return self.fill(r, LRepr::Own, record(vec![("items", items)]), None);
                    }
                    Some(_) => return None,
                    None => {}
                }
                if decl.summary.cycle || self.module && decl.finalizer {
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
        self.fill(r, repr, body, finalizer)
    }
    fn fill(
        &mut self,
        r: TypeRef,
        repr: LRepr,
        body: Body,
        finalizer: Option<FuncRef>,
    ) -> Option<TypeRef> {
        let shape = &mut self.shapes[r.index()];
        shape.repr = repr;
        shape.body = body;
        shape.finalizer = finalizer;
        Some(r)
    }
    /// Forget the rows from `mark` on: a type whose parts do not all map leaves no
    /// partial rows behind.
    pub fn truncate(&mut self, mark: usize) {
        self.shapes.truncate(mark);
        self.of.retain(|_, r| (r.0 as usize) < mark);
    }

    fn field(&mut self, ty: TyId) -> Option<FieldTy> {
        Some(match self.types.kind(ty).clone() {
            TyKind::Scalar(s) => FieldTy::Scalar(s),
            TyKind::Anchored(x, _) => self.field(x)?,
            TyKind::Adt(d, args) => match self.decls[d.index()].role {
                Some(Role::Box) => FieldTy::Box(self.shape(args[0])?),
                Some(Role::Rc) => FieldTy::Rc(self.shape(args[0])?),
                Some(Role::Arc) => FieldTy::Arc(self.shape(args[0])?),
                Some(Role::List) => FieldTy::Seq(Box::new(self.field(args[0])?)),
                Some(Role::String) => FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8))),
                _ if handle(&self.decls[d.index()]) => FieldTy::Scalar(Scalar::U64),
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

/// A capability or `Args`: a `u64` provider handle (FORM e68de117).
pub(crate) fn handle(decl: &crate::decls::TypeDecl) -> bool {
    decl.role == Some(Role::Capability) || crate::decls::is_core_prelude(&decl.path, &["Args"])
}

fn record(fields: Vec<(&str, FieldTy)>) -> Body {
    Body::Record(
        fields
            .into_iter()
            .map(|(name, ty)| Field {
                name: name.into(),
                ty,
            })
            .collect(),
    )
}

/// Recipes for every declaration without parameters whose fields all map.
pub(crate) fn recipes(p: &mut Program) {
    let Program { decls, types, .. } = p;
    let (mut shapes, mut of) = (vec![], HashMap::new());
    let mut s = Shapes {
        decls,
        types,
        shapes: &mut shapes,
        of: &mut of,
        module: false,
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
            None => s.truncate(mark),
        }
    }
    match compute(s.shapes) {
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

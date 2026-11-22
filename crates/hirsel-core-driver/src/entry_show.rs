//! PROG-3: the text program for `main`'s error value. Check describes `E` from
//! its source declarations (names, labels, which carrier is a `String`, which
//! capability is a `Secret`); the build places that description on the entry
//! module's layout rows, giving the [`Show`] the run renders.
use hirsel_core_front::{DeclBody, Role, TyId, TyKind, Types};
use hirsel_core_layout::show::{Case, Node, Part, Show};
use hirsel_core_layout::{Body, FieldTy, Layout, Recipe, Scalar, Shape, TypeRef, field_size};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A source type, as its text needs it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Shown {
    Text(String),
    Scalar(Scalar),
    Str,
    List(Box<Shown>),
    Array(Box<Shown>, u64),
    Boxed(Box<Shown>),
    Tuple(Vec<Shown>),
    /// A declared record or variant, by its index in [`ErrorShown::defs`].
    Def(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ShownCase {
    name: String,
    labelled: bool,
    fields: Vec<(String, Shown)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum ShownDef {
    Record(String, Vec<(String, Shown)>),
    Variant(Vec<ShownCase>),
}

/// `E` of an entry's `Result[(), E]`, described at check.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ErrorShown {
    defs: Vec<ShownDef>,
    root: Shown,
}

pub(crate) fn describe(
    types: &mut Types,
    decls: &hirsel_core_front::Rows<hirsel_core_front::TypeDecl>,
    ty: TyId,
) -> ErrorShown {
    let mut d = Describe {
        types,
        decls,
        defs: Vec::new(),
        seen: HashMap::new(),
    };
    let root = d.ty(ty);
    ErrorShown { defs: d.defs, root }
}

struct Describe<'a> {
    types: &'a mut Types,
    decls: &'a hirsel_core_front::Rows<hirsel_core_front::TypeDecl>,
    defs: Vec<ShownDef>,
    seen: HashMap<TyId, u32>,
}

impl Describe<'_> {
    fn ty(&mut self, ty: TyId) -> Shown {
        match self.types.kind(ty).clone() {
            TyKind::Unit | TyKind::Never => Shown::Tuple(vec![]),
            TyKind::Scalar(s) => Shown::Scalar(s),
            TyKind::Tuple(xs) => Shown::Tuple(xs.into_iter().map(|x| self.ty(x)).collect()),
            TyKind::Array(x, n) => Shown::Array(Box::new(self.ty(x)), n),
            TyKind::Anchored(x, _) | TyKind::View(x) => self.ty(x),
            TyKind::Fn(..) => Shown::Text("<fn>".into()),
            TyKind::Adt(d, args) => self.adt(ty, d, args),
            _ => Shown::Text("<value>".into()),
        }
    }

    fn adt(&mut self, ty: TyId, d: hirsel_core_front::DeclId, args: Vec<TyId>) -> Shown {
        let decl = &self.decls[d.index()];
        let name = decl.path.0.name.to_string();
        let opaque = || Shown::Text(format!("<{name}>"));
        match decl.role {
            Some(Role::String) => return Shown::Str,
            Some(Role::List) => return Shown::List(Box::new(self.ty(args[0]))),
            Some(Role::Box) => return Shown::Boxed(Box::new(self.ty(args[0]))),
            // TLS-5: a run diagnostic prints `[secret]` for every secret.
            Some(Role::Secret) => {
                return Shown::Text("[secret]".into());
            }
            Some(_) => return opaque(),
            None => {}
        }
        if decl.opaque || decl.compact.is_some() || decl.repr == hirsel_core_front::Repr::Region {
            return opaque();
        }
        if let Some(&index) = self.seen.get(&ty) {
            return Shown::Def(index);
        }
        let body = decl.body.clone();
        if let DeclBody::Alias(target) = body {
            let target = hirsel_core_front::subst(self.types, target, &args);
            return self.ty(target);
        }
        let index = self.defs.len() as u32;
        self.seen.insert(ty, index);
        self.defs.push(ShownDef::Record(String::new(), vec![]));
        let fields = |d: &mut Self, fs: &[hirsel_core_front::FieldDecl]| {
            fs.iter()
                .map(|f| {
                    let fty = hirsel_core_front::subst(d.types, f.ty, &args);
                    (f.name.clone(), d.ty(fty))
                })
                .collect::<Vec<_>>()
        };
        let def = match &body {
            DeclBody::Record(fs) => ShownDef::Record(name, fields(self, fs)),
            DeclBody::Variant(cs) => ShownDef::Variant(
                cs.iter()
                    .map(|c| ShownCase {
                        name: c.name.clone(),
                        labelled: c.labelled,
                        fields: fields(self, &c.fields),
                    })
                    .collect(),
            ),
            DeclBody::Alias(_) => unreachable!("an alias is followed above"),
        };
        self.defs[index as usize] = def;
        Shown::Def(index)
    }
}

/// Place `shown` on the entry module's rows: `E` is the `Error` case's field of
/// the `Result` row at index `dest`.
pub(crate) fn place(
    shown: &ErrorShown,
    shapes: &[Shape],
    recipes: &[Recipe],
    dest: usize,
) -> Result<Show, &'static str> {
    let (Body::Variant(cases), Layout::Fixed(result)) = (&shapes[dest].body, &recipes[dest].layout)
    else {
        return Err("entry Result needs a fixed variant");
    };
    let field = cases
        .get(1)
        .and_then(|case| case.fields.first())
        .ok_or("entry Result has no Error payload")?;
    let offset = *result
        .offsets
        .get(1)
        .and_then(|fields| fields.first())
        .ok_or("entry Result has no Error payload offset")?;
    let mut p = Place {
        shown,
        shapes,
        recipes,
        nodes: Vec::new(),
        placed: HashMap::new(),
    };
    let root = p.node(&shown.root, &field.ty);
    Ok(Show {
        nodes: p.nodes,
        root,
        offset,
    })
}

struct Place<'a> {
    shown: &'a ErrorShown,
    shapes: &'a [Shape],
    recipes: &'a [Recipe],
    nodes: Vec<Node>,
    placed: HashMap<(u32, TypeRef), u32>,
}

impl Place<'_> {
    fn push(&mut self, node: Node) -> u32 {
        self.nodes.push(node);
        self.nodes.len() as u32 - 1
    }
    fn node(&mut self, shown: &Shown, field: &FieldTy) -> u32 {
        let stride = |p: &Self, ty: &FieldTy| field_size(p.shapes, p.recipes, ty).0;
        match (shown, field) {
            (Shown::Text(text), _) => self.push(Node::Text(text.clone())),
            (Shown::Tuple(xs), _) if xs.is_empty() => self.push(Node::Record {
                name: String::new(),
                parts: vec![],
            }),
            (Shown::Scalar(s), FieldTy::Scalar(t)) if s == t => self.push(Node::Scalar(*s)),
            (Shown::Str, FieldTy::Seq(e)) if **e == FieldTy::Scalar(Scalar::U8) => {
                self.push(Node::Str)
            }
            (Shown::List(e), FieldTy::Seq(ef)) => {
                let element = self.node(e, ef);
                let stride = stride(self, ef);
                self.push(Node::List { element, stride })
            }
            (Shown::Array(e, n), FieldTy::Array(ef, m)) if n == m => {
                let element = self.node(e, ef);
                let stride = stride(self, ef);
                self.push(Node::Array {
                    element,
                    stride,
                    count: *n,
                })
            }
            (Shown::Boxed(e), FieldTy::Box(r)) => {
                let inner = self.node(e, &FieldTy::Inline(*r));
                self.push(Node::Boxed(inner))
            }
            (Shown::Tuple(xs), FieldTy::Inline(r)) => {
                let Some((fields, offsets)) = self.record(*r, xs.len()) else {
                    return self.unknown();
                };
                let parts = xs
                    .iter()
                    .zip(fields.iter().zip(offsets))
                    .map(|(x, (f, offset))| Part {
                        label: None,
                        offset,
                        node: self.node(x, &f.ty),
                    })
                    .collect();
                self.push(Node::Record {
                    name: String::new(),
                    parts,
                })
            }
            (Shown::Def(d), FieldTy::Inline(r)) => self.def(*d, *r),
            _ => self.unknown(),
        }
    }
    fn unknown(&mut self) -> u32 {
        self.push(Node::Text("<value>".into()))
    }
    /// Row `r`'s fields and offsets, when it is a fixed record of `n` fields.
    fn record(&self, r: TypeRef, n: usize) -> Option<(Vec<hirsel_core_layout::Field>, Vec<u64>)> {
        let (Body::Record(fields), Layout::Fixed(fixed)) = (
            &self.shapes[r.index()].body,
            &self.recipes[r.index()].layout,
        ) else {
            return None;
        };
        let offsets = fixed.offsets.first().cloned().unwrap_or_default();
        (fields.len() == n && offsets.len() == n).then(|| (fields.clone(), offsets))
    }
    fn def(&mut self, d: u32, r: TypeRef) -> u32 {
        if let Some(&node) = self.placed.get(&(d, r)) {
            return node;
        }
        // Reserved first: a field reaching this row through a box names it.
        let node = self.unknown();
        self.placed.insert((d, r), node);
        let placed = match &self.shown.defs[d as usize] {
            ShownDef::Record(name, fs) => self.record(r, fs.len()).map(|(fields, offsets)| {
                let parts = fs
                    .iter()
                    .zip(fields.iter().zip(offsets))
                    .map(|((label, x), (f, offset))| Part {
                        label: Some(label.clone()),
                        offset,
                        node: self.node(x, &f.ty),
                    })
                    .collect();
                Node::Record {
                    name: name.clone(),
                    parts,
                }
            }),
            ShownDef::Variant(cs) => self.variant(cs, r),
        };
        if let Some(placed) = placed {
            self.nodes[node as usize] = placed;
        }
        node
    }
    fn variant(&mut self, cs: &[ShownCase], r: TypeRef) -> Option<Node> {
        let (Body::Variant(cases), Layout::Fixed(fixed)) = (
            &self.shapes[r.index()].body,
            &self.recipes[r.index()].layout,
        ) else {
            return None;
        };
        if cases.len() != cs.len() || fixed.tag.is_none() && cs.len() != 1 {
            return None;
        }
        let (cases, fixed) = (cases.clone(), fixed.clone());
        let mut out = Vec::new();
        for (i, (c, case)) in cs.iter().zip(&cases).enumerate() {
            let offsets = fixed.offsets.get(i).cloned().unwrap_or_default();
            if case.fields.len() != c.fields.len() || offsets.len() != c.fields.len() {
                return None;
            }
            let parts = c
                .fields
                .iter()
                .zip(case.fields.iter().zip(offsets))
                .map(|((label, x), (f, offset))| Part {
                    label: c.labelled.then(|| label.clone()),
                    offset,
                    node: self.node(x, &f.ty),
                })
                .collect();
            out.push(Case {
                name: c.name.clone(),
                parens: c.labelled,
                parts,
            });
        }
        Some(Node::Variant {
            tag: fixed.tag,
            cases: out,
        })
    }
}

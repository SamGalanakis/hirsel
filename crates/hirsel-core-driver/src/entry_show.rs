//! PROG-3: the text program for `main`'s error value. Check describes `E` from
//! its source declarations (names, labels, which carrier is a `String`, which
//! capability is a `Secret`); the build places that description on the entry
//! module's layout rows, giving the [`Show`] the run renders.
use hirsel_core_front::{DeclBody, Role, TyId, TyKind, Types};
use hirsel_core_layout::show::{Case, Node, Part, Show};
use hirsel_core_layout::{Body, FieldTy, Layout, RecipeRow, Row, Scalar, TypeRef, field_size};
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
    shapes: &[Row],
    recipes: &[RecipeRow],
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
    shapes: &'a [Row],
    recipes: &'a [RecipeRow],
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

/// Nesting past this depth is written as an unknown value (`<value>`); the
/// decoder refuses deeper input, so an annex cannot exhaust the stack.
const WIRE_DEPTH: usize = 128;

impl ErrorShown {
    /// The description a recipe's entry annex carries (PROG-3 on a recipe build).
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        count(&mut out, self.defs.len());
        for def in &self.defs {
            match def {
                ShownDef::Record(name, fields) => {
                    out.push(0);
                    text(&mut out, name);
                    put_fields(&mut out, fields);
                }
                ShownDef::Variant(cases) => {
                    out.push(1);
                    count(&mut out, cases.len());
                    for case in cases {
                        text(&mut out, &case.name);
                        out.push(u8::from(case.labelled));
                        put_fields(&mut out, &case.fields);
                    }
                }
            }
        }
        put(&mut out, &self.root, 0);
        out
    }

    /// Reads [`Self::encode`]'s bytes; refuses a `Def` naming no definition.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut input = Input(bytes);
        let defs = input.list(|input| {
            Ok(match input.byte()? {
                0 => ShownDef::Record(input.text()?, input.fields()?),
                1 => ShownDef::Variant(input.list(|input| {
                    Ok(ShownCase {
                        name: input.text()?,
                        labelled: input.flag()?,
                        fields: input.fields()?,
                    })
                })?),
                _ => return Err(MALFORMED),
            })
        })?;
        let root = input.shown(0)?;
        let shown = Self { defs, root };
        let mut named = true;
        let mut check = |s: &Shown| {
            named &= !matches!(s, Shown::Def(d) if *d as usize >= shown.defs.len());
        };
        shown.root.visit(&mut check);
        for def in &shown.defs {
            match def {
                ShownDef::Record(_, fields) => fields.iter().for_each(|(_, s)| s.visit(&mut check)),
                ShownDef::Variant(cases) => cases
                    .iter()
                    .flat_map(|c| &c.fields)
                    .for_each(|(_, s)| s.visit(&mut check)),
            }
        }
        (input.0.is_empty() && named)
            .then_some(shown)
            .ok_or(MALFORMED)
    }
}

const MALFORMED: &str = "malformed entry error description";

impl Shown {
    fn visit(&self, f: &mut impl FnMut(&Shown)) {
        f(self);
        match self {
            Shown::List(e) | Shown::Array(e, _) | Shown::Boxed(e) => e.visit(f),
            Shown::Tuple(xs) => xs.iter().for_each(|x| x.visit(f)),
            Shown::Text(_) | Shown::Scalar(_) | Shown::Str | Shown::Def(_) => {}
        }
    }
}

fn count(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u32).to_le_bytes());
}

fn text(out: &mut Vec<u8>, s: &str) {
    count(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

fn put_fields(out: &mut Vec<u8>, fields: &[(String, Shown)]) {
    count(out, fields.len());
    for (label, shown) in fields {
        text(out, label);
        put(out, shown, 0);
    }
}

fn put(out: &mut Vec<u8>, shown: &Shown, depth: usize) {
    if depth == WIRE_DEPTH {
        return put(out, &Shown::Text("<value>".into()), 0);
    }
    match shown {
        Shown::Text(s) => {
            out.push(0);
            text(out, s);
        }
        Shown::Scalar(s) => {
            let code = Scalar::ALL.iter().position(|a| a == s).expect("a scalar");
            out.extend_from_slice(&[1, code as u8]);
        }
        Shown::Str => out.push(2),
        Shown::List(e) => {
            out.push(3);
            put(out, e, depth + 1);
        }
        Shown::Array(e, n) => {
            out.push(4);
            out.extend_from_slice(&n.to_le_bytes());
            put(out, e, depth + 1);
        }
        Shown::Boxed(e) => {
            out.push(5);
            put(out, e, depth + 1);
        }
        Shown::Tuple(xs) => {
            out.push(6);
            count(out, xs.len());
            xs.iter().for_each(|x| put(out, x, depth + 1));
        }
        Shown::Def(d) => {
            out.push(7);
            count(out, *d as usize);
        }
    }
}

struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], &'static str> {
        if self.0.len() < n {
            return Err(MALFORMED);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    fn flag(&mut self) -> Result<bool, &'static str> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MALFORMED),
        }
    }
    fn word(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    fn count(&mut self) -> Result<usize, &'static str> {
        let n = self.word()? as usize;
        // Each element takes at least one byte, so a count past the rest is malformed.
        (n <= self.0.len()).then_some(n).ok_or(MALFORMED)
    }
    fn text(&mut self) -> Result<String, &'static str> {
        let n = self.count()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| MALFORMED)
    }
    fn list<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T, &'static str>,
    ) -> Result<Vec<T>, &'static str> {
        let n = self.count()?;
        (0..n).map(|_| item(self)).collect()
    }
    fn fields(&mut self) -> Result<Vec<(String, Shown)>, &'static str> {
        self.list(|input| Ok((input.text()?, input.shown(0)?)))
    }
    fn shown(&mut self, depth: usize) -> Result<Shown, &'static str> {
        if depth > WIRE_DEPTH {
            return Err(MALFORMED);
        }
        Ok(match self.byte()? {
            0 => Shown::Text(self.text()?),
            1 => Shown::Scalar(*Scalar::ALL.get(self.byte()? as usize).ok_or(MALFORMED)?),
            2 => Shown::Str,
            3 => Shown::List(Box::new(self.shown(depth + 1)?)),
            4 => {
                let n = u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes"));
                Shown::Array(Box::new(self.shown(depth + 1)?), n)
            }
            5 => Shown::Boxed(Box::new(self.shown(depth + 1)?)),
            6 => Shown::Tuple(self.list(|input| input.shown(depth + 1))?),
            7 => Shown::Def(self.word()?),
            _ => return Err(MALFORMED),
        })
    }
}

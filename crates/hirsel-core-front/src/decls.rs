//! Declarations: one published summary per type declaration (OWN-2), computed once in
//! dependency order, and one checked signature per function (LANG-8). No body is read.

use crate::types::*;
use crate::{Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Recipe, Scalar};
use hirsel_core_resolve::{Site, Target};
use hirsel_core_syntax::{NodeId, NodeKind};

/// Bound bits (OWN-4).
pub const PLAIN: u8 = 1;
pub const SHAREABLE: u8 = 2;
pub const SEND: u8 = 4;
pub const KEY: u8 = 8;
pub const ORDERED: u8 = 16;
/// Exactly the integer types, and exactly f32 and f64: written only on type parameters of
/// bodiless core declarations (OWN-4; the resolver refuses them elsewhere).
pub const INTEGER: u8 = 32;
pub const FLOAT: u8 = 64;
/// `packed` or `streamable` (MEM-12): compact encodings, not built. A type argument that
/// needs one is a gap, never a refusal, until stable types are built.
pub const ENCODED: u8 = 128;
pub fn bound_bit(name: &str) -> u8 {
    match name {
        "plain" => PLAIN,
        "shareable" => SHAREABLE,
        "send" => SEND,
        "key" => KEY,
        "integer" => INTEGER,
        "float" => FLOAT,
        "packed" | "streamable" => ENCODED,
        _ => ORDERED,
    }
}
pub fn bound_names(b: u8) -> String {
    let names = [
        "plain",
        "shareable",
        "send",
        "key",
        "ordered",
        "integer",
        "float",
        "packed or streamable",
    ];
    (0..8)
        .filter(|i| b & (1 << i) != 0)
        .map(|i| names[i])
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Holds bits (OWN-2).
pub const H_RESOURCE: u8 = 1;
pub const H_CAPABILITY: u8 = 2;
pub const H_SCOPED: u8 = 4;
pub const H_STABLE: u8 = 8;

/// What a rigid type parameter may hold (OWN-2): every category its bounds do not
/// exclude. `plain` and `shareable` exclude resources, scoped values and stable
/// references, and nothing excludes a capability.
pub fn param_holds(bounds: u8) -> u8 {
    let all = H_RESOURCE | H_CAPABILITY | H_SCOPED | H_STABLE;
    if bounds & (PLAIN | SHAREABLE) != 0 {
        all & !(H_RESOURCE | H_SCOPED | H_STABLE)
    } else {
        all
    }
}

/// A kind (OWN-3). `Generic` is a rigid type parameter without the `plain` bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Plain,
    Own,
    Stable,
    Resource,
    Counted,
    Scoped,
    Generic,
}

/// What a declaration's keyword says (OWN-1).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Repr {
    Own,
    Stable,
    Resource,
    Scoped,
}

/// Whether `path` is a declaration of the core prelude, named one of `names` when
/// `names` is not empty.
pub(crate) fn is_core_prelude(path: &DefPath, names: &[&str]) -> bool {
    path.package_is("core", "1")
        && path.0.modules == ["core"]
        && (names.is_empty() || names.contains(&path.0.name.as_str()))
}

/// A core type whose kind and layout are fixed by its definition path (LANG-7, OWN-3).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Role {
    View,
    Box,
    Rc,
    Arc,
    List,
    String,
    Text,
    Map,
    Region,
    Capability,
    Task,
    Sender,
    Receiver,
}
impl Role {
    fn of(path: &DefPath) -> Option<Role> {
        if !is_core_prelude(path, &[]) {
            return None;
        }
        Some(match path.0.name.as_str() {
            "View" => Role::View,
            "Box" => Role::Box,
            "Rc" => Role::Rc,
            "Arc" => Role::Arc,
            "List" => Role::List,
            "String" => Role::String,
            "Text" => Role::Text,
            "Map" => Role::Map,
            "Region" => Role::Region,
            "Task" => Role::Task,
            "Sender" => Role::Sender,
            "Receiver" => Role::Receiver,
            "Dir" | "Net" | "Clock" | "Random" | "Stdout" | "Stderr" | "Stdin" | "Env"
            | "Process" | "Tasks" => Role::Capability,
            _ => return None,
        })
    }
    pub fn kind(self) -> Kind {
        match self {
            Role::Box | Role::List | Role::String | Role::Map => Kind::Own,
            Role::Rc | Role::Arc => Kind::Counted,
            Role::View | Role::Region => Kind::Scoped,
            Role::Text => Kind::Stable,
            Role::Capability => Kind::Plain,
            Role::Task | Role::Sender | Role::Receiver => Kind::Resource,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Generic {
    pub name: hirsel_core_syntax::Symbol,
    pub bounds: u8,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TyId,
    pub site: Site,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CaseDecl {
    pub name: String,
    pub labelled: bool,
    pub fields: Vec<FieldDecl>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum DeclBody {
    Record(Vec<FieldDecl>),
    Variant(Vec<CaseDecl>),
    Alias(TyId),
}

/// The published summary (OWN-2). For a generic declaration, kind is stated in terms of
/// the parameters: the declaration's own kind joined with its arguments' at `inline_params`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// The kind with every parameter taken as plain.
    pub kind_fixed: Option<Kind>,
    /// Parameters held inline (they decide kind, size and layout).
    pub inline_params: u64,
    pub holds: u8,
    /// Parameters named anywhere in the fields: an argument there adds its holds.
    pub holds_params: u64,
    /// The layout recipe of a declaration with no parameters, when its fields map onto
    /// the layout crate's field types.
    pub recipe: Option<Recipe>,
    /// Part of an inline containment cycle (refused once, OWN-2).
    pub cycle: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TypeDecl {
    pub path: DefPath,
    pub site: Site,
    pub at: ItemRef,
    pub repr: Repr,
    pub public: bool,
    pub opaque: bool,
    pub generics: Vec<Generic>,
    pub declared: u8,
    pub body: DeclBody,
    pub role: Option<Role>,
    /// Core `Option` or `Result`: kind, bounds and copying follow the arguments
    /// (OWN-3, SEM-7), and a scoped argument is allowed (OWN-19).
    pub structural: bool,
    pub finalizer: bool,
    pub summary: Summary,
}
impl TypeDecl {
    /// What the declaration's own type may hold with its parameters rigid (OWN-2): its
    /// fields' categories, and each parameter named in them per [`param_holds`].
    pub fn holds_rigid(&self) -> u8 {
        let mut h = self.summary.holds;
        for (i, g) in self.generics.iter().enumerate() {
            if self.summary.holds_params & (1 << i) != 0 {
                h |= param_holds(g.bounds);
            }
        }
        h
    }
    pub fn fields(&self, case: Option<usize>) -> &[FieldDecl] {
        match (&self.body, case) {
            (DeclBody::Record(f), None) => f,
            (DeclBody::Variant(c), Some(i)) => &c[i].fields,
            _ => &[],
        }
    }
    pub fn all_fields(&self) -> impl Iterator<Item = &FieldDecl> {
        let (r, v): (&[FieldDecl], &[CaseDecl]) = match &self.body {
            DeclBody::Record(f) => (f, &[]),
            DeclBody::Variant(c) => (&[], c),
            DeclBody::Alias(_) => (&[], &[]),
        };
        r.iter().chain(v.iter().flat_map(|c| c.fields.iter()))
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ParamDecl {
    pub name: hirsel_core_syntax::Symbol,
    pub mode: Mode,
    pub ty: TyId,
    pub site: Site,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FnDecl {
    pub path: DefPath,
    pub site: Site,
    pub at: ItemRef,
    pub generics: Vec<Generic>,
    pub params: Vec<ParamDecl>,
    pub result: TyId,
    /// The written result type, where OWN-20 refuses a missing anchor.
    pub result_site: Site,
    /// Parameters the result is anchored to (OWN-20).
    pub anchors: Vec<u32>,
    pub waits: bool,
    pub body: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Def {
    Type(DeclId),
    Fn(u32),
}

/// A check deferred until every summary exists.
pub(crate) enum Pending {
    /// `ty` must have `bounds` (LANG-12, OWN-4 for counted payloads).
    Bound {
        ty: TyId,
        bounds: u8,
        params: Vec<u8>,
        site: Site,
        rule: &'static str,
    },
    /// A type argument of a generic other than Option and Result is not scoped (OWN-19).
    NotScoped { ty: TyId, site: Site },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Visit {
    Todo,
    Open,
    Done,
}

impl Program<'_> {
    /// Pass 1: register every type and function definition with its header names.
    pub(crate) fn register(&mut self) {
        for (m, module) in self.modules.iter().enumerate().skip(self.library_modules) {
            for (i, item) in module.syntax.items().enumerate() {
                let root = item.tree.node(item.root);
                let Some(name) = root.token else { continue };
                let at = ItemRef {
                    module: m as u32,
                    item: i as u32,
                };
                let name_span = item.token_span(name);
                let Some(definition) = self.index.definition(module.id, item.symbol(name)) else {
                    continue;
                };
                let path = self.index.definition_path(definition);
                let site = Site {
                    module: module.id,
                    item: i as u32,
                    span: name_span,
                };
                let mut public = false;
                let mut opaque = false;
                let mut generics = vec![];
                let mut declared = 0;
                let mut finalizer = false;
                for &c in item.tree.children(item.root) {
                    let n = item.tree.node(c);
                    match n.kind {
                        NodeKind::Public => public = true,
                        NodeKind::Opaque => opaque = true,
                        NodeKind::Generics => generics = self.generics(at, c),
                        NodeKind::Bounds => {
                            for &b in item.tree.children(c) {
                                let t = item.tree.node(b).token.unwrap();
                                let bit = bound_bit(item.tree.token_text(item.text, t));
                                if bit == PLAIN {
                                    self.refusals.push(refusal(
                                        "OWN-4",
                                        self.site(at, b),
                                        "`plain` is never declared",
                                    ));
                                }
                                declared |= bit;
                            }
                        }
                        NodeKind::Drop => finalizer = true,
                        NodeKind::Attribute => {
                            let t = n.token.unwrap();
                            if item.tree.token_text(item.text, t) == "compact" {
                                self.gaps.push(Gap::new(
                                    self.site(at, c),
                                    "compact stable layout is not built",
                                ))
                            }
                        }
                        _ => {}
                    }
                }
                let repr = match root.kind {
                    NodeKind::TypeDecl => Repr::Own,
                    NodeKind::StableType => Repr::Stable,
                    NodeKind::ResourceType => Repr::Resource,
                    NodeKind::ScopedType => Repr::Scoped,
                    NodeKind::Function => {
                        let id = self.fns.len() as u32;
                        let body = item
                            .tree
                            .children(item.root)
                            .iter()
                            .copied()
                            .find(|&c| item.tree.node(c).kind == NodeKind::Block);
                        self.fns.push(FnDecl {
                            path: path.clone(),
                            result_site: site,
                            site,
                            at,
                            generics,
                            params: vec![],
                            result: UNIT,
                            anchors: vec![],
                            waits: false,
                            body,
                        });
                        self.by_path.entry(path).or_insert(Def::Fn(id));
                        self.by_definition[definition.0 as usize] = Some(Def::Fn(id));
                        continue;
                    }
                    _ => continue,
                };
                let id = DeclId(self.decls.len() as u32);
                let role = Role::of(&path);
                let structural = is_core_prelude(&path, &["Option", "Result"]);
                self.decls.push(TypeDecl {
                    path: path.clone(),
                    site,
                    at,
                    repr,
                    public,
                    opaque,
                    generics,
                    declared,
                    body: DeclBody::Record(vec![]),
                    role,
                    structural,
                    finalizer,
                    summary: Summary::default(),
                });
                self.by_path.entry(path).or_insert(Def::Type(id));
                self.by_definition[definition.0 as usize] = Some(Def::Type(id));
            }
        }
        self.alias_state = vec![Visit::Todo as u8; self.decls.len()];
        self.alias_state[..self.decls.base_len()].fill(Visit::Done as u8);
    }

    fn generics(&mut self, at: ItemRef, n: NodeId) -> Vec<Generic> {
        let item = self.item(at);
        let mut out = vec![];
        for &g in item.tree.children(n) {
            let gn = item.tree.node(g);
            if gn.kind != NodeKind::Generic {
                self.gaps.push(Gap::new(
                    self.site(at, g),
                    "region parameters are not built",
                ));
                continue;
            }
            let name = item.symbol(gn.token.unwrap());
            let mut bounds = 0;
            for &b in item.tree.children(g) {
                for &bn in item.tree.children(b) {
                    let t = item.tree.node(bn).token.unwrap();
                    bounds |= bound_bit(item.tree.token_text(item.text, t));
                }
            }
            out.push(Generic { name, bounds });
        }
        out
    }

    /// Pass 2: lower every declaration's fields, alias right sides and signatures.
    pub(crate) fn lower_headers(&mut self) {
        for d in self.decls.base_len()..self.decls.len() {
            self.lower_decl(DeclId(d as u32));
        }
        for f in self.fns.base_len()..self.fns.len() {
            self.lower_signature(f);
        }
    }

    fn lower_decl(&mut self, d: DeclId) {
        if self.alias_state[d.index()] != Visit::Todo as u8 {
            return;
        }
        self.alias_state[d.index()] = Visit::Open as u8;
        self.alias_stack.push((d, None));
        let at = self.decls[d.index()].at;
        let item = self.item(at);
        let generics = self.decls[d.index()].generics.clone();
        let mut body = DeclBody::Record(vec![]);
        for &c in item.tree.children(item.root) {
            let n = item.tree.node(c);
            match n.kind {
                NodeKind::Fields => {
                    let mut fields = vec![];
                    for &f in item.tree.children(c) {
                        let fnode = item.tree.node(f);
                        let name = item
                            .tree
                            .token_text(item.text, fnode.token.unwrap())
                            .to_owned();
                        let tnode = item.tree.children(f)[0];
                        let ty = self.lower_ty(at, tnode, &generics);
                        fields.push(FieldDecl {
                            name,
                            ty,
                            site: self.site(at, f),
                        });
                    }
                    body = DeclBody::Record(fields);
                }
                NodeKind::Variants => {
                    let mut cases = vec![];
                    for &v in item.tree.children(c) {
                        let vnode = item.tree.node(v);
                        let name = item
                            .tree
                            .token_text(item.text, vnode.token.unwrap())
                            .to_owned();
                        let mut fields = vec![];
                        let mut labelled = false;
                        for (k, &p) in item.tree.children(v).iter().enumerate() {
                            let pnode = item.tree.node(p);
                            let label = pnode
                                .token
                                .map(|t| item.tree.token_text(item.text, t).to_owned());
                            labelled = label.is_some();
                            let tnode = item.tree.children(p)[0];
                            let ty = self.lower_ty(at, tnode, &generics);
                            fields.push(FieldDecl {
                                name: label.unwrap_or_else(|| k.to_string()),
                                ty,
                                site: self.site(at, tnode),
                            });
                        }
                        cases.push(CaseDecl {
                            name,
                            labelled,
                            fields,
                        });
                    }
                    body = DeclBody::Variant(cases);
                }
                NodeKind::TypePath
                | NodeKind::TypeArguments
                | NodeKind::UnitType
                | NodeKind::GroupType
                | NodeKind::TupleType
                | NodeKind::CallableType
                | NodeKind::OwnCallableType
                | NodeKind::ScopedCallableType => {
                    let ty = self.lower_ty(at, c, &generics);
                    body = DeclBody::Alias(ty);
                }
                _ => {}
            }
        }
        self.decls[d.index()].body = body;
        self.alias_state[d.index()] = Visit::Done as u8;
        self.alias_stack.pop();
    }

    /// Lower a written type. `generics` are the enclosing definition's parameters.
    pub(crate) fn lower_ty(&mut self, at: ItemRef, n: NodeId, generics: &[Generic]) -> TyId {
        let item = self.item(at);
        let node = item.tree.node(n);
        let children = item.tree.children(n).to_vec();
        match node.kind {
            NodeKind::UnitType => UNIT,
            NodeKind::GroupType => self.lower_ty(at, children[0], generics),
            NodeKind::TupleType => {
                let xs = children
                    .iter()
                    .map(|&c| self.lower_ty(at, c, generics))
                    .collect();
                self.types.intern(TyKind::Tuple(xs))
            }
            NodeKind::TypePath => self.named_ty(at, n, &[], generics),
            NodeKind::TypeArguments => {
                let mut args = vec![];
                for &c in &children[1..] {
                    if item.tree.node(c).kind == NodeKind::Anchor {
                        // Not built, so the type is not built: no argument count to check.
                        self.gaps
                            .push(Gap::new(self.site(at, c), "anchor arguments are not built"));
                        return ERROR;
                    }
                    let t = self.lower_ty(at, c, generics);
                    args.push((t, self.site(at, c)));
                }
                self.named_ty(at, children[0], &args, generics)
            }
            NodeKind::Anchor => self.lower_ty(at, children[0], generics),
            NodeKind::CallableType => {
                let mut params = vec![];
                let mut result = UNIT;
                let mut waits = false;
                for &c in &children {
                    match item.tree.node(c).kind {
                        NodeKind::Parameters => {
                            for &p in item.tree.children(c) {
                                let mode = mode_of(item.tree.node(p).kind);
                                let t = self.lower_ty(at, item.tree.children(p)[0], generics);
                                params.push((mode, t));
                            }
                        }
                        NodeKind::ResultType => {
                            result = self.lower_ty(at, item.tree.children(c)[0], generics)
                        }
                        NodeKind::Waits => waits = true,
                        NodeKind::Generics => self.gaps.push(Gap::new(
                            self.site(at, c),
                            "generic callable types are not built",
                        )),
                        _ => {}
                    }
                }
                self.types.intern(TyKind::Fn(params, result, waits))
            }
            NodeKind::OwnCallableType | NodeKind::ScopedCallableType => {
                self.gaps.push(Gap::new(
                    self.site(at, n),
                    "owning and scoped callable types are not built",
                ));
                ERROR
            }
            _ => {
                self.gaps
                    .push(Gap::new(self.site(at, n), "type form is not built"));
                ERROR
            }
        }
    }

    fn named_ty(
        &mut self,
        at: ItemRef,
        path: NodeId,
        args: &[(TyId, Site)],
        generics: &[Generic],
    ) -> TyId {
        let site = self.site(at, path);
        let target = self.target(at, path).copied();
        let decl = match target {
            Some(Target::Builtin(name)) => {
                let Some(s) = Scalar::from_name(self.index.symbols.text(name)) else {
                    // The one builtin that is not a scalar: `Inline[T, n]`.
                    self.gaps
                        .push(Gap::new(site, "inline array types are not built"));
                    return ERROR;
                };
                return self.no_args(scalar(s), args, &site);
            }
            Some(Target::Definition(p)) if self.core_named(p, &["Never"]) => {
                return self.no_args(NEVER, args, &site);
            }
            Some(Target::Binder(b)) => {
                let name = &self.items[at.module as usize][at.item as usize]
                    .resolution
                    .binders[b as usize]
                    .name;
                return match generics.iter().position(|g| &g.name == name) {
                    Some(i) => {
                        let t = self.types.intern(TyKind::Param(i as u32));
                        self.no_args(t, args, &site)
                    }
                    None => {
                        self.gaps
                            .push(Gap::new(site, "this type parameter is not built"));
                        ERROR
                    }
                };
            }
            Some(Target::Definition(p)) => match self.by_definition[p.0 as usize].as_ref() {
                Some(Def::Type(d)) => *d,
                _ => {
                    self.refusals.push(refusal(
                        "LANG-7",
                        site,
                        format!("`{}` is not a type", self.index.definition_path(p)),
                    ));
                    return ERROR;
                }
            },
            // The resolver already refused a missing name.
            _ => return ERROR,
        };
        let generic_count = self.decls[decl.index()].generics.len();
        if args.len() != generic_count {
            self.refusals.push(refusal(
                "LANG-12",
                site,
                format!(
                    "`{}` takes {generic_count} type arguments, {} written",
                    self.decls[decl.index()].path.0.name,
                    args.len()
                ),
            ));
            return ERROR;
        }
        let arg_tys: Vec<TyId> = args.iter().map(|a| a.0).collect();
        let params: Vec<u8> = generics.iter().map(|g| g.bounds).collect();
        let d = &self.decls[decl.index()];
        let role = d.role;
        if !d.structural {
            for (i, (t, s)) in args.iter().enumerate() {
                self.pending.push(Pending::NotScoped { ty: *t, site: *s });
                let need = d.generics[i].bounds;
                if need != 0 {
                    let counted = matches!(role, Some(Role::Rc | Role::Arc));
                    self.pending.push(Pending::Bound {
                        ty: *t,
                        bounds: need,
                        params: params.clone(),
                        site: *s,
                        rule: if counted { "OWN-4" } else { "LANG-12" },
                    });
                }
            }
        }
        if role == Some(Role::View) {
            return self.types.intern(TyKind::View(arg_tys[0]));
        }
        let alias = self.is_alias(decl);
        if alias && let Some(top) = self.alias_stack.last_mut() {
            top.1 = Some(site);
        }
        if alias && self.alias_state[decl.index()] == Visit::Open as u8 {
            self.alias_cycle(decl);
            return ERROR;
        }
        if alias {
            self.lower_decl(decl);
            if let DeclBody::Alias(t) = self.decls[decl.index()].body {
                return subst(&mut self.types, t, &arg_tys);
            }
            return ERROR;
        }
        self.types.intern(TyKind::Adt(decl, arg_tys))
    }

    /// Whether a declaration is an alias, read from its syntax (LANG-9).
    fn is_alias(&self, d: DeclId) -> bool {
        if self.alias_state[d.index()] == Visit::Done as u8 {
            return matches!(self.decls[d.index()].body, DeclBody::Alias(_));
        }
        let item = self.item(self.decls[d.index()].at);
        item.tree.children(item.root).iter().any(|&c| {
            matches!(
                item.tree.node(c).kind,
                NodeKind::TypePath
                    | NodeKind::TypeArguments
                    | NodeKind::UnitType
                    | NodeKind::GroupType
                    | NodeKind::TupleType
                    | NodeKind::CallableType
                    | NodeKind::OwnCallableType
                    | NodeKind::ScopedCallableType
            )
        })
    }

    fn no_args(&mut self, t: TyId, args: &[(TyId, Site)], site: &Site) -> TyId {
        if !args.is_empty() {
            self.refusals
                .push(refusal("LANG-12", *site, "this type takes no arguments"));
            return ERROR;
        }
        t
    }

    fn lower_signature(&mut self, f: usize) {
        let at = self.fns[f].at;
        let item = self.item(at);
        let generics = self.fns[f].generics.clone();
        let mut params = vec![];
        let mut result = UNIT;
        let mut anchor_nodes = vec![];
        let mut waits = false;
        for &c in item.tree.children(item.root) {
            match item.tree.node(c).kind {
                NodeKind::Parameters => {
                    for &p in item.tree.children(c) {
                        let pn = item.tree.node(p);
                        if pn.kind == NodeKind::BytesParameter {
                            self.gaps
                                .push(Gap::new(self.site(at, p), "sized parameters are not built"));
                        }
                        let name = item.symbol(pn.token.unwrap());
                        let tnode = item.tree.children(p)[0];
                        let ty = self.lower_ty(at, tnode, &generics);
                        params.push(ParamDecl {
                            name,
                            mode: mode_of(pn.kind),
                            ty,
                            site: self.site(at, p),
                        });
                    }
                }
                NodeKind::ResultType => {
                    let t = item.tree.children(c)[0];
                    if item.tree.node(t).kind == NodeKind::Anchor {
                        anchor_nodes.push(item.tree.children(t)[1]);
                    }
                    result = self.lower_ty(at, t, &generics);
                    self.fns[f].result_site = self.site(at, t);
                }
                NodeKind::Waits => waits = true,
                NodeKind::Outlives => self
                    .gaps
                    .push(Gap::new(self.site(at, c), "outlives clauses are not built")),
                _ => {}
            }
        }
        let mut anchors = vec![];
        for a in anchor_nodes {
            let parts = path_text(item, a);
            match params
                .iter()
                .position(|p| self.index.symbols.text(p.name) == parts)
            {
                Some(i) if params[i].mode == Mode::View => anchors.push(i as u32),
                _ => self.refusals.push(refusal(
                    "OWN-20",
                    self.site(at, a),
                    format!("anchor `{parts}` is not a view parameter"),
                )),
            }
        }
        let fd = &mut self.fns[f];
        fd.params = params;
        fd.result = result;
        fd.anchors = anchors;
        fd.waits = waits;
    }

    /// Pass 3: summaries in dependency order, then the deferred checks.
    pub(crate) fn summarize(&mut self) {
        let n = self.decls.len();
        let mut state = vec![Visit::Todo; n];
        state[..self.decls.base_len()].fill(Visit::Done);
        let mut stack: Vec<(DeclId, usize)> = vec![];
        let order: Vec<usize> = (0..n).collect();
        for d in order {
            if state[d] == Visit::Todo {
                self.inline_visit(DeclId(d as u32), &mut state, &mut stack);
            }
        }
        self.holds();
        self.check_declared_bounds();
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            match p {
                Pending::Bound {
                    ty,
                    bounds,
                    params,
                    site,
                    rule,
                } => {
                    let have = has_bounds(&self.decls, &self.types, ty, &params);
                    if bounds & ENCODED != 0 {
                        self.gaps
                            .push(Gap::new(site, "packed and streamable bounds are not built"));
                    } else if have & bounds != bounds {
                        self.refusals.push(refusal(
                            rule,
                            site,
                            format!("type argument is not {}", bound_names(bounds & !have)),
                        ));
                    }
                }
                Pending::NotScoped { ty, site } => {
                    if kind(&self.decls, &self.types, ty, &[]) == Kind::Scoped {
                        self.refusals.push(refusal(
                            "OWN-19",
                            site,
                            "a scoped type is not a type argument of a generic other than Option and Result",
                        ));
                    }
                }
            }
        }
        self.check_signatures();
        crate::layout::recipes(self);
    }

    /// Kind and inline parameters, reading only fields held inline (OWN-2). Re-entering
    /// an open declaration is an inline containment cycle.
    fn inline_visit(&mut self, d: DeclId, state: &mut [Visit], stack: &mut Vec<(DeclId, usize)>) {
        state[d.index()] = Visit::Open;
        let decl = &self.decls[d.index()];
        let mut kind_fixed = match decl.role {
            Some(r) => Some(r.kind()),
            None => match decl.repr {
                Repr::Own => Some(Kind::Plain),
                Repr::Stable => Some(Kind::Stable),
                Repr::Resource => Some(Kind::Resource),
                Repr::Scoped => Some(Kind::Scoped),
            },
        };
        let mut inline = 0u64;
        let fields: Vec<TyId> = if decl.role.is_some() {
            vec![]
        } else {
            decl.all_fields().map(|f| f.ty).collect()
        };
        for (k, ty) in fields.into_iter().enumerate() {
            stack.push((d, k));
            let (fk, fp) = self.inline_ty(ty, state, stack);
            stack.pop();
            inline |= fp;
            if self.decls[d.index()].repr == Repr::Own && self.decls[d.index()].role.is_none() {
                kind_fixed = Some(join(kind_fixed.unwrap(), fk));
            }
        }
        if let Some(Kind::Scoped) = kind_fixed
            && self.decls[d.index()].repr == Repr::Own
        {
            // A scoped field of an own type is refused by OWN-19 below; the type stays own.
            kind_fixed = Some(Kind::Own);
        }
        let s = &mut self.decls[d.index()].summary;
        s.kind_fixed = kind_fixed;
        s.inline_params = inline;
        state[d.index()] = Visit::Done;
    }

    /// The kind (parameters as plain) of an inline field type and the parameters it holds inline.
    fn inline_ty(
        &mut self,
        ty: TyId,
        state: &mut [Visit],
        stack: &mut Vec<(DeclId, usize)>,
    ) -> (Kind, u64) {
        match self.types.kind(ty).clone() {
            TyKind::Param(i) => (Kind::Plain, 1 << i),
            TyKind::Tuple(xs) => {
                let mut k = Kind::Plain;
                let mut p = 0;
                for x in xs {
                    let (xk, xp) = self.inline_ty(x, state, stack);
                    k = join(k, xk);
                    p |= xp;
                }
                (k, p)
            }
            TyKind::View(_) => (Kind::Scoped, 0),
            TyKind::Adt(e, args) => {
                if let Some(r) = self.decls[e.index()].role {
                    // An indirection: fixed size, its payload is not read (OWN-2).
                    return (r.kind(), 0);
                }
                match state[e.index()] {
                    Visit::Open => {
                        self.cycle(e, stack);
                        return (Kind::Own, 0);
                    }
                    Visit::Todo => self.inline_visit(e, state, stack),
                    Visit::Done => {}
                }
                let s = &self.decls[e.index()].summary;
                let mut k = s.kind_fixed.unwrap_or(Kind::Own);
                let ip = s.inline_params;
                let mut p = 0;
                for (i, a) in args.into_iter().enumerate() {
                    if ip & (1 << i) != 0 {
                        let (ak, ap) = self.inline_ty(a, state, stack);
                        if self.decls[e.index()].repr == Repr::Own {
                            k = join(k, ak);
                        }
                        p |= ap;
                    }
                }
                (k, p)
            }
            _ => (Kind::Plain, 0),
        }
    }

    fn cycle(&mut self, entered: DeclId, stack: &[(DeclId, usize)]) {
        let start = stack.iter().position(|&(d, _)| d == entered).unwrap();
        let members = &stack[start..];
        if members
            .iter()
            .any(|&(d, _)| self.decls[d.index()].summary.cycle)
        {
            return;
        }
        let &(least, field) = members
            .iter()
            .min_by(|a, b| self.identity(a.0).cmp(&self.identity(b.0)))
            .unwrap();
        for &(d, _) in members {
            self.decls[d.index()].summary.cycle = true;
        }
        let decl = &self.decls[least.index()];
        let f = decl.all_fields().nth(field).unwrap();
        let mut r = refusal(
            "OWN-2",
            f.site,
            format!(
                "`{}` contains itself inline through field `{}`",
                decl.path.0.name, f.name
            ),
        );
        r.causal = members
            .iter()
            .filter(|m| m.0 != least)
            .map(|m| self.decls[m.0.index()].site)
            .collect();
        self.refusals.push(r);
    }

    /// An alias chain reaching itself (LANG-9), refused once, like OWN-2's cycles, at
    /// the continuing reference of the member whose identity is least.
    fn alias_cycle(&mut self, entry: DeclId) {
        let from = self
            .alias_stack
            .iter()
            .position(|m| m.0 == entry)
            .expect("an open alias is on the stack");
        let members = self.alias_stack[from..].to_vec();
        let least = members
            .iter()
            .min_by_key(|m| self.identity(m.0))
            .expect("a cycle has a member");
        let mut r = refusal(
            "LANG-9",
            least.1.expect("every member continues the chain"),
            "a chain of aliases reaches itself",
        );
        r.causal = members
            .iter()
            .filter(|m| m.0 != least.0)
            .map(|m| self.decls[m.0.index()].site)
            .collect();
        self.refusals.push(r);
    }

    /// Definition identity: package, module path, byte offset (OWN-2).
    fn identity(&self, d: DeclId) -> (String, String, Vec<String>, u32) {
        let decl = &self.decls[d.index()];
        let offset = self.item(decl.at).file_offset + decl.site.span.start;
        let p = &decl.path.0;
        (
            p.package.name.clone(),
            p.package.version.clone(),
            p.modules.clone(),
            offset,
        )
    }

    /// Holds sets: types that name each other through any field form a group, and each
    /// member holds the group's union (OWN-2). One Tarjan pass, no iteration.
    fn holds(&mut self) {
        let n = self.decls.len();
        let mut direct = vec![0u8; n];
        let mut refs: Vec<Vec<usize>> = vec![vec![]; n];
        for d in self.decls.base_len()..n {
            let decl = &self.decls[d];
            let mut params = 0u64;
            let mut h = match decl.role {
                Some(Role::Capability) => H_CAPABILITY,
                Some(Role::Task | Role::Sender | Role::Receiver) => H_RESOURCE,
                Some(Role::View | Role::Region) => H_SCOPED,
                Some(Role::Text) => H_STABLE,
                Some(_) => 0,
                None => match decl.repr {
                    Repr::Resource => H_RESOURCE,
                    Repr::Scoped => H_SCOPED,
                    Repr::Stable => H_STABLE,
                    Repr::Own => 0,
                },
            };
            if decl.role.is_none() {
                for f in decl.all_fields() {
                    walk_holds(&self.types, f.ty, &mut h, &mut params, &mut refs[d]);
                }
            }
            direct[d] = h;
            self.decls[d].summary.holds_params = params;
        }
        for d in 0..self.decls.base_len() {
            direct[d] = self.decls[d].summary.holds;
        }
        let mut t = Tarjan {
            index: vec![u32::MAX; n],
            low: vec![0; n],
            on: vec![false; n],
            stack: vec![],
            next: 0,
            refs: &refs,
            direct: &direct,
            out: vec![0; n],
        };
        for d in 0..n {
            if t.index[d] == u32::MAX {
                t.visit(d);
            }
        }
        for d in self.decls.base_len()..n {
            self.decls[d].summary.holds = t.out[d];
        }
    }

    /// Declared bounds are checked once against the fields, reading each field type's
    /// declared bounds (OWN-4). Scoped fields of non-scoped types are OWN-19.
    fn check_declared_bounds(&mut self) {
        for d in self.decls.base_len()..self.decls.len() {
            let decl = &self.decls[d];
            if decl.role.is_some() {
                continue;
            }
            let params: Vec<u8> = decl.generics.iter().map(|g| g.bounds).collect();
            let declared = decl.declared & !PLAIN;
            let mut out = vec![];
            if declared & SHAREABLE != 0 && decl.repr != Repr::Own {
                out.push(refusal(
                    "OWN-4",
                    decl.site,
                    "resources, scoped and stable types are never shareable",
                ));
            }
            for f in decl.all_fields() {
                if decl.repr != Repr::Scoped
                    && kind(&self.decls, &self.types, f.ty, &params) == Kind::Scoped
                {
                    out.push(refusal(
                        "OWN-19",
                        f.site,
                        format!("field `{}` of a non-scoped type is scoped", f.name),
                    ));
                }
                let have = has_bounds(&self.decls, &self.types, f.ty, &params);
                let missing = declared & !have;
                if missing != 0 {
                    out.push(refusal(
                        "OWN-4",
                        f.site,
                        format!("field `{}` is not {}", f.name, bound_names(missing)),
                    ));
                }
            }
            self.refusals.extend(out);
        }
    }

    fn check_signatures(&mut self) {
        for f in self.fns.base_len()..self.fns.len() {
            let fd = &self.fns[f];
            let params: Vec<u8> = fd.generics.iter().map(|g| g.bounds).collect();
            if kind(&self.decls, &self.types, fd.result, &params) == Kind::Scoped
                && fd.anchors.is_empty()
                && !matches!(self.types.kind(fd.result), TyKind::Error)
            {
                self.refusals.push(refusal(
                    "OWN-20",
                    fd.result_site,
                    "a scoped result names its anchor",
                ));
            }
        }
    }
}

struct Tarjan<'a> {
    index: Vec<u32>,
    low: Vec<u32>,
    on: Vec<bool>,
    stack: Vec<usize>,
    next: u32,
    refs: &'a [Vec<usize>],
    direct: &'a [u8],
    out: Vec<u8>,
}
impl Tarjan<'_> {
    fn visit(&mut self, v: usize) {
        self.index[v] = self.next;
        self.low[v] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on[v] = true;
        for &w in &self.refs[v] {
            if self.index[w] == u32::MAX {
                self.visit(w);
                self.low[v] = self.low[v].min(self.low[w]);
            } else if self.on[w] {
                self.low[v] = self.low[v].min(self.index[w]);
            }
        }
        if self.low[v] == self.index[v] {
            let at = self.stack.iter().rposition(|&x| x == v).unwrap();
            let group: Vec<usize> = self.stack.drain(at..).collect();
            let mut h = 0;
            for &m in &group {
                self.on[m] = false;
                h |= self.direct[m];
                for &w in &self.refs[m] {
                    if !group.contains(&w) {
                        h |= self.out[w];
                    }
                }
            }
            for &m in &group {
                self.out[m] = h;
            }
        }
    }
}

fn walk_holds(t: &Types, ty: TyId, h: &mut u8, params: &mut u64, refs: &mut Vec<usize>) {
    match t.kind(ty) {
        TyKind::Param(i) => *params |= 1 << i,
        TyKind::Tuple(xs) => {
            for &x in xs {
                walk_holds(t, x, h, params, refs);
            }
        }
        TyKind::View(x) => {
            *h |= H_SCOPED;
            walk_holds(t, *x, h, params, refs);
        }
        TyKind::Adt(e, xs) => {
            refs.push(e.index());
            for &x in xs {
                walk_holds(t, x, h, params, refs);
            }
        }
        _ => {}
    }
}

fn join(a: Kind, b: Kind) -> Kind {
    use Kind::*;
    match (a, b) {
        (Scoped, _) | (_, Scoped) => Scoped,
        (Plain, Plain) => Plain,
        (Plain, Generic) | (Generic, Plain) | (Generic, Generic) => Generic,
        _ => Own,
    }
}

/// The kind of a type (OWN-3). `params` are the bounds of the enclosing definition's
/// type parameters.
pub fn kind(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u8]) -> Kind {
    match t.kind(ty) {
        TyKind::Error
        | TyKind::Hole
        | TyKind::Never
        | TyKind::Unit
        | TyKind::Scalar(_)
        | TyKind::Fn(..) => Kind::Plain,
        TyKind::Tuple(xs) => xs
            .iter()
            .fold(Kind::Plain, |k, &x| join(k, kind(decls, t, x, params))),
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
                Repr::Stable => Kind::Stable,
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
                            k = join(k, kind(decls, t, a, params));
                        }
                    }
                    k
                }
            }
        }
    }
}

/// Whether `let` copies a value of this type (OWN-3).
pub fn copyable(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u8]) -> bool {
    match kind(decls, t, ty, params) {
        Kind::Plain | Kind::Stable => true,
        Kind::Scoped => match t.kind(ty) {
            TyKind::View(_) => true,
            TyKind::Tuple(xs) => xs.iter().all(|&x| copyable(decls, t, x, params)),
            TyKind::Adt(d, args) => {
                let decl = &decls[d.index()];
                if decl.role.is_some() {
                    return decl.role == Some(Role::Region) || decl.role == Some(Role::View);
                }
                if decl.structural {
                    return args.iter().all(|&x| copyable(decls, t, x, params));
                }
                // A scoped declaration copies when every field does; its fields were
                // lowered against its own parameters.
                decl.all_fields().all(|f| {
                    !matches!(
                        kind(decls, t, f.ty, &[]),
                        Kind::Own | Kind::Resource | Kind::Counted | Kind::Generic
                    )
                })
            }
            _ => false,
        },
        _ => false,
    }
}

/// The bounds a type has (OWN-4, SEM-7).
pub fn has_bounds(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u8]) -> u8 {
    let all = PLAIN | SHAREABLE | SEND | KEY | ORDERED;
    let plain_extra = if kind(decls, t, ty, params) == Kind::Plain {
        PLAIN | SHAREABLE | SEND
    } else {
        0
    };
    let meet = |xs: &[TyId]| {
        xs.iter()
            .fold(all, |b, &x| b & has_bounds(decls, t, x, params))
    };
    plain_extra
        | match t.kind(ty) {
            TyKind::Error | TyKind::Hole | TyKind::Never => all | INTEGER | FLOAT | ENCODED,
            TyKind::Unit => all,
            TyKind::Scalar(Scalar::F32 | Scalar::F64) => PLAIN | SHAREABLE | SEND | FLOAT,
            TyKind::Scalar(s) if hirsel_core_ir::Ty::Scalar(*s).int().is_some() => all | INTEGER,
            TyKind::Scalar(_) => all,
            TyKind::Fn(..) => PLAIN | SHAREABLE | SEND,
            TyKind::Tuple(xs) => meet(xs),
            TyKind::View(x) => has_bounds(decls, t, *x, params) & (KEY | ORDERED),
            TyKind::Param(i) => params.get(*i as usize).copied().unwrap_or(0),
            TyKind::Adt(d, args) => {
                let decl = &decls[d.index()];
                match decl.role {
                    Some(Role::List) => meet(args) & (KEY | ORDERED | SHAREABLE | SEND),
                    Some(Role::String) => KEY | ORDERED | SHAREABLE | SEND,
                    Some(Role::Text) => KEY | ORDERED,
                    Some(Role::Rc) => SHAREABLE | (meet(args) & (KEY | ORDERED)),
                    Some(Role::Arc) => SHAREABLE | SEND | (meet(args) & (KEY | ORDERED)),
                    Some(Role::Box) => meet(args) & (SHAREABLE | SEND | KEY | ORDERED),
                    Some(Role::Sender | Role::Receiver) => SEND,
                    Some(Role::Map) => meet(args) & (SHAREABLE | SEND),
                    Some(Role::Capability) => PLAIN | SHAREABLE | SEND,
                    Some(_) => 0,
                    None if decl.structural => meet(args),
                    None => decl.declared & !PLAIN,
                }
            }
        }
}

pub(crate) fn mode_of(k: NodeKind) -> Mode {
    match k {
        NodeKind::MutParameter => Mode::Mut,
        NodeKind::TakeParameter => Mode::Take,
        _ => Mode::View,
    }
}

pub(crate) fn path_text(item: hirsel_core_syntax::Item<'_>, n: NodeId) -> String {
    let r = &item.tree.node(n).origin.tokens;
    item.tree.tokens[r.start as usize..r.end as usize]
        .iter()
        .map(|t| &item.text[t.span.start as usize..t.span.end as usize])
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The canonical text of a type: definition paths and scalar names, as instance keys and
/// `Module::types` spell them (DECISIONS-883 decision 9).
pub fn canonical(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId) -> String {
    let mut s = String::new();
    write_canonical(decls, t, ty, &mut s);
    s
}
fn write_canonical(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, s: &mut String) {
    use std::fmt::Write;
    let list = |xs: &[TyId], s: &mut String| {
        for (i, &x) in xs.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            write_canonical(decls, t, x, s);
        }
    };
    match t.kind(ty) {
        TyKind::Error => s.push_str("{error}"),
        TyKind::Hole => s.push('_'),
        TyKind::Never => s.push_str("Never"),
        TyKind::Unit => s.push_str("()"),
        TyKind::Scalar(x) => s.push_str(x.name()),
        TyKind::Tuple(xs) => {
            s.push('(');
            list(xs, s);
            s.push(')');
        }
        TyKind::Adt(d, xs) => {
            let _ = write!(s, "{}", decls[d.index()].path);
            if !xs.is_empty() {
                s.push('[');
                list(xs, s);
                s.push(']');
            }
        }
        TyKind::View(x) => {
            s.push_str("View[");
            write_canonical(decls, t, *x, s);
            s.push(']');
        }
        TyKind::Param(i) => {
            let _ = write!(s, "${i}");
        }
        TyKind::Fn(ps, r, w) => {
            s.push_str("fn(");
            for (i, (m, p)) in ps.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(match m {
                    Mode::View => "",
                    Mode::Mut => "mut ",
                    Mode::Take => "take ",
                });
                write_canonical(decls, t, *p, s);
            }
            s.push_str(") -> ");
            write_canonical(decls, t, *r, s);
            if *w {
                s.push_str(" waits");
            }
        }
    }
}

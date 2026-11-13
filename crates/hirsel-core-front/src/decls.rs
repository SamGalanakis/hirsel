//! Declarations: one published summary per type declaration (OWN-2), computed once in
//! dependency order, and one checked signature per function (LANG-8). No body is read.

use crate::types::*;
use crate::{Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Recipe, Scalar, Width};
use hirsel_core_resolve::{Site, Target};
use hirsel_core_syntax::{NodeId, NodeKind};

/// Bound bits (OWN-4).
pub const PLAIN: u16 = 1;
pub const SHAREABLE: u16 = 2;
pub const SEND: u16 = 4;
pub const KEY: u16 = 8;
pub const ORDERED: u16 = 16;
/// Exactly the integer types, and exactly f32 and f64: written only on type parameters of
/// core declarations (OWN-4; the resolver refuses them elsewhere).
pub const INTEGER: u16 = 32;
pub const FLOAT: u16 = 64;
/// Independently computed encoding bounds (MEM-12).
pub const PACKED: u16 = 128;
pub const STREAMABLE: u16 = 256;
pub const ENCODED: u16 = PACKED | STREAMABLE;
const FIXED_ENCODING: u16 = 512;
/// A declared bound set closed under implication (OWN-4): `integer` and `float` imply
/// every bound all their admitted types have, and `streamable` implies `packed`.
pub fn implied(mut b: u16) -> u16 {
    if b & INTEGER != 0 {
        b |= PLAIN | SHAREABLE | SEND | KEY | ORDERED | ENCODED;
    }
    if b & FLOAT != 0 {
        b |= PLAIN | SHAREABLE | SEND | ENCODED;
    }
    if b & STREAMABLE != 0 {
        b |= PACKED;
    }
    b
}
pub fn bound_bit(name: &str) -> u16 {
    match name {
        "plain" => PLAIN,
        "shareable" => SHAREABLE,
        "send" => SEND,
        "key" => KEY,
        "integer" => INTEGER,
        "float" => FLOAT,
        "packed" => PACKED,
        "streamable" => STREAMABLE,
        _ => ORDERED,
    }
}
pub fn bound_names(b: u16) -> String {
    let names = [
        "plain",
        "shareable",
        "send",
        "key",
        "ordered",
        "integer",
        "float",
        "packed",
        "streamable",
    ];
    (0..9)
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
/// An owner without a copy contract (OWN-2).
pub const H_OPAQUE: u8 = 16;

/// What a rigid type parameter may hold (OWN-2): every category its bounds do not
/// exclude. `plain` and `shareable` exclude resources, scoped values and stable
/// references, and nothing excludes a capability.
pub fn param_holds(bounds: u16) -> u8 {
    let all = H_RESOURCE | H_CAPABILITY | H_SCOPED | H_STABLE | H_OPAQUE;
    if bounds & (PLAIN | SHAREABLE) != 0 {
        all & !(H_RESOURCE | H_SCOPED | H_STABLE | H_OPAQUE)
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
    HostFile,
    FixedSpan,
    SeqSpan,
    IndexedSpan,
    SeqIndex,
    Link,
    Dest,
    Writer,
    Child,
    SpanBuilder,
    Builder,
    Args,
    Unique,
    Site,
    Causes,
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
            "HostFile" => Role::HostFile,
            "FixedSpan" => Role::FixedSpan,
            "SeqSpan" => Role::SeqSpan,
            "IndexedSpan" => Role::IndexedSpan,
            "SeqIndex" => Role::SeqIndex,
            "Link" => Role::Link,
            "Dest" => Role::Dest,
            "Writer" => Role::Writer,
            "Child" => Role::Child,
            "SpanBuilder" => Role::SpanBuilder,
            "Builder" => Role::Builder,
            "Args" => Role::Args,
            "Unique" => Role::Unique,
            "Site" => Role::Site,
            "Causes" => Role::Causes,

            "Dir" | "Net" | "Clock" | "Random" | "Stdout" | "Stderr" | "Stdin" | "Env"
            | "Process" | "Tasks" => Role::Capability,
            _ => return None,
        })
    }
    pub fn kind(self) -> Kind {
        match self {
            Role::Box
            | Role::List
            | Role::String
            | Role::Map
            | Role::SpanBuilder
            | Role::Builder
            | Role::Causes => Kind::Own,
            Role::Rc | Role::Arc => Kind::Counted,
            Role::View | Role::Region | Role::Dest | Role::Writer => Kind::Scoped,
            Role::Text | Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan | Role::SeqIndex => {
                Kind::Stable
            }
            Role::Capability
            | Role::Args
            | Role::Unique
            | Role::Site
            | Role::Child
            | Role::Link => Kind::Plain,
            Role::Task | Role::Sender | Role::Receiver | Role::HostFile => Kind::Resource,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Generic {
    pub name: hirsel_core_syntax::Symbol,
    pub bounds: u16,
    pub region: bool,
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
    /// Argument positions that condition declared bounds (OWN-4). Phantom parameters
    /// condition nothing, including when hidden behind another phantom type.
    pub bound_params: Vec<u32>,
    /// Encoding properties with rigid parameters satisfying every encoding condition.
    pub encoding: u16,
    /// Parameter positions that condition packed, streamable, and fixed encoding.
    pub encoding_params: [Vec<u32>; 3],
    /// Region arguments held by the value. For a stable declaration these must
    /// outlive the enclosing region (MEM-6).
    pub region_params: Vec<u32>,
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
    pub declared: u16,
    pub body: DeclBody,
    pub role: Option<Role>,
    /// Core `Option` or `Result`: kind, bounds and copying follow the arguments
    /// (OWN-3, SEM-7), and a scoped argument is allowed (OWN-19).
    pub structural: bool,
    /// The function row of its `drop` block (FAIL-11), a body of this item.
    pub finalizer: Option<u32>,
    /// `grant bytes n` (MEM-27): the limit its finalizer runs under; 0 if undeclared.
    pub grant: u64,
    pub compact: Option<Width>,
    /// `@twin(P)` (MEM-17): the paired type as written, and the attribute's site.
    pub twin: Option<(TyId, Site)>,
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
    /// A byte-count parameter, whose argument keeps the `bytes` mark (LANG-5).
    pub bytes: bool,
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
    /// The function item's declaration-only callable type, shared by all its uses.
    pub callable: TyId,
    /// The written result type, where OWN-20 refuses a missing anchor.
    pub result_site: Site,
    /// Parameters the result is anchored to (OWN-20).
    pub anchors: Vec<u32>,
    /// Region roots named anywhere in the result, including nested type arguments.
    pub region_anchors: Vec<TyId>,
    /// Declared outlives edges; queries take their reflexive transitive closure.
    pub outlives: Vec<(TyId, TyId)>,
    pub waits: bool,
    pub body: Option<NodeId>,
    /// A resource's finalizer: the `drop` block of this type's item, under the type's
    /// own path. Its one parameter is the resource, `mut`, passed as its block pointer
    /// (FAIL-11, FORM).
    #[serde(default)]
    pub drop_of: Option<DeclId>,
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
        bounds: u16,
        params: Vec<u16>,
        site: Site,
        rule: &'static str,
    },
    /// A type argument of a generic other than Option and Result is not scoped (OWN-19).
    NotScoped { ty: TyId, site: Site },
    Root {
        base: TyId,
        root: TyId,
        at: ItemRef,
        site: Site,
    },
    Array {
        ty: TyId,
        params: Vec<u16>,
        site: Site,
    },
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
                let mut drop = None;
                let mut grant = 0;
                let mut compact = None;
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
                                if bit & (PLAIN | ENCODED) != 0 {
                                    self.refusals.push(refusal(
                                        "OWN-4",
                                        self.site(at, b),
                                        "plain, packed and streamable are computed, never declared",
                                    ));
                                }
                                declared |= bit;
                            }
                        }
                        NodeKind::Drop => drop = Some(c),
                        NodeKind::Literal => {
                            grant = literal_length(item, c).unwrap_or_else(|| {
                                self.refusals.push(refusal(
                                    "LANG-3",
                                    self.site(at, c),
                                    "a grant is an integer literal fitting u64",
                                ));
                                0
                            });
                        }
                        NodeKind::Attribute => {
                            let t = n.token.unwrap();
                            if item.tree.token_text(item.text, t) == "compact" {
                                let tokens = n.origin.tokens.clone();
                                compact = item.tree.tokens
                                    [tokens.start as usize..tokens.end as usize]
                                    .iter()
                                    .find_map(|t| {
                                        match &item.text[t.span.start as usize..t.span.end as usize]
                                        {
                                            "u16" => Some(Width::U16),
                                            "u32" => Some(Width::U32),
                                            "u64" => Some(Width::U64),
                                            _ => None,
                                        }
                                    });
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
                            callable: ERROR,
                            anchors: vec![],
                            region_anchors: vec![],
                            outlives: vec![],
                            waits: false,
                            body,
                            drop_of: None,
                        });
                        self.by_path.entry(path).or_insert(Def::Fn(id));
                        self.by_definition[definition.0 as usize] = Some(Def::Fn(id));
                        continue;
                    }
                    _ => continue,
                };
                let id = DeclId(self.decls.len() as u32);
                let finalizer = drop.map(|d| {
                    let f = self.fns.len() as u32;
                    let body = item.tree.children(d).last().copied();
                    self.fns.push(FnDecl {
                        path: path.clone(),
                        result_site: site,
                        site: self.site(at, d),
                        at,
                        generics: generics.clone(),
                        params: vec![],
                        result: UNIT,
                        callable: ERROR,
                        anchors: vec![],
                        region_anchors: vec![],
                        outlives: vec![],
                        waits: true,
                        body,
                        drop_of: Some(id),
                    });
                    f
                });
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
                    grant,
                    compact,
                    twin: None,
                    summary: Summary::default(),
                });
                self.by_path.entry(path).or_insert(Def::Type(id));
                self.by_definition[definition.0 as usize] = Some(Def::Type(id));
            }
        }
        self.alias_state = vec![Visit::Todo as u8; self.decls.len() - self.decls.base_len()];
    }

    fn generics(&mut self, at: ItemRef, n: NodeId) -> Vec<Generic> {
        let item = self.item(at);
        let mut out = vec![];
        for &g in item.tree.children(n) {
            let gn = item.tree.node(g);
            let name = item.symbol(gn.token.unwrap());
            let mut bounds = 0;
            for &b in item.tree.children(g) {
                for &bn in item.tree.children(b) {
                    let t = item.tree.node(bn).token.unwrap();
                    bounds |= bound_bit(item.tree.token_text(item.text, t));
                }
            }
            out.push(Generic {
                name,
                bounds: implied(bounds),
                region: gn.kind == NodeKind::RegionGeneric,
            });
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

    fn alias_visit(&self, d: DeclId) -> u8 {
        if d.index() < self.decls.base_len() {
            Visit::Done as u8
        } else {
            self.alias_state[d.index() - self.decls.base_len()]
        }
    }
    fn lower_decl(&mut self, d: DeclId) {
        if self.alias_visit(d) != Visit::Todo as u8 {
            return;
        }
        self.alias_state[d.index() - self.decls.base_len()] = Visit::Open as u8;
        self.alias_stack.push((d, None));
        let at = self.decls[d.index()].at;
        let item = self.item(at);
        let mut generics = self.decls[d.index()].generics.clone();
        for g in &mut generics {
            if !g.region {
                g.bounds |= self.decls[d.index()].declared;
            }
        }
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
                        let ty = if self.decls[d.index()].repr == Repr::Stable {
                            self.default_field_root(ty, &generics)
                        } else {
                            ty
                        };
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
                            let ty = if self.decls[d.index()].repr == Repr::Stable {
                                self.default_field_root(ty, &generics)
                            } else {
                                ty
                            };
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
                NodeKind::Attribute
                    if item.tree.token_text(item.text, n.token.unwrap()) == "twin" =>
                {
                    let path = item.tree.children(c)[0];
                    let ty = self.lower_ty(at, path, &[]);
                    let site = self.site(at, c);
                    self.decls[d.index()].twin = Some((ty, site));
                }
                NodeKind::TypePath
                | NodeKind::TypeArguments
                | NodeKind::UnitType
                | NodeKind::GroupType
                | NodeKind::TupleType
                | NodeKind::CallableType
                | NodeKind::OwnCallableType
                | NodeKind::ScopedCallableType
                | NodeKind::Anchor => {
                    let ty = self.lower_ty(at, c, &generics);
                    body = DeclBody::Alias(ty);
                }
                _ => {}
            }
        }
        self.decls[d.index()].body = body;
        let mut roots = vec![];
        match self.decls[d.index()].body {
            DeclBody::Alias(ty) => collect_roots(&self.types, ty, &mut roots),
            _ => {
                for f in self.decls[d.index()].all_fields() {
                    collect_roots(&self.types, f.ty, &mut roots);
                }
            }
        }
        self.decls[d.index()].summary.region_params = roots
            .into_iter()
            .filter_map(|r| match self.types.kind(r) {
                TyKind::Region(Root::Generic(i)) => Some(*i),
                _ => None,
            })
            .collect();
        self.alias_state[d.index() - self.decls.base_len()] = Visit::Done as u8;
        self.alias_stack.pop();
    }

    fn default_field_root(&mut self, ty: TyId, generics: &[Generic]) -> TyId {
        let stable = match self.types.kind(ty) {
            TyKind::Adt(d, _) => {
                self.decls[d.index()].repr == Repr::Stable
                    || self.decls[d.index()]
                        .role
                        .is_some_and(|r| r.kind() == Kind::Stable)
            }
            TyKind::Param(i) => generics[*i as usize].bounds & ENCODED != 0,
            _ => false,
        };
        if stable {
            let root = self.types.intern(TyKind::Region(Root::Enclosing));
            return self.types.intern(TyKind::Anchored(ty, root));
        }
        match self.types.kind(ty).clone() {
            TyKind::Tuple(xs) => {
                let xs = xs
                    .into_iter()
                    .map(|x| self.default_field_root(x, generics))
                    .collect();
                self.types.intern(TyKind::Tuple(xs))
            }
            TyKind::Adt(d, xs) if self.decls[d.index()].structural => {
                let xs = xs
                    .into_iter()
                    .map(|x| self.default_field_root(x, generics))
                    .collect();
                self.types.intern(TyKind::Adt(d, xs))
            }
            _ => ty,
        }
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
            NodeKind::TypeArguments if matches!(self.target(at, children[0]), Some(Target::Builtin(name)) if self.index.symbols.text(*name) == "Inline") =>
            {
                if children.len() != 3 {
                    self.refusals.push(refusal(
                        "LANG-12",
                        self.site(at, n),
                        "Inline takes an element type and a literal length",
                    ));
                    return ERROR;
                }
                let element = self.lower_ty(at, children[1], generics);
                let count = literal_length(item, children[2]);
                let Some(count) = count else {
                    self.refusals.push(refusal(
                        if item.tree.node(children[2]).kind == NodeKind::Literal {
                            "LANG-3"
                        } else {
                            "SEM-9"
                        },
                        self.site(at, children[2]),
                        "Inline length is a literal fitting its integer type and u64",
                    ));
                    return ERROR;
                };
                self.pending.push(Pending::Array {
                    ty: element,
                    params: generics.iter().map(|g| g.bounds).collect(),
                    site: self.site(at, children[1]),
                });
                self.types.intern(TyKind::Array(element, count))
            }
            NodeKind::TypeArguments => {
                let mut args = vec![];
                for &c in &children[1..] {
                    let t = self.lower_ty(at, c, generics);
                    args.push((t, self.site(at, c)));
                }
                self.named_ty(at, children[0], &args, generics)
            }
            NodeKind::Anchor if children.len() == 1 => {
                let root = self.root_ty(at, children[0], generics);
                self.pending.push(Pending::Root {
                    base: ERROR,
                    root,
                    at,
                    site: self.site(at, children[0]),
                });
                root
            }
            NodeKind::Anchor => {
                let ty = self.lower_ty(at, children[0], generics);
                let root = self.root_ty(at, children[1], generics);
                self.pending.push(Pending::Root {
                    base: ty,
                    root,
                    at,
                    site: self.site(at, children[1]),
                });
                if scoped_payload(&self.decls, &self.types, ty) {
                    ty
                } else {
                    self.types.intern(TyKind::Anchored(ty, root))
                }
            }
            NodeKind::CallableType | NodeKind::OwnCallableType | NodeKind::ScopedCallableType => {
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
                let callable = match callable_kind(item, n) {
                    Ok(k) => k,
                    Err(bound) => {
                        self.refusals.push(refusal(
                            "LANG-13",
                            self.site(at, bound),
                            "an owning callable bracket contains only send",
                        ));
                        return ERROR;
                    }
                };
                self.types
                    .intern(TyKind::Fn(params, result, waits, callable))
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
        let params: Vec<u16> = generics.iter().map(|g| g.bounds).collect();
        let d = &self.decls[decl.index()];
        let role = d.role;
        if !d.structural {
            for (i, (t, s)) in args.iter().enumerate() {
                self.pending.push(Pending::NotScoped { ty: *t, site: *s });
                if d.generics[i].region {
                    self.pending.pop();
                    if !matches!(self.types.kind(*t), TyKind::Region(_)) && *t != ERROR {
                        self.refusals.push(refusal(
                            "MEM-6",
                            *s,
                            "a region parameter takes an `in r` argument",
                        ));
                    }
                    continue;
                }
                if matches!(self.types.kind(*t), TyKind::Region(_)) {
                    self.refusals.push(refusal(
                        "LANG-12",
                        *s,
                        "a type parameter takes a type, not a region",
                    ));
                    continue;
                }
                let need = d.generics[i].bounds;
                if need != 0 {
                    let counted = matches!(role, Some(Role::Rc | Role::Arc));
                    self.pending.push(Pending::Bound {
                        ty: *t,
                        bounds: need,
                        params: params.clone(),
                        site: *s,
                        rule: if counted {
                            "OWN-4"
                        } else if matches!(
                            role,
                            Some(
                                Role::FixedSpan
                                    | Role::SeqSpan
                                    | Role::IndexedSpan
                                    | Role::SeqIndex
                            )
                        ) {
                            "MEM-12"
                        } else {
                            "LANG-12"
                        },
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
        if alias && self.alias_visit(decl) == Visit::Open as u8 {
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
        if self.alias_visit(d) == Visit::Done as u8 {
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
                    | NodeKind::Anchor
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
        if let Some(d) = self.fns[f].drop_of {
            // FAIL-11: `drop(t)` takes the resource `mut`; it cannot move, copy or
            // return it, and has a unit result. It is shielded and may wait.
            let args = (0..self.fns[f].generics.len())
                .map(|i| self.types.intern(TyKind::Param(i as u32)))
                .collect();
            let ty = self.types.intern(TyKind::Adt(d, args));
            let node = item
                .tree
                .children(item.root)
                .iter()
                .copied()
                .find(|&c| item.tree.node(c).kind == NodeKind::Drop)
                .unwrap();
            let name = item.symbol(item.tree.node(node).token.unwrap());
            let site = self.fns[f].site;
            let fd = &mut self.fns[f];
            fd.params = vec![ParamDecl {
                bytes: false,
                name,
                mode: Mode::Mut,
                ty,
                site,
            }];
            fd.callable = self.types.intern(TyKind::Fn(
                vec![(Mode::Mut, ty)],
                UNIT,
                true,
                Callable::Plain,
            ));
            return;
        }
        let generics = self.fns[f].generics.clone();
        let mut params = vec![];
        let mut result = UNIT;
        let mut anchor_nodes = vec![];
        let mut written_roots = vec![];
        let mut waits = false;
        for &c in item.tree.children(item.root) {
            match item.tree.node(c).kind {
                NodeKind::Parameters => {
                    for &p in item.tree.children(c) {
                        let pn = item.tree.node(p);
                        let name = item.symbol(pn.token.unwrap());
                        let tnode = item.tree.children(p)[0];
                        let ty = self.lower_ty(at, tnode, &generics);
                        let bytes = pn.kind == NodeKind::BytesParameter;
                        if bytes && ty != scalar(Scalar::U64) && ty != ERROR {
                            self.refusals.push(refusal(
                                "LANG-5",
                                self.site(at, p),
                                "a `bytes` parameter has type `u64`",
                            ));
                        }
                        params.push(ParamDecl {
                            bytes,
                            name,
                            mode: mode_of(pn.kind),
                            ty,
                            site: self.site(at, p),
                        });
                    }
                }
                NodeKind::ResultType => {
                    let t = item.tree.children(c)[0];
                    fn anchors(
                        item: hirsel_core_syntax::Item<'_>,
                        n: NodeId,
                        out: &mut Vec<NodeId>,
                    ) {
                        let kids = item.tree.children(n);
                        if item.tree.node(n).kind == NodeKind::Anchor {
                            out.push(*kids.last().unwrap());
                        }
                        for &c in kids {
                            anchors(item, c, out);
                        }
                    }
                    anchors(item, t, &mut anchor_nodes);
                    result = self.lower_ty(at, t, &generics);
                    self.fns[f].result_site = self.site(at, t);
                }
                NodeKind::Waits => waits = true,
                NodeKind::Outlives => {
                    for pair in item.tree.children(c).as_chunks::<2>().0 {
                        let a = self.root_ty(at, pair[0], &generics);
                        let b = self.root_ty(at, pair[1], &generics);
                        for (&node, root) in pair.iter().zip([a, b]) {
                            if let TyKind::Region(Root::Parameter(i)) = self.types.kind(root)
                                && !matches!(self.types.kind(params[*i as usize].ty), TyKind::Adt(d, _) if self.decls[d.index()].role == Some(Role::Region))
                            {
                                self.refusals.push(refusal(
                                    "MEM-6",
                                    self.site(at, node),
                                    "an outlives clause names only region parameters",
                                ));
                            }
                        }
                        self.fns[f].outlives.push((a, b));
                    }
                }
                _ => {}
            }
        }
        let mut anchors = vec![];
        for &a in &anchor_nodes {
            let parts = path_text(item, a);
            match params
                .iter()
                .position(|p| self.index.symbols.text(p.name) == parts)
            {
                Some(i) => anchors.push(i as u32),
                _ if generics
                    .iter()
                    .any(|g| g.region && self.index.symbols.text(g.name) == parts) => {}
                _ => self.refusals.push(refusal(
                    "OWN-20",
                    self.site(at, a),
                    format!("anchor `{parts}` is not a view parameter"),
                )),
            }
        }
        for &n in &anchor_nodes {
            written_roots.push(self.root_ty(at, n, &generics));
        }
        let callable = self.types.intern(TyKind::Fn(
            params.iter().map(|p| (p.mode, p.ty)).collect(),
            result,
            waits,
            Callable::Plain,
        ));
        let fd = &mut self.fns[f];
        fd.callable = callable;
        fd.region_anchors = written_roots;
        fd.params = params;
        fd.result = result;
        fd.anchors = anchors;
        fd.waits = waits;
        let edges = std::mem::take(&mut fd.outlives);
        let mut pending = vec![];
        let mut seen = vec![];
        for &(source, _) in &edges {
            if fd.outlives.iter().any(|&(a, _)| a == source) {
                continue;
            }
            pending.clear();
            seen.clear();
            pending.push(source);
            while let Some(root) = pending.pop() {
                if seen.contains(&root) {
                    continue;
                }
                seen.push(root);
                fd.outlives.push((source, root));
                pending.extend(
                    edges
                        .iter()
                        .filter(|&&(from, _)| from == root)
                        .map(|&(_, to)| to),
                );
            }
        }
        collect_roots(&self.types, result, &mut fd.region_anchors);
        for &root in &fd.region_anchors {
            if let TyKind::Region(Root::Parameter(i)) = self.types.kind(root) {
                if !fd.anchors.contains(i) {
                    fd.anchors.push(*i);
                }
            }
        }
    }

    /// A root names a region generic, a Region parameter, or a view parameter.
    fn root_ty(&mut self, at: ItemRef, n: NodeId, generics: &[Generic]) -> TyId {
        let item = self.item(at);
        let Some(Target::Binder(b)) = self.target(at, n).copied() else {
            return ERROR;
        };
        let binder = &self.items[at.module as usize][at.item as usize]
            .resolution
            .binders[b as usize];
        if let Some(i) = generics
            .iter()
            .position(|g| g.region && g.name == binder.name)
        {
            return self.types.intern(TyKind::Region(Root::Generic(i as u32)));
        }
        let parameters = item
            .tree
            .children(item.root)
            .iter()
            .copied()
            .find(|&c| item.tree.node(c).kind == NodeKind::Parameters);
        if let Some(parameters) = parameters {
            for (i, &p) in item.tree.children(parameters).iter().enumerate() {
                if item.symbol(item.tree.node(p).token.unwrap()) == binder.name {
                    return self.types.intern(TyKind::Region(Root::Parameter(i as u32)));
                }
            }
        }
        self.refusals.push(refusal(
            "MEM-6",
            self.site(at, n),
            "a root names a region or view parameter",
        ));
        ERROR
    }

    /// Pass 3: summaries in dependency order, then the deferred checks.
    pub(crate) fn summarize(&mut self) {
        let n = self.decls.len();
        let base = self.decls.base_len();
        let mut state = vec![Visit::Todo; n - base];
        let mut stack: Vec<(DeclId, usize)> = vec![];
        let order = base..n;
        for d in order {
            if state[d - base] == Visit::Todo {
                self.inline_visit(DeclId(d as u32), &mut state, &mut stack);
            }
        }
        self.bound_conditions();
        self.encodings();
        self.holds();
        self.check_declared_bounds();
        self.check_twins();
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
                    if have & bounds != bounds {
                        self.refusals.push(refusal(
                            rule,
                            site,
                            format!("type argument is not {}", bound_names(bounds & !have)),
                        ));
                    }
                }
                Pending::Root {
                    base,
                    root,
                    at,
                    site,
                } => {
                    if let TyKind::Region(Root::Parameter(i)) = self.types.kind(root) {
                        let fd = self
                            .fns
                            .own_iter()
                            .find(|f| f.at == at)
                            .expect("parameter root belongs to a function");
                        let p = &fd.params[*i as usize];
                        let scoped = kind(&self.decls, &self.types, base, &[]) == Kind::Scoped;
                        let region = matches!(self.types.kind(p.ty), TyKind::Adt(d, _) if self.decls[d.index()].role == Some(Role::Region));
                        if !region && !scoped && !matches!(self.types.kind(base), TyKind::Param(_))
                        {
                            self.refusals.push(refusal(
                                "MEM-6",
                                site,
                                "a stable root must name a Region parameter",
                            ));
                        } else if scoped && p.mode != Mode::View {
                            self.refusals.push(refusal(
                                "OWN-20",
                                fd.result_site,
                                "a scoped anchor names a view or Region parameter",
                            ));
                        }
                    }
                }
                Pending::Array { ty, params, site } => {
                    if !matches!(
                        kind(&self.decls, &self.types, ty, &params),
                        Kind::Plain | Kind::Own
                    ) {
                        self.refusals.push(refusal(
                            "SEM-9",
                            site,
                            "Inline elements must be plain or own; a type parameter must be plain",
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
        state[d.index() - self.decls.base_len()] = Visit::Open;
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
        state[d.index() - self.decls.base_len()] = Visit::Done;
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
            TyKind::Anchored(x, _) | TyKind::Array(x, _) => self.inline_ty(x, state, stack),
            TyKind::View(_) => (Kind::Scoped, 0),
            TyKind::Fn(..) => (kind(&self.decls, &self.types, ty, &[]), 0),
            TyKind::Adt(e, args) => {
                if let Some(r) = self.decls[e.index()].role {
                    // An indirection: fixed size, its payload is not read (OWN-2).
                    return (r.kind(), 0);
                }
                match if e.index() < self.decls.base_len() {
                    Visit::Done
                } else {
                    state[e.index() - self.decls.base_len()]
                } {
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

    /// Compute encoding summaries from each declaration graph, assuming a recursive
    /// group's own encoding while visiting every field. No iterative approximation.
    fn encodings(&mut self) {
        for d in self.decls.base_len()..self.decls.len() {
            let args: Vec<_> = (0..self.decls[d].generics.len())
                .map(|i| self.types.intern(TyKind::Param(i as u32)))
                .collect();
            let ty = self.types.intern(TyKind::Adt(DeclId(d as u32), args));
            let mut params = vec![PLAIN | ENCODED | FIXED_ENCODING; self.decls[d].generics.len()];
            let encoding = derive_encoding(
                &self.decls,
                &mut self.types,
                ty,
                &params,
                &mut vec![],
                false,
            );
            let mut conditions: [Vec<u32>; 3] = Default::default();
            for i in 0..params.len() {
                params[i] = 0;
                let without = derive_encoding(
                    &self.decls,
                    &mut self.types,
                    ty,
                    &params,
                    &mut vec![],
                    false,
                );
                for (k, bit) in [PACKED, STREAMABLE, FIXED_ENCODING].into_iter().enumerate() {
                    if encoding & bit != 0 && without & bit == 0 {
                        conditions[k].push(i as u32);
                    }
                }
                params[i] = PLAIN | ENCODED | FIXED_ENCODING;
            }
            self.decls[d].summary.encoding = encoding;
            self.decls[d].summary.encoding_params = conditions;
        }
    }

    /// Parameter dependency reachability, from declarations alone. Each edge follows a
    /// field's type-argument position; a phantom position has no path to a field.
    fn bound_conditions(&mut self) {
        for d in self.decls.base_len()..self.decls.len() {
            let mut mentioned = vec![];
            for i in 0..self.decls[d].generics.len() {
                if mentions(
                    &self.decls,
                    &self.types,
                    DeclId(d as u32),
                    i as u32,
                    &mut vec![],
                ) {
                    mentioned.push(i as u32);
                }
            }
            self.decls[d].summary.bound_params = mentioned;
        }
    }

    /// Holds sets: types that name each other through any field form a group, and each
    /// member holds the group's union (OWN-2). One Tarjan pass, no iteration.
    fn holds(&mut self) {
        let base = self.decls.base_len();
        let n = self.decls.len() - base;
        let mut direct = vec![0u8; n];
        let mut refs: Vec<Vec<usize>> = vec![vec![]; n];
        for d in base..self.decls.len() {
            let decl = &self.decls[d];
            let mut params = 0u64;
            let mut h = match decl.role {
                Some(Role::Capability) => H_CAPABILITY,
                Some(Role::Task | Role::Sender | Role::Receiver | Role::HostFile) => H_RESOURCE,
                Some(Role::View | Role::Region | Role::Dest | Role::Writer) => H_SCOPED,
                Some(
                    Role::Text
                    | Role::FixedSpan
                    | Role::SeqSpan
                    | Role::IndexedSpan
                    | Role::SeqIndex,
                ) => H_STABLE,
                // A builder and `Causes` are owners with no copy contract (OWN-2); a
                // builder also holds its region's stable blob.
                Some(Role::SpanBuilder | Role::Builder) => H_STABLE | H_OPAQUE,
                Some(Role::Causes) => H_OPAQUE,
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
                    walk_holds(
                        &self.decls,
                        &self.types,
                        f.ty,
                        &mut h,
                        &mut params,
                        &mut refs[d - base],
                    );
                }
            }
            refs[d - base].retain_mut(|other| {
                if *other < base {
                    h |= self.decls[*other].summary.holds;
                    false
                } else {
                    *other -= base;
                    true
                }
            });
            direct[d - base] = h;
            self.decls[d].summary.holds_params = params;
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
        for d in base..self.decls.len() {
            self.decls[d].summary.holds = t.out[d - base];
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
            let declared = decl.declared & !PLAIN;
            let params: Vec<u16> = decl.generics.iter().map(|g| g.bounds | declared).collect();
            let mut out = vec![];
            let forbidden = if decl.repr != Repr::Own { SHAREABLE } else { 0 }
                | if matches!(decl.repr, Repr::Stable | Repr::Scoped) {
                    SEND
                } else {
                    0
                };
            if declared & forbidden != 0 {
                out.push(refusal(
                    "OWN-4",
                    decl.site,
                    format!(
                        "this representation cannot be {}",
                        bound_names(declared & forbidden)
                    ),
                ));
            }
            if decl.compact.is_some() && decl.repr != Repr::Stable {
                out.push(refusal(
                    "MEM-9",
                    decl.site,
                    "compact layout requires a stable declaration",
                ));
            }
            for f in decl.all_fields() {
                if decl.repr == Repr::Stable
                    && !stable_field(&self.decls, &self.types, f.ty, &params)
                {
                    out.push(refusal(
                        if decl.compact.is_some() {
                            "MEM-9"
                        } else {
                            "MEM-2"
                        },
                        f.site,
                        "a stable field is plain or stable",
                    ));
                } else if decl.repr != Repr::Scoped
                    && kind(&self.decls, &self.types, f.ty, &params) == Kind::Scoped
                {
                    out.push(refusal(
                        "OWN-19",
                        f.site,
                        format!("field `{}` of a non-scoped type is scoped", f.name),
                    ));
                }
                if decl.repr != Repr::Stable && unrooted_stable(&self.decls, &self.types, f.ty) {
                    out.push(refusal(
                        "MEM-6",
                        f.site,
                        "a stored stable field names its region",
                    ));
                }
                let have = has_bounds(&self.decls, &self.types, f.ty, &params);
                let missing = declared & !have;
                if missing != 0 {
                    out.push(refusal(
                        if missing & SEND != 0 {
                            "CONC-2"
                        } else {
                            "OWN-4"
                        },
                        f.site,
                        format!("field `{}` is not {}", f.name, bound_names(missing)),
                    ));
                }
            }
            self.refusals.extend(out);
        }
    }

    /// `@twin(P)` (MEM-17): a non-generic declaration pairs with the non-generic `P`,
    /// symmetrically, when their fields (or arms and payloads) have the same names in
    /// the same order and each pair of types pairs.
    fn check_twins(&mut self) {
        for d in self.decls.base_len()..self.decls.len() {
            let Some((ty, site)) = self.decls[d].twin.clone() else {
                continue;
            };
            let why = match self.types.kind(ty) {
                TyKind::Error => continue,
                TyKind::Adt(e, xs) if xs.is_empty() && self.decls[e.index()].role.is_none() => {
                    let e = *e;
                    let other = self.decls[e.index()].twin.as_ref().map(|(t, _)| *t);
                    if !self.decls[d].generics.is_empty() {
                        Some("a twin declaration is not generic".to_owned())
                    } else if e.index() == d {
                        Some("a type is not its own twin".to_owned())
                    } else if other.is_some_and(
                        |t| !matches!(self.types.kind(t), TyKind::Adt(x, _) if x.index() == d),
                    ) {
                        Some("the twin names another twin".to_owned())
                    } else {
                        twin_decls(&self.decls, &self.types, DeclId(d as u32), e).err()
                    }
                }
                _ => Some("a twin is a declared non-generic type".to_owned()),
            };
            if let Some(why) = why {
                self.refusals.push(refusal("MEM-17", site, why));
            }
        }
    }

    fn check_signatures(&mut self) {
        for f in self.fns.base_len()..self.fns.len() {
            let fd = &self.fns[f];
            let params: Vec<u16> = fd.generics.iter().map(|g| g.bounds).collect();
            if unrooted_stable(&self.decls, &self.types, fd.result) {
                self.refusals.push(refusal(
                    "MEM-6",
                    fd.result_site,
                    "a stable result names its region",
                ));
            }
            for p in &fd.params {
                if p.mode == Mode::Mut
                    && kind(&self.decls, &self.types, p.ty, &params) == Kind::Stable
                {
                    self.refusals
                        .push(refusal("MEM-5", p.site, "stable data cannot be mutated"));
                }
            }
            if kind(&self.decls, &self.types, fd.result, &params) == Kind::Scoped
                && {
                    let item = self.item(fd.at);
                    let result = item
                        .tree
                        .children(item.root)
                        .iter()
                        .copied()
                        .find(|&c| item.tree.node(c).kind == NodeKind::ResultType)
                        .unwrap();
                    written_missing_anchor(self, fd.at, item.tree.children(result)[0])
                }
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

/// The declared twin of `d`, either way round (MEM-17: the pair is symmetric).
pub fn twin_of(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, d: DeclId) -> Option<DeclId> {
    let named = |x: DeclId| match decls[x.index()].twin.as_ref().map(|(ty, _)| t.kind(*ty)) {
        Some(TyKind::Adt(e, xs)) if xs.is_empty() => Some(*e),
        _ => None,
    };
    named(d).or_else(|| {
        (0..decls.len())
            .map(|i| DeclId(i as u32))
            .find(|&x| named(x) == Some(d))
    })
}

fn unanchored(t: &impl TyRead, ty: TyId) -> TyId {
    match t.kind(ty) {
        TyKind::Anchored(x, _) => *x,
        _ => ty,
    }
}

/// Whether declarations `a` and `b` pair as twins, assuming they do while their own
/// fields are checked (so recursive twins check); `Err` names the first mismatch.
fn twin_decls(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    a: DeclId,
    b: DeclId,
) -> Result<(), String> {
    let pair_fields = |x: &[FieldDecl], y: &[FieldDecl]| -> Result<(), String> {
        if x.len() != y.len() || x.iter().zip(y).any(|(f, g)| f.name != g.name) {
            return Err("twins have the same field names in the same order".to_owned());
        }
        for (f, g) in x.iter().zip(y) {
            if !twin_tys(decls, t, (a, b), f.ty, g.ty) {
                return Err(format!("field `{}` has no paired type", f.name));
            }
        }
        Ok(())
    };
    match (&decls[a.index()].body, &decls[b.index()].body) {
        (DeclBody::Record(x), DeclBody::Record(y)) => pair_fields(x, y),
        (DeclBody::Variant(x), DeclBody::Variant(y)) => {
            if x.len() != y.len() || x.iter().zip(y).any(|(c, k)| c.name != k.name) {
                return Err("twins have the same arm names in the same order".to_owned());
            }
            x.iter()
                .zip(y)
                .try_for_each(|(c, k)| pair_fields(&c.fields, &k.fields))
        }
        _ => Err("twins are both records or both variant types".to_owned()),
    }
}

/// MEM-17's pairing of two field types: identical, a built-in pair, a declared (or
/// assumed) twin pair, or `Option`, `Result` or a tuple of pairs, component by component.
fn twin_tys(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    assumed: (DeclId, DeclId),
    x: TyId,
    y: TyId,
) -> bool {
    let (x, y) = (unanchored(t, x), unanchored(t, y));
    if x == y {
        return true;
    }
    let twins = |p: DeclId, q: DeclId| {
        assumed == (p, q) || assumed == (q, p) || twin_of(decls, t, p) == Some(q)
    };
    match (t.kind(x), t.kind(y)) {
        (TyKind::Tuple(xs), TyKind::Tuple(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(&p, &q)| twin_tys(decls, t, assumed, p, q))
        }
        (TyKind::Adt(p, xs), TyKind::Adt(q, ys)) => {
            let (dp, dq) = (&decls[p.index()], &decls[q.index()]);
            let pair = |r: Role, s: Role| {
                (dp.role == Some(r) && dq.role == Some(s))
                    || (dp.role == Some(s) && dq.role == Some(r))
            };
            if pair(Role::String, Role::Text) {
                return true;
            }
            if pair(Role::List, Role::FixedSpan) {
                return unanchored(t, xs[0]) == unanchored(t, ys[0]);
            }
            if p == q && dp.structural {
                return xs
                    .iter()
                    .zip(ys)
                    .all(|(&a, &b)| twin_tys(decls, t, assumed, a, b));
            }
            // `Box[X]` and a stable reference to `Y`, where `X` and `Y` are twins.
            let boxed = |r: &TypeDecl, args: &[TyId], s: DeclId| {
                r.role == Some(Role::Box)
                    && decls[s.index()].repr == Repr::Stable
                    && matches!(t.kind(unanchored(t, args[0])), TyKind::Adt(b, _) if twins(*b, s))
            };
            if boxed(dp, xs, *q) || boxed(dq, ys, *p) {
                return true;
            }
            xs.is_empty()
                && ys.is_empty()
                && dp.role.is_none()
                && dq.role.is_none()
                && twins(*p, *q)
        }
        _ => false,
    }
}

fn walk_holds(
    decls: &crate::Rows<TypeDecl>,
    t: &Types,
    ty: TyId,
    h: &mut u8,
    params: &mut u64,
    refs: &mut Vec<usize>,
) {
    match t.kind(ty) {
        TyKind::Param(i) => *params |= 1 << i,
        TyKind::Tuple(xs) => {
            for &x in xs {
                walk_holds(decls, t, x, h, params, refs);
            }
        }
        TyKind::Anchored(x, _) | TyKind::Array(x, _) => walk_holds(decls, t, *x, h, params, refs),
        TyKind::Fn(_, _, _, Callable::Scoped) => *h |= H_SCOPED,
        TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend) => *h |= H_OPAQUE,
        TyKind::View(x) => {
            *h |= H_SCOPED;
            walk_holds(decls, t, *x, h, params, refs);
        }
        TyKind::Adt(e, xs) => {
            refs.push(e.index());
            for &i in &decls[e.index()].summary.bound_params {
                walk_holds(decls, t, xs[i as usize], h, params, refs);
            }
        }
        _ => {}
    }
}

fn mentions(
    decls: &crate::Rows<TypeDecl>,
    t: &Types,
    d: DeclId,
    i: u32,
    seen: &mut Vec<(DeclId, u32)>,
) -> bool {
    if seen.contains(&(d, i)) {
        return false;
    }
    seen.push((d, i));
    let decl = &decls[d.index()];
    if let Some(role) = decl.role {
        return matches!(
            role,
            Role::View
                | Role::Box
                | Role::Rc
                | Role::Arc
                | Role::List
                | Role::Map
                | Role::Task
                | Role::FixedSpan
                | Role::SeqSpan
                | Role::IndexedSpan
                | Role::SeqIndex
        );
    }
    fn ty_mentions(
        decls: &crate::Rows<TypeDecl>,
        t: &Types,
        ty: TyId,
        i: u32,
        seen: &mut Vec<(DeclId, u32)>,
    ) -> bool {
        match t.kind(ty) {
            TyKind::Param(p) => *p == i,
            TyKind::Tuple(xs) => xs.iter().any(|&x| ty_mentions(decls, t, x, i, seen)),
            TyKind::Anchored(x, _) | TyKind::View(x) | TyKind::Array(x, _) => {
                ty_mentions(decls, t, *x, i, seen)
            }
            TyKind::Adt(d, xs) => xs.iter().enumerate().any(|(p, &x)| {
                ty_mentions(decls, t, x, i, seen)
                    && mentions(decls, t, *d, p as u32, &mut seen.clone())
            }),
            _ => false,
        }
    }
    match decl.body {
        DeclBody::Alias(ty) => ty_mentions(decls, t, ty, i, seen),
        _ => decl
            .all_fields()
            .any(|f| ty_mentions(decls, t, f.ty, i, seen)),
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
pub fn kind(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> Kind {
    match t.kind(ty) {
        TyKind::Error | TyKind::Hole | TyKind::Never | TyKind::Unit | TyKind::Scalar(_) => {
            Kind::Plain
        }
        TyKind::Fn(_, _, _, callable) => match callable {
            Callable::Plain => Kind::Plain,
            Callable::Own | Callable::OwnSend => Kind::Own,
            Callable::Scoped => Kind::Scoped,
        },
        TyKind::Tuple(xs) => xs
            .iter()
            .fold(Kind::Plain, |k, &x| join(k, kind(decls, t, x, params))),
        TyKind::Region(_) => Kind::Scoped,
        TyKind::Anchored(x, _) => {
            let k = kind(decls, t, *x, params);
            if k == Kind::Generic { Kind::Stable } else { k }
        }
        TyKind::Array(x, _) => kind(decls, t, *x, params),
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
pub fn copyable(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> bool {
    match kind(decls, t, ty, params) {
        Kind::Plain | Kind::Stable => true,
        Kind::Scoped => match t.kind(ty) {
            TyKind::Anchored(x, _) => copyable(decls, t, *x, params),
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
pub fn has_bounds(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> u16 {
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
            TyKind::Error | TyKind::Hole => all | INTEGER | FLOAT | ENCODED,
            TyKind::Never | TyKind::Unit => all,
            TyKind::Scalar(Scalar::F32 | Scalar::F64) => PLAIN | SHAREABLE | SEND | FLOAT | ENCODED,
            TyKind::Scalar(s) if hirsel_core_ir::Ty::Scalar(*s).int().is_some() => {
                all | INTEGER | ENCODED
            }
            TyKind::Scalar(_) => all | ENCODED,
            TyKind::Fn(_, _, _, callable) => match callable {
                Callable::Plain => PLAIN | SHAREABLE | SEND,
                Callable::OwnSend => SEND,
                Callable::Own | Callable::Scoped => 0,
            },
            TyKind::Tuple(xs) => meet(xs) | encoding_bounds(decls, t, ty, params) & ENCODED,
            TyKind::Region(_) => 0,
            TyKind::Anchored(x, _) => has_bounds(decls, t, *x, params),
            TyKind::Array(x, _) => has_bounds(decls, t, *x, params) & all,
            TyKind::View(x) => has_bounds(decls, t, *x, params) & (KEY | ORDERED),
            TyKind::Param(i) => params.get(*i as usize).copied().unwrap_or(0),
            TyKind::Adt(d, args) => {
                let decl = &decls[d.index()];
                let declared = match decl.role {
                    Some(Role::List) => meet(args) & (KEY | ORDERED | SHAREABLE | SEND),
                    Some(Role::String) => KEY | ORDERED | SHAREABLE | SEND,
                    Some(Role::Text) => KEY | ORDERED,
                    Some(Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan) => {
                        meet(args) & (KEY | ORDERED)
                    }
                    Some(Role::Child | Role::Unique | Role::Site) => PLAIN | SHAREABLE | SEND | KEY,
                    Some(Role::Args) => PLAIN | SHAREABLE | SEND,
                    Some(Role::Causes) => SEND,
                    Some(Role::Rc) => SHAREABLE | (meet(args) & (KEY | ORDERED)),
                    Some(Role::Arc) => SHAREABLE | SEND | (meet(args) & (KEY | ORDERED)),
                    Some(Role::Box) => meet(args) & (SHAREABLE | SEND | KEY | ORDERED),
                    Some(Role::Sender | Role::Receiver | Role::HostFile) => SEND,
                    Some(Role::Map) => meet(args) & (SHAREABLE | SEND),
                    Some(Role::Capability) => PLAIN | SHAREABLE | SEND,
                    Some(_) => 0,
                    None if decl.structural => meet(args),
                    None => {
                        let mut bounds = decl.declared & !PLAIN;
                        for &i in &decl.summary.bound_params {
                            bounds &= has_bounds(decls, t, args[i as usize], params);
                        }
                        bounds
                    }
                };
                let declared = if matches!(decl.repr, Repr::Stable | Repr::Scoped) {
                    declared & !(SEND | SHAREABLE)
                } else {
                    declared
                };
                declared | encoding_bounds(decls, t, ty, params) & ENCODED
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
            let _ = decls[d.index()].path.write_to(s);
            if !xs.is_empty() {
                s.push('[');
                list(xs, s);
                s.push(']');
            }
        }
        TyKind::Region(root) => {
            let _ = write!(s, "in {root:?}");
        }
        TyKind::Anchored(x, r) => {
            write_canonical(decls, t, *x, s);
            s.push(' ');
            write_canonical(decls, t, *r, s);
        }
        TyKind::Array(x, n) => {
            s.push_str("Inline[");
            write_canonical(decls, t, *x, s);
            let _ = write!(s, ", {n}]");
        }
        TyKind::View(x) => {
            s.push_str("View[");
            write_canonical(decls, t, *x, s);
            s.push(']');
        }
        TyKind::Param(i) => {
            let _ = write!(s, "${i}");
        }
        TyKind::Fn(ps, r, w, k) => {
            s.push_str(match k {
                Callable::Plain => "fn(",
                Callable::Own => "own fn(",
                Callable::OwnSend => "own[send] fn(",
                Callable::Scoped => "scoped fn(",
            });
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

/// A literal inline length, checked without evaluating a body or a constant.
pub(crate) fn literal_length(item: hirsel_core_syntax::Item<'_>, n: NodeId) -> Option<u64> {
    if item.tree.node(n).kind != NodeKind::Literal {
        return None;
    }
    let token = item.tree.node(n).token?;
    let text = item.tree.token_text(item.text, token);
    let suffix = Scalar::ALL
        .into_iter()
        .find(|&s| hirsel_core_ir::Ty::Scalar(s).int().is_some() && text.ends_with(s.name()));
    let digits = suffix
        .and_then(|s| text.strip_suffix(s.name()))
        .unwrap_or(text);
    let (radix, digits) = if let Some(d) = digits.strip_prefix("0x") {
        (16u64, d)
    } else if let Some(d) = digits.strip_prefix("0o") {
        (8, d)
    } else if let Some(d) = digits.strip_prefix("0b") {
        (2, d)
    } else {
        (10, digits)
    };
    let mut seen = false;
    let value = digits
        .chars()
        .filter(|&c| c != '_')
        .try_fold(0u64, |n, c| {
            seen = true;
            n.checked_mul(radix)?
                .checked_add(u64::from(c.to_digit(radix as u32)?))
        })?;
    if !seen {
        return None;
    }
    if let Some(suffix) = suffix {
        let (signed, width) = hirsel_core_ir::Ty::Scalar(suffix).int().unwrap();
        let bits = width - u32::from(signed);
        let max = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        if value > max {
            return None;
        }
    }
    Some(value)
}

/// Region roots of a type. The caller supplies and reuses the output arena.
pub fn collect_roots(t: &impl TyRead, ty: TyId, out: &mut Vec<TyId>) {
    match t.kind(ty) {
        TyKind::Region(_) => {
            if !out.contains(&ty) {
                out.push(ty);
            }
        }
        TyKind::Anchored(x, r) => {
            collect_roots(t, *x, out);
            collect_roots(t, *r, out);
        }
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => {
            for &x in xs {
                collect_roots(t, x, out);
            }
        }
        TyKind::View(x) | TyKind::Array(x, _) => collect_roots(t, *x, out),
        _ => {}
    }
}
impl FnDecl {
    pub fn outlives(&self, a: TyId, b: TyId) -> bool {
        a == b || self.outlives.contains(&(a, b))
    }
}

fn written_missing_anchor(p: &Program<'_>, at: ItemRef, n: NodeId) -> bool {
    let item = p.item(at);
    let kids = item.tree.children(n);
    match item.tree.node(n).kind {
        NodeKind::Anchor => false,
        NodeKind::TupleType | NodeKind::GroupType => {
            kids.iter().any(|&c| written_missing_anchor(p, at, c))
        }
        NodeKind::TypeArguments => {
            if let Some(Target::Definition(d)) = p.target(at, kids[0]) {
                if let Some(Def::Type(d)) = p.by_definition[d.0 as usize] {
                    let decl = &p.decls[d.index()];
                    if decl.structural {
                        return kids[1..].iter().any(|&c| written_missing_anchor(p, at, c));
                    }
                    if decl.repr == Repr::Scoped
                        || decl.role.is_some_and(|r| r.kind() == Kind::Scoped)
                    {
                        return !kids[1..].iter().any(|&c| {
                            item.tree.node(c).kind == NodeKind::Anchor
                                && item.tree.children(c).len() == 1
                        });
                    }
                }
            }
            false
        }
        NodeKind::TypePath => match p.target(at, n) {
            Some(Target::Definition(d)) => match p.by_definition[d.0 as usize] {
                Some(Def::Type(d)) => {
                    let decl = &p.decls[d.index()];
                    decl.repr == Repr::Scoped || decl.role.is_some_and(|r| r.kind() == Kind::Scoped)
                }
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
}

pub(crate) fn scoped_payload(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::View(_) | TyKind::Fn(_, _, _, Callable::Scoped) => true,
        TyKind::Tuple(xs) => xs.iter().any(|&x| scoped_payload(decls, t, x)),
        TyKind::Adt(d, xs) => {
            let decl = &decls[d.index()];
            decl.repr == Repr::Scoped
                || decl.role.is_some_and(|r| r.kind() == Kind::Scoped)
                || decl.structural && xs.iter().any(|&x| scoped_payload(decls, t, x))
        }
        _ => false,
    }
}

/// A bare stable parameter reads the caller's value in its original region (MEM-6).
/// This conversion is only for parameter reads, never for storing or returning it.
pub(crate) fn stable_parameter(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    actual: TyId,
    expected: TyId,
) -> bool {
    let actual = match t.kind(actual) {
        TyKind::Anchored(x, _) => *x,
        _ => actual,
    };
    match (t.kind(actual), t.kind(expected)) {
        (TyKind::Adt(a, xs), TyKind::Adt(b, ys)) => {
            xs == ys
                && ((a == b
                    && (decls[b.index()].repr == Repr::Stable
                        || decls[b.index()]
                            .role
                            .is_some_and(|r| r.kind() == Kind::Stable)))
                    || decls[a.index()].role == Some(Role::IndexedSpan)
                        && decls[b.index()].role == Some(Role::SeqSpan))
        }

        _ => false,
    }
}

/// Read a published encoding summary and its conditions. No field is reread here.
fn encoding_bounds(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    params: &[u16],
) -> u16 {
    let all = ENCODED | FIXED_ENCODING;
    match t.kind(ty) {
        TyKind::Scalar(_) | TyKind::Error | TyKind::Hole => all,
        TyKind::Anchored(x, _) => encoding_bounds(decls, t, *x, params),
        TyKind::Param(i) => params.get(*i as usize).copied().unwrap_or(0) & all,
        TyKind::Tuple(xs) if !xs.is_empty() && kind(decls, t, ty, params) == Kind::Plain => xs
            .iter()
            .fold(all, |b, &x| b & encoding_bounds(decls, t, x, params)),
        TyKind::Adt(d, args) => {
            let decl = &decls[d.index()];
            match decl.role {
                Some(Role::Text) => ENCODED,
                Some(Role::FixedSpan) => {
                    if encoding_bounds(decls, t, args[0], params) & FIXED_ENCODING != 0 {
                        ENCODED
                    } else {
                        0
                    }
                }
                Some(Role::SeqSpan | Role::IndexedSpan) => {
                    encoding_bounds(decls, t, args[0], params) & ENCODED
                }
                Some(_) => 0,
                None => {
                    if decl.compact.is_none() && kind(decls, t, ty, params) != Kind::Plain {
                        return 0;
                    }
                    let mut bits = decl.summary.encoding;
                    for (i, bit) in [PACKED, STREAMABLE, FIXED_ENCODING].into_iter().enumerate() {
                        if decl.summary.encoding_params[i].iter().any(|&p| {
                            encoding_bounds(decls, t, args[p as usize], params) & bit == 0
                        }) {
                            bits &= !bit;
                        }
                    }
                    bits
                }
            }
        }
        _ => 0,
    }
}

fn derive_encoding(
    decls: &crate::Rows<TypeDecl>,
    t: &mut Types,
    ty: TyId,
    params: &[u16],
    stack: &mut Vec<(DeclId, Vec<TyId>)>,
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
                    if let Some((_, previous)) = stack.iter().find(|(id, _)| *id == d) {
                        return if previous == &args { all } else { 0 };
                    }
                    stack.push((d, args.clone()));
                    let mut bits = all;
                    for f in decl.all_fields() {
                        let ft = subst(t, f.ty, &args);
                        bits &= derive_encoding(decls, t, ft, params, stack, compact);
                    }
                    stack.pop();
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

fn stable_field(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> bool {
    match t.kind(ty) {
        TyKind::Tuple(xs) => xs.iter().all(|&x| stable_field(decls, t, x, params)),
        TyKind::Adt(d, xs) if is_core_prelude(&decls[d.index()].path, &["Option"]) => {
            xs.iter().all(|&x| stable_field(decls, t, x, params))
        }
        _ => matches!(kind(decls, t, ty, params), Kind::Plain | Kind::Stable),
    }
}
fn unrooted_stable(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Anchored(_, _) => false,
        TyKind::Tuple(xs) => xs.iter().any(|&x| unrooted_stable(decls, t, x)),
        TyKind::Adt(d, xs) => {
            decls[d.index()].repr == Repr::Stable
                || decls[d.index()]
                    .role
                    .is_some_and(|r| r.kind() == Kind::Stable)
                || decls[d.index()]
                    .summary
                    .bound_params
                    .iter()
                    .any(|&i| unrooted_stable(decls, t, xs[i as usize]))
        }
        TyKind::Array(x, _) => unrooted_stable(decls, t, *x),
        _ => false,
    }
}

/// Read the callable kind and enforce LANG-13's owning bracket at its bound name.
pub(crate) fn callable_kind(
    item: hirsel_core_syntax::Item<'_>,
    n: NodeId,
) -> Result<Callable, NodeId> {
    Ok(match item.tree.node(n).kind {
        NodeKind::ScopedCallableType => Callable::Scoped,
        NodeKind::OwnCallableType => {
            if let Some(&bounds) = item
                .tree
                .children(n)
                .iter()
                .find(|&&c| item.tree.node(c).kind == NodeKind::Bounds)
            {
                let bs = item.tree.children(bounds);
                for (i, &bound) in bs.iter().enumerate() {
                    if i != 0
                        || item
                            .tree
                            .token_text(item.text, item.tree.node(bound).token.unwrap())
                            != "send"
                    {
                        return Err(bound);
                    }
                }
                Callable::OwnSend
            } else {
                Callable::Own
            }
        }
        _ => Callable::Plain,
    })
}

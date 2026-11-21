//! Declarations: one published summary per type declaration (OWN-2), computed once in
//! dependency order, and one checked signature per function (LANG-8). No body is read.

use crate::types::*;
use crate::{Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Recipe, Scalar, Width};
use hirsel_core_resolve::{DefinitionId, Site, Target};
use hirsel_core_syntax::{NodeId, NodeKind, Symbol};

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
/// A declared conditional codec promise (OWN-4, SEM-10).
pub const CODEC: u16 = 1024;
/// An empty holds set (OWN-2, OWN-4): computed like `plain`, never declared on a type.
/// `codec`, `integer` and `float` imply it through [`param_holds`].
pub const INERT: u16 = 2048;
/// May move to another unit (CONC-2); every `cross` type is `send`.
pub const CROSS: u16 = 4096;
/// A declared bound set closed under implication (OWN-4): `integer` and `float` imply
/// every bound all their admitted types have, and `streamable` implies `packed`.
pub fn implied(mut b: u16) -> u16 {
    if b & INTEGER != 0 {
        b |= PLAIN | SHAREABLE | SEND | CROSS | KEY | ORDERED | ENCODED | CODEC;
    }
    if b & FLOAT != 0 {
        b |= PLAIN | SHAREABLE | SEND | CROSS | ENCODED | CODEC;
    }
    if b & CROSS != 0 {
        b |= SEND;
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
        "cross" => CROSS,
        "key" => KEY,
        "integer" => INTEGER,
        "float" => FLOAT,
        "packed" => PACKED,
        "streamable" => STREAMABLE,
        "codec" => CODEC,
        "inert" => INERT,
        _ => ORDERED,
    }
}
/// A count with its unit as a compared fact (PROG-7): `1 type argument`, `2 arguments`.
pub(crate) fn counted(k: usize, unit: &str) -> String {
    format!("{k} {unit}{}", if k == 1 { "" } else { "s" })
}

pub fn bound_names(b: u16) -> String {
    let names = [
        (PLAIN, "plain"),
        (SHAREABLE, "shareable"),
        (SEND, "send"),
        (CROSS, "cross"),
        (KEY, "key"),
        (ORDERED, "ordered"),
        (INTEGER, "integer"),
        (FLOAT, "float"),
        (PACKED, "packed"),
        (STREAMABLE, "streamable"),
        (CODEC, "codec"),
        (INERT, "inert"),
    ];
    names
        .into_iter()
        .filter(|(bit, _)| b & bit != 0)
        .map(|(_, name)| name)
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Holds bits (OWN-2).
pub const H_RESOURCE: u8 = 1;
pub const H_CAPABILITY: u8 = 2;
pub const H_SCOPED: u8 = 4;
pub const H_REGION: u8 = 8;
/// An owner without a copy contract (OWN-2).
pub const H_OPAQUE: u8 = 16;
/// A foreign handle outside its owning resource (OWN-2 and FFI-6).
pub const H_RAW_HANDLE: u8 = 32;

/// What a rigid type parameter may hold (OWN-2): every category its bounds do not
/// exclude. `plain` and `shareable` exclude resources, scoped values and region
/// references, and `shareable` also excludes raw handles. `inert` and `codec` exclude
/// every held category.
pub fn param_holds(bounds: u16) -> u8 {
    let all = H_RESOURCE | H_CAPABILITY | H_SCOPED | H_REGION | H_OPAQUE | H_RAW_HANDLE;
    if bounds & (CODEC | INERT) != 0 {
        0
    } else if bounds & SHAREABLE != 0 {
        H_CAPABILITY
    } else if bounds & (PLAIN | SHAREABLE) != 0 {
        all & !(H_RESOURCE | H_SCOPED | H_REGION | H_OPAQUE)
    } else {
        all
    }
}

/// A kind (OWN-3). `Generic` is a rigid type parameter without the `plain` bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Plain,
    Own,
    Region,
    Resource,
    Counted,
    Scoped,
    Generic,
}

impl Kind {
    /// The kind as a compared fact of a kind refusal (PROG-7).
    pub fn keyword(self) -> &'static str {
        match self {
            Kind::Plain => "plain",
            Kind::Own => "own",
            Kind::Region => "region",
            Kind::Resource => "resource",
            Kind::Counted => "counted",
            Kind::Scoped => "scoped",
            Kind::Generic => "type parameter",
        }
    }
}

/// What a declaration's keyword says (OWN-1).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Repr {
    Own,
    Region,
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
    Latest,
    Client,
    Inbox,
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
    Log,
    StaticFn,
    Socket,
    Flow,
    FlowEndpoint,
    FrozenBytes,
    ForeignHandle,
}
impl Role {
    fn of(path: &DefPath) -> Option<Role> {
        if path.package_is("core", "1") && path.0.modules == ["secret"] && path.0.name == "Scram" {
            return Some(Role::HostFile);
        }
        if path.is_core("foreign.Ptr") {
            return Some(Role::ForeignHandle);
        }
        if !is_core_prelude(path, &[]) {
            return None;
        }
        Some(match path.0.name.as_str() {
            "View" => Role::View,
            "Box" => Role::Box,
            "Rc" => Role::Rc,
            "Arc" => Role::Arc,
            "Latest" => Role::Latest,
            "Client" => Role::Client,
            "Inbox" => Role::Inbox,
            "List" => Role::List,
            "String" => Role::String,
            "FrozenBytes" => Role::FrozenBytes,
            "Text" => Role::Text,
            "Map" => Role::Map,
            "Region" => Role::Region,
            "Task" => Role::Task,
            "Sender" => Role::Sender,
            "Receiver" => Role::Receiver,
            "HostFile" | "HostEntries" | "ClockHand" | "StopTrigger" | "Served" | "Call" => {
                Role::HostFile
            }
            "Socket" | "ReadHalf" | "WriteHalf" | "Listener" => Role::Socket,
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
            "Log" => Role::Log,
            "StaticFn" => Role::StaticFn,
            "Flow" => Role::Flow,
            "Into" | "Port" | "Sink" => Role::FlowEndpoint,

            "Dir" | "Net" | "Clock" | "Random" | "Stdout" | "Stderr" | "Stdin" | "Env"
            | "Process" | "Tasks" | "Secret" | "Foreign" => Role::Capability,
            _ => return None,
        })
    }
    pub fn kind(self) -> Kind {
        match self {
            Role::Box
            | Role::List
            | Role::String
            | Role::FrozenBytes
            | Role::Map
            | Role::SpanBuilder
            | Role::Builder
            | Role::Causes
            | Role::Log => Kind::Own,
            Role::Rc | Role::Arc => Kind::Counted,
            Role::View | Role::Region | Role::Dest | Role::Writer => Kind::Scoped,
            Role::Text | Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan | Role::SeqIndex => {
                Kind::Region
            }
            Role::Capability
            | Role::Args
            | Role::Unique
            | Role::Site
            | Role::Child
            | Role::Link
            | Role::StaticFn
            | Role::FlowEndpoint
            | Role::ForeignHandle => Kind::Plain,
            Role::Task
            | Role::Sender
            | Role::Receiver
            | Role::Socket
            | Role::HostFile
            | Role::Latest
            | Role::Client
            | Role::Inbox
            | Role::Flow => Kind::Resource,
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
    /// Part of an invalid recursive definition, already refused (OWN-2 or LANG-12).
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
    /// The function row of its `clone[..](x: T)` item (MEM-17), declared in any core module.
    pub clone_body: Option<u32>,
    /// `grant bytes n` (MEM-27): the limit its finalizer runs under; 0 if undeclared.
    pub grant: u64,
    pub compact: Option<Width>,
    /// Sparse declaration features share one absent word and one stored option.
    pub extra: Option<Box<DeclExtra>>,
    pub summary: Summary,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DeclExtra {
    /// SEM-10 schema-only declarations, absent from value layout.
    pub codec: Option<Box<crate::codec_decls::CodecDecl>>,
}
impl TypeDecl {
    pub fn codec(&self) -> Option<&crate::codec_decls::CodecDecl> {
        self.extra.as_ref().and_then(|e| e.codec.as_deref())
    }
    pub(crate) fn codec_mut(&mut self) -> &mut crate::codec_decls::CodecDecl {
        self.extra
            .get_or_insert_default()
            .codec
            .get_or_insert_default()
    }
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
    /// PROG-4: only another test function may refer to this function.
    pub is_test: bool,
    pub path: DefPath,
    pub site: Site,
    pub at: ItemRef,
    pub generics: Vec<Generic>,
    pub params: Vec<ParamDecl>,
    /// A value parameter is core Region; root substitution can be skipped otherwise.
    pub has_region_parameters: bool,
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
    /// A core own row's clone body: the `clone(x)` block of this type's item, under
    /// the path `Type.$clone`. Its one parameter is the value, viewed; its result is
    /// `Self`, lowered to `(from, to)`; a refusal raises `Capacity` (MEM-17,
    /// `Shape::clone`).
    #[serde(default)]
    pub clone_of: Option<DeclId>,
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
    /// A type argument of a generic other than Option and Result is not scoped (OWN-19);
    /// a `Log`'s element holds no view or scoped value (MEM-29).
    NotScoped {
        ty: TyId,
        site: Site,
        log: bool,
    },
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
    CallableResult {
        ty: TyId,
        at: ItemRef,
        node: NodeId,
    },
    ForeignMarker {
        marker: crate::foreign::Marker,
        ty: TyId,
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
        let mut attributes = rustc_hash::FxHashMap::default();
        let mut members = rustc_hash::FxHashMap::default();
        for (m, module) in self.modules.iter().enumerate().skip(self.library_modules) {
            for (i, item) in module.syntax.items().enumerate() {
                let root = item.tree.node(item.root);
                let Some(name) = root.token else { continue };
                let at = ItemRef {
                    module: m as u32,
                    item: i as u32,
                };
                let name_span = item.token_span(name);
                attributes.clear();
                if root.kind == NodeKind::CloneHook {
                    for &c in item.tree.children(item.root) {
                        if item.tree.node(c).kind == NodeKind::Attribute {
                            self.claim_declaration_name(&mut attributes, at, c, "attribute");
                        }
                    }
                    let site = Site {
                        module: module.id,
                        source: Some(module.source),
                        item: i as u32,
                        span: name_span,
                    };
                    if let Some(definition) =
                        self.index
                            .item_definition(module.id, module.source, i as u32, item)
                    {
                        self.register_clone_hook(at, site, definition);
                    }
                    continue;
                }
                let Some(definition) = self.index.definition(module.id, item.symbol(name)) else {
                    continue;
                };
                let path = self.index.definition_path(definition);
                let site = Site {
                    module: module.id,
                    source: Some(module.source),
                    item: i as u32,
                    span: name_span,
                };
                let mut public = false;
                let mut is_test = false;
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
                                if bit & (PLAIN | INERT | ENCODED) != 0 {
                                    self.refusals.push(refusal(
                                        "OWN-4",
                                        self.site(at, b),
                                        "plain, inert, packed and streamable are computed, never declared",
                                    ));
                                }
                                if bit == CODEC
                                    && (compact.is_some()
                                        || matches!(
                                            root.kind,
                                            NodeKind::ResourceType | NodeKind::ScopedType
                                        ))
                                {
                                    self.refusals.push(refusal("OWN-4", self.site(at, b),
                                        "a resource, region or scoped definition cannot declare codec"));
                                }
                                declared |= implied(bit);
                            }
                        }
                        NodeKind::Drop => drop = Some(c),
                        NodeKind::Literal
                            if matches!(
                                root.kind,
                                NodeKind::TypeDecl | NodeKind::ResourceType | NodeKind::ScopedType
                            ) =>
                        {
                            grant = literal_length(item, c).unwrap_or_else(|| {
                                self.refusals.push(refusal(
                                    "LANG-3",
                                    Site {
                                        span: item.token_span(n.token.unwrap()),
                                        ..self.site(at, c)
                                    },
                                    "a grant is an integer literal fitting u64",
                                ));
                                0
                            });
                        }
                        NodeKind::Fields | NodeKind::Variants => {
                            members.clear();
                            let part = if n.kind == NodeKind::Fields {
                                "field"
                            } else {
                                "variant"
                            };
                            for &member in item.tree.children(c) {
                                if !matches!(
                                    item.tree.node(member).kind,
                                    NodeKind::RemovedField | NodeKind::RemovedVariant
                                ) {
                                    self.claim_declaration_name(&mut members, at, member, part);
                                }
                            }
                        }
                        NodeKind::Attribute => {
                            self.claim_declaration_name(&mut attributes, at, c, "attribute");
                            let t = n.token.unwrap();
                            is_test |= item.tree.token_text(item.text, t) == "test";
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
                            is_test,
                            path: path.clone(),
                            result_site: site,
                            site,
                            at,
                            generics,
                            params: vec![],
                            has_region_parameters: false,
                            result: UNIT,
                            callable: ERROR,
                            anchors: vec![],
                            region_anchors: vec![],
                            outlives: vec![],
                            waits: false,
                            body,
                            drop_of: None,
                            clone_of: None,
                        });
                        self.by_path.entry(path).or_insert(Def::Fn(id));
                        self.by_definition[definition.0 as usize] = Some(Def::Fn(id));
                        continue;
                    }
                    _ => continue,
                };
                if drop.is_some() && repr != Repr::Resource && !path.package_is("core", "1") {
                    self.refusals.push(refusal(
                        "FAIL-11",
                        site,
                        "only core types may declare a finalizer on an own row",
                    ));
                }
                let id = DeclId(self.decls.len() as u32);
                let finalizer = drop.map(|d| {
                    let f = self.fns.len() as u32;
                    let body = item.tree.children(d).last().copied();
                    self.fns.push(FnDecl {
                        is_test: false,
                        path: path.clone(),
                        result_site: site,
                        site: self.site(at, d),
                        at,
                        generics: generics.clone(),
                        params: vec![],
                        has_region_parameters: false,
                        result: UNIT,
                        callable: ERROR,
                        anchors: vec![],
                        region_anchors: vec![],
                        outlives: vec![],
                        waits: true,
                        body,
                        drop_of: Some(id),
                        clone_of: None,
                    });
                    f
                });
                let role = Role::of(&path);
                let repr = if repr == Repr::Own
                    && (compact.is_some() || role.is_some_and(|r| r.kind() == Kind::Region))
                {
                    Repr::Region
                } else {
                    repr
                };
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
                    clone_body: None,
                    grant,
                    compact,
                    extra: (declared & CODEC != 0).then(|| {
                        Box::new(DeclExtra {
                            codec: Some(Box::default()),
                        })
                    }),
                    summary: Summary::default(),
                });
                self.by_path.entry(path).or_insert(Def::Type(id));
                self.by_definition[definition.0 as usize] = Some(Def::Type(id));
            }
        }
        self.alias_state = vec![Visit::Todo as u8; self.decls.len() - self.decls.base_len()];
    }

    fn claim_declaration_name(
        &mut self,
        names: &mut rustc_hash::FxHashMap<Symbol, NodeId>,
        at: ItemRef,
        node: NodeId,
        part: &str,
    ) {
        let item = self.item(at);
        let token = item.tree.node(node).token.unwrap();
        match names.entry(item.symbol(token)) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(node);
            }
            std::collections::hash_map::Entry::Occupied(e) => {
                let mut r = refusal(
                    "LANG-7",
                    self.site(at, node),
                    format!(
                        "duplicate {part} `{}`",
                        item.tree.token_text(item.text, token)
                    ),
                );
                r.causal.push(self.site(at, *e.get()));
                self.refusals.push(r);
            }
        }
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
            let at = self.fns[f].at;
            let item = self.item(at);
            if item.tree.node(item.root).kind == NodeKind::CloneHook {
                self.clone_hook(f);
            }
        }
        self.bind_codec_makers();
    }

    /// MEM-17: `clone[..](x: T[..]) { .. }` is a core item, so a core type's copy is
    /// written in the module holding its storage kernels. Its type is attached in
    /// `clone_hook`, once the parameter's type is lowered.
    /// The body's identity is its index definition, `<module>/<Type>.$clone`.
    fn register_clone_hook(&mut self, at: ItemRef, site: Site, definition: DefinitionId) {
        let path = self.index.definition_path(definition);
        if !path.package_is("core", "1") {
            self.refusals.push(refusal(
                "MEM-17",
                site,
                "only core types may declare a clone body",
            ));
            return;
        }
        let item = self.item(at);
        let kids = item.tree.children(item.root);
        let generics_node = kids
            .iter()
            .copied()
            .find(|&c| item.tree.node(c).kind == NodeKind::Generics);
        let body = kids
            .iter()
            .copied()
            .find(|&c| item.tree.node(c).kind == NodeKind::Block);
        let generics = generics_node.map_or(vec![], |g| self.generics(at, g));
        self.fns.push(FnDecl {
            is_test: false,
            path,
            result_site: site,
            site,
            at,
            generics,
            params: vec![],
            has_region_parameters: false,
            result: UNIT,
            callable: ERROR,
            anchors: vec![],
            region_anchors: vec![],
            outlives: vec![],
            waits: false,
            body,
            drop_of: None,
            clone_of: None,
        });
        self.by_definition[definition.0 as usize] = Some(Def::Fn(self.fns.len() as u32 - 1));
    }

    /// MEM-17: the clone body takes one view of a core own type `T[P..]`, generic in
    /// exactly `T`'s parameters with no stronger bounds (it runs for every instance that
    /// may be cloned), and returns `T[P..]`. A type has one.
    fn clone_hook(&mut self, f: usize) {
        let fd = &self.fns[f];
        let n = fd.generics.len();
        let target = match fd.params.as_slice() {
            [p] if p.mode == Mode::View && fd.result == UNIT && !fd.waits => {
                match self.types.kind(p.ty) {
                    TyKind::Adt(d, args)
                        if args.len() == n
                            && args.iter().enumerate().all(|(i, &a)| {
                                matches!(self.types.kind(a), TyKind::Param(j) if *j as usize == i)
                            }) =>
                    {
                        Some(*d)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let target = target.filter(|d| {
            let t = &self.decls[d.index()];
            d.index() >= self.decls.base_len()
                && t.repr == Repr::Own
                && t.generics.len() == n
                && t.generics
                    .iter()
                    .zip(&fd.generics)
                    .all(|(t, h)| t.region == h.region && h.bounds & !t.bounds == 0)
        });
        let site = fd.site;
        let Some(d) = target else {
            self.refusals.push(refusal(
                "MEM-17",
                site,
                "a clone body takes one view of a core own type, generic in exactly its parameters with no stronger bounds, and writes no result",
            ));
            return;
        };
        if let Some(first) = self.decls[d.index()].clone_body {
            let mut r = refusal("MEM-17", site, "a type has one clone body");
            r.causal.push(self.fns[first as usize].site);
            self.refusals.push(r);
            return;
        }
        let ty = self.fns[f].params[0].ty;
        let result = ty;
        let callable = self.types.intern(TyKind::Fn(
            vec![(Mode::View, ty)],
            result,
            false,
            Callable::Plain,
        ));
        let fd = &mut self.fns[f];
        fd.result = result;
        fd.callable = callable;
        fd.clone_of = Some(d);
        self.decls[d.index()].clone_body = Some(f as u32);
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
                NodeKind::CodecVia => {
                    let children = item.tree.children(c);
                    let ty = self.lower_ty(at, children[0], &generics);
                    self.decls[d.index()].codec_mut().via =
                        Some(crate::codec_decls::CodecVia { ty, node: c });
                }
                NodeKind::Fields => {
                    let mut fields = vec![];
                    for (position, &f) in item.tree.children(c).iter().enumerate() {
                        let fnode = item.tree.node(f);
                        let name = item
                            .tree
                            .token_text(item.text, fnode.token.unwrap())
                            .to_owned();
                        let tnode = item.tree.children(f)[0];
                        let ty = self.lower_ty(at, tnode, &generics);
                        let ty = self.outside_vector(at, tnode, ty, "a field");
                        let ty = if self.decls[d.index()].repr == Repr::Region {
                            self.default_field_root(ty, &generics)
                        } else {
                            ty
                        };
                        let field = FieldDecl {
                            name,
                            ty,
                            site: self.site(at, f),
                        };
                        if fnode.kind == NodeKind::RemovedField {
                            let site = field.site;
                            self.decls[d.index()].codec_mut().removed.push(
                                crate::codec_decls::RemovedDecl {
                                    name: field.name.clone(),
                                    position: position as u32,
                                    variant: false,
                                    fields: vec![field],
                                    site,
                                },
                            );
                        } else {
                            fields.push(field);
                        }
                        if let Some(&default) = item.tree.children(f).get(1) {
                            if fnode.kind == NodeKind::RemovedField {
                                self.refusals.push(refusal(
                                    "SEM-10",
                                    self.site(at, default),
                                    "a removed field has no default",
                                ));
                            } else if self.decls[d.index()].declared & CODEC == 0 {
                                self.refusals.push(refusal(
                                    "SEM-10",
                                    self.site(at, default),
                                    "a field default requires a codec declaration",
                                ));
                            } else {
                                self.check_field_default(at, default, ty);
                            }
                        }
                    }
                    body = DeclBody::Record(fields);
                }
                NodeKind::Variants => {
                    let mut cases = vec![];
                    for (position, &v) in item.tree.children(c).iter().enumerate() {
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
                            let ty = self.outside_vector(at, tnode, ty, "a field");
                            let ty = if self.decls[d.index()].repr == Repr::Region {
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
                        if vnode.kind == NodeKind::RemovedVariant {
                            let site = self.site(at, v);
                            self.decls[d.index()].codec_mut().removed.push(
                                crate::codec_decls::RemovedDecl {
                                    name,
                                    position: position as u32,
                                    variant: true,
                                    fields,
                                    site,
                                },
                            );
                        } else {
                            cases.push(CaseDecl {
                                name,
                                labelled,
                                fields,
                            });
                        }
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
                let decl = &self.decls[d.index()];
                decl.repr == Repr::Region || decl.role.is_some_and(|r| r.kind() == Kind::Region)
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
                    .collect::<Vec<_>>();
                if xs.contains(&ERROR) {
                    return ERROR;
                }
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
        self.lower_ty_result(at, n, generics, false)
    }

    pub(crate) fn scalar_vector(
        &mut self,
        at: ItemRef,
        n: NodeId,
        ty: TyId,
        count: Option<u64>,
    ) -> TyId {
        match vector_shape(&self.types, ty, count) {
            Ok(shape) => self.types.intern(TyKind::Vector(shape)),
            Err(message) => {
                if ty != ERROR {
                    self.refusals
                        .push(refusal("SEM-11", self.site(at, n), message));
                }
                ERROR
            }
        }
    }

    pub(crate) fn outside_vector(
        &mut self,
        at: ItemRef,
        n: NodeId,
        ty: TyId,
        context: &'static str,
    ) -> TyId {
        if matches!(self.types.kind(ty), TyKind::Vector(_)) {
            self.refusals.push(refusal(
                "SEM-11",
                self.site(at, n),
                format!("a vector cannot be {context}"),
            ));
            ERROR
        } else {
            ty
        }
    }

    fn lower_ty_result(
        &mut self,
        at: ItemRef,
        n: NodeId,
        generics: &[Generic],
        scoped_roots: bool,
    ) -> TyId {
        let item = self.item(at);
        let node = item.tree.node(n);
        let children = item.tree.children(n);
        match node.kind {
            NodeKind::UnitType => UNIT,
            NodeKind::GroupType => self.lower_ty_result(at, children[0], generics, scoped_roots),
            NodeKind::TupleType => {
                let xs = children
                    .iter()
                    .map(|&c| {
                        let ty = self.lower_ty_result(at, c, generics, scoped_roots);
                        self.outside_vector(at, c, ty, "a tuple element")
                    })
                    .collect::<Vec<_>>();
                if xs.contains(&ERROR) {
                    return ERROR;
                }
                self.types.intern(TyKind::Tuple(xs))
            }
            NodeKind::TypePath => self.named_ty(at, n, &[], generics),
            NodeKind::TypeArguments
                if matches!(
                    self.target(at, children[0]),
                    Some(Target::Builtin(hirsel_core_syntax::sym::SIMD))
                ) =>
            {
                if children.len() != 3 {
                    self.refusals.push(refusal(
                        "SEM-11",
                        self.site(at, n),
                        "Simd takes an integer lane type and a literal lane count",
                    ));
                    return ERROR;
                }
                let count = literal_length(item, children[2]);
                let lane = self.lower_ty_result(at, children[1], generics, scoped_roots);
                self.scalar_vector(at, n, lane, count)
            }
            NodeKind::TypeArguments if matches!(self.target(at, children[0]), Some(Target::Builtin(name)) if self.index.symbols.text(*name) == "Inline") =>
            {
                if children.len() != 3 {
                    self.refusals.push(
                        refusal(
                            "LANG-12",
                            self.site(at, n),
                            "Inline takes an element type and a literal length",
                        )
                        .with_comparison(
                            counted(children.len() - 1, "type argument"),
                            counted(2, "type argument"),
                        ),
                    );
                    return ERROR;
                }
                let element = self.lower_ty_result(at, children[1], generics, scoped_roots);
                let element = self.outside_vector(at, children[1], element, "an array element");
                if element == ERROR {
                    return ERROR;
                }
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
                let mut roots = vec![];
                for &c in &children[1..] {
                    let pending = self.pending.len();
                    let t = self.lower_ty_result(at, c, generics, scoped_roots);
                    let t = self.outside_vector(at, c, t, "a type argument");
                    if item.tree.node(c).kind == NodeKind::Anchor {
                        roots.extend(pending..self.pending.len());
                    }
                    args.push((t, self.site(at, c)));
                }
                if args.iter().any(|&(t, _)| t == ERROR) {
                    return ERROR;
                }
                let mut ty = self.named_ty(at, children[0], &args, generics);
                // `R[in a, in b]`: each bare anchor roots the named type itself (OWN-20).
                for i in roots {
                    if let Pending::Root { base, .. } = &mut self.pending[i]
                        && *base == ERROR
                    {
                        *base = ty;
                    }
                }
                if scoped_roots && scoped_payload(&self.decls, &self.types, ty) {
                    for &(root, _) in &args {
                        if matches!(self.types.kind(root), TyKind::Region(_)) {
                            ty = self.types.intern(TyKind::Anchored(ty, root));
                        }
                    }
                }
                ty
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
                let ty = self.lower_ty_result(at, children[0], generics, scoped_roots);
                let ty = self.outside_vector(at, children[0], ty, "anchored to a region");
                let root = self.root_ty(at, children[1], generics);
                self.pending.push(Pending::Root {
                    base: ty,
                    root,
                    at,
                    site: self.site(at, children[1]),
                });
                if !scoped_roots && scoped_payload(&self.decls, &self.types, ty) {
                    ty
                } else {
                    self.types.intern(TyKind::Anchored(ty, root))
                }
            }
            NodeKind::CallableType | NodeKind::OwnCallableType | NodeKind::ScopedCallableType => {
                let callable = match callable_kind(item, n) {
                    Ok(k) => k,
                    Err((bound, message)) => {
                        self.refusals
                            .push(refusal("LANG-13", self.site(at, bound), message));
                        return ERROR;
                    }
                };
                let mut params = vec![];
                let mut result = UNIT;
                let mut waits = false;
                for &c in children {
                    match item.tree.node(c).kind {
                        NodeKind::Parameters => {
                            for &p in item.tree.children(c) {
                                let mode = callable_mode_of(item.tree.node(p).kind);
                                let ty_node = item.tree.children(p)[0];
                                let t = self.lower_ty(at, ty_node, generics);
                                let t =
                                    self.outside_vector(at, ty_node, t, "part of a callable type");
                                if mode == Mode::Bytes && t != scalar(Scalar::U64) && t != ERROR {
                                    self.refusals.push(refusal(
                                        "LANG-5",
                                        self.site(at, p),
                                        "a `bytes` parameter has type `u64`",
                                    ));
                                }
                                params.push((mode, t));
                            }
                        }
                        NodeKind::ResultType => {
                            let node = item.tree.children(c)[0];
                            result = self.lower_ty_result(at, node, generics, true);
                            result =
                                self.outside_vector(at, node, result, "part of a callable type");
                            if callable == Callable::ScopedView
                                && view_result_reserved(&self.decls, &self.types, result)
                            {
                                self.refusals.push(refusal(
                                    "LANG-13",
                                    self.site(at, node),
                                    VIEW_RESULT_RESERVED,
                                ));
                                return ERROR;
                            }
                            if scoped_payload(&self.decls, &self.types, result)
                                && written_missing_anchor(self, at, node)
                            {
                                self.refusals.push(refusal(
                                    "OWN-20",
                                    self.site(at, node),
                                    "a callable's scoped result names a region anchor",
                                ));
                                return ERROR;
                            }
                            self.pending.push(Pending::CallableResult {
                                ty: result,
                                at,
                                node,
                            });
                        }
                        NodeKind::Waits => waits = true,
                        NodeKind::Generics => self.gaps.push(Gap::new(
                            self.site(at, c),
                            "generic callable types are not built",
                        )),
                        _ => {}
                    }
                }
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
                    if name == hirsel_core_syntax::sym::SIMD {
                        self.refusals.push(refusal(
                            "SEM-11",
                            site,
                            "Simd takes an integer lane type and a literal lane count",
                        ));
                        return ERROR;
                    }
                    self.gaps
                        .push(Gap::new(site, "inline array types are not built"));
                    return ERROR;
                };
                return self.no_args(scalar(s), args, &site);
            }
            Some(Target::Definition(p)) if self.is_core_never(p) => {
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
        let d = &self.decls[decl.index()];
        let generic_count = d.generics.len();
        if args.len() != generic_count {
            self.refusals.push(
                refusal(
                    "LANG-12",
                    site,
                    format!(
                        "`{}` takes {generic_count} type arguments, {} written",
                        d.path.0.name,
                        args.len()
                    ),
                )
                .with_comparison(
                    counted(args.len(), "type argument"),
                    counted(generic_count, "type argument"),
                ),
            );
            return ERROR;
        }
        let arg_tys: Vec<TyId> = args.iter().map(|a| a.0).collect();
        let role = d.role;
        if let Some(marker) = crate::foreign::Marker::of(d)
            && let [ty] = arg_tys.as_slice()
        {
            self.pending.push(Pending::ForeignMarker {
                marker,
                ty: *ty,
                site,
            });
        }
        if !d.structural {
            for (i, (t, s)) in args.iter().enumerate() {
                self.pending.push(Pending::NotScoped {
                    ty: *t,
                    site: *s,
                    log: role == Some(Role::Log),
                });
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
                    self.refusals.push(
                        refusal("LANG-12", *s, "a type parameter takes a type, not a region")
                            .with_comparison("region", "type"),
                    );
                    continue;
                }
                let need = d.generics[i].bounds;
                if need != 0 {
                    self.pending.push(Pending::Bound {
                        ty: *t,
                        bounds: need,
                        params: generics.iter().map(|g| g.bounds).collect(),
                        site: *s,
                        // LANG-12: a written type argument lacking its parameter's bound
                        // (`Rc[HostFile]`) is refused at that argument.
                        rule: if matches!(
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
        let alias = self.is_alias(decl, d);
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
    fn is_alias(&self, d: DeclId, decl: &TypeDecl) -> bool {
        if self.alias_visit(d) == Visit::Done as u8 {
            return matches!(decl.body, DeclBody::Alias(_));
        }
        let item = self.item(decl.at);
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
            self.refusals.push(
                refusal("LANG-12", *site, "this type takes no arguments").with_comparison(
                    counted(args.len(), "type argument"),
                    counted(0, "type argument"),
                ),
            );
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
        let mut callable_result = UNIT;
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
                        let steps = pn.kind == NodeKind::StepsParameter;
                        if steps && ty != scalar(Scalar::U64) && ty != ERROR {
                            self.refusals.push(refusal(
                                "LANG-5",
                                self.site(at, p),
                                "a `steps` parameter has type `u64`",
                            ));
                        }
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
                            mode: if steps { Mode::Steps } else { mode_of(pn.kind) },
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
                    callable_result = self.lower_ty_result(at, t, &generics, true);
                    result = erase_scoped_result(&self.decls, &mut self.types, callable_result);
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
                    format!("anchor `{parts}` is not a view or `mut` parameter"),
                )),
            }
        }
        for &n in &anchor_nodes {
            written_roots.push(self.root_ty(at, n, &generics));
        }
        let callable = self.types.intern(TyKind::Fn(
            params
                .iter()
                .map(|p| (if p.bytes { Mode::Bytes } else { p.mode }, p.ty))
                .collect(),
            callable_result,
            waits,
            Callable::Plain,
        ));
        let callable = crate::types::bind_callable_roots(&mut self.types, callable);
        let has_region_parameters = params.iter().any(|p| {
            matches!(self.types.kind(p.ty), TyKind::Adt(d, _) if self.decls[d.index()].role == Some(Role::Region))
        });
        let fd = &mut self.fns[f];
        fd.has_region_parameters = has_region_parameters;
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
        self.mark_raw_handle_summaries();
        self.check_raw_handle_positions();
        self.check_declared_bounds();
        self.check_codec_declarations();
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            match p {
                Pending::ForeignMarker { marker, ty, site } => {
                    if ty != ERROR
                        && !self
                            .foreign_names()
                            .admits(self, &self.types, marker, ty, site.module)
                    {
                        self.refusals
                            .push(refusal(marker.rule(), site, marker.message()));
                    }
                }
                Pending::Bound {
                    ty,
                    bounds,
                    params,
                    site,
                    rule,
                } => {
                    let have = has_bounds(&self.decls, &self.types, ty, &params);
                    if have & bounds != bounds {
                        let b = bound_names(bounds & !have);
                        self.refusals.push(
                            refusal(rule, site, format!("type argument is not {b}"))
                                .with_comparison(canonical(&self.decls, &self.types, ty), b),
                        );
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
                        } else if scoped && !matches!(p.mode, Mode::View | Mode::Mut) {
                            self.refusals.push(refusal(
                                "OWN-20",
                                fd.result_site,
                                "a scoped anchor names a view, `mut`, or Region parameter",
                            ));
                        }
                    }
                }
                Pending::Array { ty, params, site } => {
                    if crate::foreign::holds_raw_handle(&self.decls, &self.types, ty) {
                        self.refusals.push(refusal(
                            "FFI-6",
                            site,
                            "a raw handle cannot be an Inline element",
                        ));
                        continue;
                    }
                    let element_kind = kind(&self.decls, &self.types, ty, &params);
                    if element_kind == Kind::Scoped {
                        self.refusals.push(
                            refusal("OWN-19", site, "a scoped value is not a collection element")
                                .with_comparison(
                                    canonical(&self.decls, &self.types, ty),
                                    "not scoped",
                                ),
                        );
                    } else if !matches!(element_kind, Kind::Plain | Kind::Own) {
                        self.refusals.push(refusal(
                            "SEM-9",
                            site,
                            "Inline elements must be plain or own; a type parameter must be plain",
                        ));
                    }
                }
                Pending::CallableResult { ty, at, node } => {
                    // Bounds distinguish Plain from Generic, never Scoped.
                    if kind(&self.decls, &self.types, ty, &[]) == Kind::Scoped {
                        let region = |root| {
                            let Some(Target::Binder(b)) = self.target(at, root).copied() else {
                                return false;
                            };
                            let binder = &self.items[at.module as usize][at.item as usize]
                                .resolution
                                .binders[b as usize];
                            binder.kind == hirsel_core_resolve::BinderKind::Region
                                || self.fns.own_iter().find(|f| f.at == at).is_some_and(|f| {
                                    f.params.iter().any(|p| p.name == binder.name
                                        && matches!(self.types.kind(p.ty), TyKind::Adt(d, _) if self.decls[d.index()].role == Some(Role::Region)))
                                })
                        };
                        if written_result_missing_anchor(self, at, node, &region) {
                            self.refusals.push(refusal(
                                "OWN-20",
                                self.site(at, node),
                                "a callable's scoped result names a region anchor",
                            ));
                        }
                    }
                }
                Pending::NotScoped { ty, site, log } => {
                    if crate::foreign::holds_raw_handle(&self.decls, &self.types, ty) {
                        self.refusals.push(refusal(
                            "FFI-6",
                            site,
                            "a raw handle cannot be this type argument",
                        ));
                        continue;
                    }
                    if kind(&self.decls, &self.types, ty, &[]) == Kind::Scoped {
                        let (rule, wanted, message) = if log {
                            (
                                "MEM-29",
                                "no view or scoped value",
                                "a Log's element holds no view or scoped value",
                            )
                        } else {
                            (
                                "OWN-19",
                                "not scoped",
                                "a scoped type is not a type argument of a generic other than Option and Result",
                            )
                        };
                        self.refusals.push(
                            refusal(rule, site, message)
                                .with_comparison(canonical(&self.decls, &self.types, ty), wanted),
                        );
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
                Repr::Region => Some(Kind::Region),
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
            if self.decls[d].codec().is_some() {
                let params = (0..self.decls[d].generics.len())
                    .filter(|&i| {
                        mentions_for_bound(
                            &self.decls,
                            &self.types,
                            DeclId(d as u32),
                            i as u32,
                            &mut vec![],
                            true,
                        )
                    })
                    .map(|i| i as u32)
                    .collect();
                self.decls[d].codec_mut().params = params;
            }
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
                Some(
                    Role::Task
                    | Role::Sender
                    | Role::Receiver
                    | Role::Socket
                    | Role::HostFile
                    | Role::Latest
                    | Role::Client
                    | Role::Inbox
                    | Role::Flow,
                ) => H_RESOURCE,
                Some(Role::View | Role::Region | Role::Dest | Role::Writer) => H_SCOPED,
                Some(
                    Role::Text
                    | Role::FixedSpan
                    | Role::SeqSpan
                    | Role::IndexedSpan
                    | Role::SeqIndex,
                ) => H_REGION,
                // A builder and `Causes` are owners with no copy contract (OWN-2); a
                // builder also holds its region's stable blob.
                Some(Role::SpanBuilder | Role::Builder) => H_REGION | H_OPAQUE,
                Some(Role::Causes | Role::Log) => H_OPAQUE,
                Some(_) => 0,
                None => match decl.repr {
                    Repr::Resource => H_RESOURCE,
                    Repr::Scoped => H_SCOPED,
                    Repr::Region => H_REGION,
                    Repr::Own => 0,
                },
            };
            if decl.role.is_some() {
                // A built-in holds what its arguments hold: `List[T]` holds a resource
                // when `T` does (MEM-17 never clones it, LANG-14 never captures it).
                if decl.role != Some(Role::FlowEndpoint) {
                    params = (1u64 << decl.generics.len()) - 1;
                }
            } else {
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
            let via = decl.codec().is_some_and(|c| c.via.is_some());
            let field_bounds = if via { declared & !CODEC } else { declared };
            let mut out = vec![];
            let forbidden = if decl.repr != Repr::Own { SHAREABLE } else { 0 }
                | if matches!(decl.repr, Repr::Region | Repr::Scoped) {
                    SEND | CROSS
                } else {
                    0
                }
                // SEM-7: resources and scoped types have neither `key` nor `ordered`.
                | if matches!(decl.repr, Repr::Resource | Repr::Scoped) {
                    KEY | ORDERED
                } else {
                    0
                };
            if declared & forbidden != 0 {
                let b = bound_names(declared & forbidden);
                let repr = match decl.repr {
                    Repr::Own => "own",
                    Repr::Region => "region",
                    Repr::Resource => "resource",
                    Repr::Scoped => "scoped",
                };
                out.push(
                    refusal(
                        "OWN-4",
                        decl.site,
                        format!("this representation cannot be {b}"),
                    )
                    .with_comparison(b, format!("{repr} type without {}", bound_names(forbidden))),
                );
            }
            if decl.compact.is_some()
                && (decl.repr != Repr::Region || matches!(decl.body, DeclBody::Alias(_)))
            {
                out.push(refusal(
                    "MEM-9",
                    decl.site,
                    "compact layout applies only to an unmodified record or variant declaration",
                ));
            }
            for f in decl.all_fields() {
                if decl.repr == Repr::Region
                    && !region_field(&self.decls, &self.types, f.ty, &params)
                {
                    let found = kind(&self.decls, &self.types, f.ty, &params).keyword();
                    out.push(
                        refusal(
                            "MEM-9",
                            f.site,
                            "a compact field holds plain data or data in its enclosing blob",
                        )
                        .with_comparison(found, "plain data or region data in the enclosing blob"),
                    );
                } else if decl.repr != Repr::Scoped
                    && kind(&self.decls, &self.types, f.ty, &params) == Kind::Scoped
                {
                    out.push(
                        refusal(
                            "OWN-19",
                            f.site,
                            format!("field `{}` of a non-scoped type is scoped", f.name),
                        )
                        .with_comparison(canonical(&self.decls, &self.types, f.ty), "not scoped"),
                    );
                }
                if decl.repr != Repr::Region && unrooted_region(&self.decls, &self.types, f.ty) {
                    out.push(refusal(
                        "MEM-6",
                        f.site,
                        "a stored region field names its region",
                    ));
                }
                if declared & CODEC != 0
                    && via
                    && !holds_nothing(&self.decls, &self.types, f.ty, &params)
                {
                    out.push(
                        refusal(
                            "OWN-4",
                            f.site,
                            format!("field `{}` of a codec type must hold nothing", f.name),
                        )
                        .with_comparison(
                            canonical(&self.decls, &self.types, f.ty),
                            "empty holds set",
                        ),
                    );
                }
                let have = has_bounds(&self.decls, &self.types, f.ty, &params);
                let missing = field_bounds & !have;
                if missing != 0 {
                    let b = bound_names(missing);
                    let mut r = refusal(
                        if missing & (SEND | CROSS) != 0 {
                            "CONC-2"
                        } else {
                            "OWN-4"
                        },
                        f.site,
                        format!("field `{}` is not {b}", f.name),
                    )
                    .with_comparison(canonical(&self.decls, &self.types, f.ty), b);
                    if missing & CODEC != 0 {
                        for (site, explanation) in
                            missing_codec_declarations(&self.decls, &self.types, f.ty, &params)
                        {
                            r.causal.push(site);
                            r.message += &format!("; {explanation}");
                        }
                    }
                    out.push(r);
                }
            }
            self.refusals.extend(out);
        }
    }

    fn check_signatures(&mut self) {
        for f in self.fns.base_len()..self.fns.len() {
            let fd = &self.fns[f];
            let params: Vec<u16> = fd.generics.iter().map(|g| g.bounds).collect();
            if unrooted_region(&self.decls, &self.types, fd.result) {
                self.refusals.push(refusal(
                    "MEM-6",
                    fd.result_site,
                    "a stable result names its region",
                ));
            }
            for p in &fd.params {
                if p.mode == Mode::Mut
                    && kind(&self.decls, &self.types, p.ty, &params) == Kind::Region
                {
                    self.refusals.push(
                        refusal("MEM-5", p.site, "stable data cannot be mutated")
                            .with_comparison("mut", "view"),
                    );
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

/// The fixed holds set and the rigid parameters that contribute to it. This
/// reads the actual fields' summaries, including for a type with `codec via`.
pub(crate) fn holds_components(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
) -> (u8, u64) {
    match t.kind(ty) {
        TyKind::Param(i) => (0, 1 << i),
        TyKind::Tuple(xs) => xs.iter().fold((0, 0), |(holds, params), &x| {
            let (h, p) = holds_components(decls, t, x);
            (holds | h, params | p)
        }),
        TyKind::Anchored(x, _) | TyKind::Array(x, _) => holds_components(decls, t, *x),
        TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView) => (H_SCOPED, 0),
        TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend) => (H_OPAQUE, 0),
        TyKind::View(x) => {
            let (holds, params) = holds_components(decls, t, *x);
            (holds | H_SCOPED, params)
        }
        TyKind::Region(_) => (H_SCOPED, 0),
        TyKind::Adt(d, xs) => {
            let summary = &decls[d.index()].summary;
            xs.iter()
                .enumerate()
                .filter(|(i, _)| summary.holds_params & (1 << i) != 0)
                .fold((summary.holds, 0), |(holds, params), (_, &x)| {
                    let (h, p) = holds_components(decls, t, x);
                    (holds | h, params | p)
                })
        }
        _ => (0, 0),
    }
}

/// An empty holds set (OWN-2) for `ty`, reading each rigid parameter's bounds. It walks
/// the type directly, so a parameter index past a bit mask's width needs no mask.
pub(crate) fn holds_nothing(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    params: &[u16],
) -> bool {
    match t.kind(ty) {
        TyKind::Param(i) => param_holds(params.get(*i as usize).copied().unwrap_or(0)) == 0,
        TyKind::Tuple(xs) => xs.iter().all(|&x| holds_nothing(decls, t, x, params)),
        TyKind::Anchored(x, _) | TyKind::Array(x, _) => holds_nothing(decls, t, *x, params),
        TyKind::Fn(_, _, _, Callable::Plain) => true,
        TyKind::Fn(..) | TyKind::View(_) | TyKind::Region(_) => false,
        TyKind::Adt(d, xs) => {
            let summary = &decls[d.index()].summary;
            summary.holds == 0
                && xs.iter().enumerate().all(|(i, &x)| {
                    (i < 64 && summary.holds_params & (1 << i) == 0)
                        || holds_nothing(decls, t, x, params)
                })
        }
        _ => true,
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
        TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView) => *h |= H_SCOPED,
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
    mentions_for_bound(decls, t, d, i, seen, false)
}
fn mentions_for_bound(
    decls: &crate::Rows<TypeDecl>,
    t: &Types,
    d: DeclId,
    i: u32,
    seen: &mut Vec<(DeclId, u32)>,
    codec: bool,
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
        codec: bool,
    ) -> bool {
        match t.kind(ty) {
            TyKind::Param(p) => *p == i,
            TyKind::Tuple(xs) => xs.iter().any(|&x| ty_mentions(decls, t, x, i, seen, codec)),
            TyKind::Anchored(x, _) | TyKind::View(x) | TyKind::Array(x, _) => {
                ty_mentions(decls, t, *x, i, seen, codec)
            }
            TyKind::Adt(d, xs) => xs.iter().enumerate().any(|(p, &x)| {
                ty_mentions(decls, t, x, i, seen, codec)
                    && mentions_for_bound(decls, t, *d, p as u32, &mut seen.clone(), codec)
            }),
            _ => false,
        }
    }
    if codec && let Some(schema) = decl.codec() {
        if let Some(via) = &schema.via {
            return ty_mentions(decls, t, via.ty, i, seen, codec);
        }
        if schema
            .removed
            .iter()
            .flat_map(|r| &r.fields)
            .any(|f| ty_mentions(decls, t, f.ty, i, seen, codec))
        {
            return true;
        }
    }
    match decl.body {
        DeclBody::Alias(ty) => ty_mentions(decls, t, ty, i, seen, codec),
        _ => decl
            .all_fields()
            .any(|f| ty_mentions(decls, t, f.ty, i, seen, codec)),
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
    kind_row(decls, t, t.kind(ty), params)
}

pub(crate) fn vector_shape(
    types: &impl TyRead,
    lane: TyId,
    count: Option<u64>,
) -> Result<hirsel_core_layout::Vector, &'static str> {
    let TyKind::Scalar(lane) = *types.kind(lane) else {
        return Err("a vector lane must be an integer type");
    };
    let count = count.ok_or("a vector lane count must be an integer literal")?;
    hirsel_core_layout::Vector::new(lane, count)
        .ok_or("a vector must have integer lanes and occupy 16 or 32 bytes")
}

fn kind_row(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, row: &TyKind, params: &[u16]) -> Kind {
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
        TyKind::Tuple(xs) => xs
            .iter()
            .fold(Kind::Plain, |k, &x| join(k, kind(decls, t, x, params))),
        TyKind::Region(_) => Kind::Scoped,
        TyKind::Anchored(x, _) => {
            let k = kind(decls, t, *x, params);
            if k == Kind::Generic { Kind::Region } else { k }
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
    let row = t.kind(ty);
    match kind_row(decls, t, row, params) {
        Kind::Plain | Kind::Region => true,
        Kind::Scoped => match row {
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

/// The nominal declarations that prevent this field type from satisfying `codec`.
fn missing_codec_declarations(
    decls: &crate::Rows<TypeDecl>,
    types: &impl TyRead,
    ty: TyId,
    params: &[u16],
) -> Vec<(Site, String)> {
    fn visit(
        decls: &crate::Rows<TypeDecl>,
        types: &impl TyRead,
        ty: TyId,
        params: &[u16],
        out: &mut Vec<(Site, String)>,
    ) {
        if has_bounds(decls, types, ty, params) & CODEC != 0 {
            return;
        }
        match types.kind(ty) {
            TyKind::Adt(d, args) => {
                let decl = &decls[d.index()];
                if decl.role.is_none() && !decl.structural && decl.declared & CODEC == 0 {
                    if !out.iter().any(|(s, _)| *s == decl.site) {
                        let name = &decl.path.0.name;
                        let explanation = if decl.repr == Repr::Own {
                            format!("`{name}` is missing `: codec` at its declaration")
                        } else {
                            format!(
                                "`{name}` has a {} representation, which cannot be codec",
                                match decl.repr {
                                    Repr::Own => "own",
                                    Repr::Region => "region",
                                    Repr::Resource => "resource",
                                    Repr::Scoped => "scoped",
                                }
                            )
                        };
                        out.push((decl.site, explanation));
                    }
                } else {
                    for (i, &arg) in args.iter().enumerate() {
                        if decl.structural
                            || matches!(decl.role, Some(Role::List | Role::Map))
                            || decl.summary.bound_params.contains(&(i as u32))
                        {
                            visit(decls, types, arg, params, out);
                        }
                    }
                }
            }
            TyKind::Tuple(args) => {
                for &arg in args {
                    visit(decls, types, arg, params, out);
                }
            }
            TyKind::Anchored(inner, _) => visit(decls, types, *inner, params, out),
            _ => {}
        }
    }
    let mut out = vec![];
    visit(decls, types, ty, params, &mut out);
    out
}

/// The bounds a type has (OWN-4, SEM-7).
pub fn has_bounds(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> u16 {
    bounds_inner::<true>(decls, t, ty, params) | inert(decls, t, ty, params)
}

/// `inert` (OWN-4) is exactly an empty holds set (OWN-2), so no other bound conditions it.
fn inert(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> u16 {
    if holds_nothing(decls, t, ty, params) {
        INERT
    } else {
        0
    }
}

pub(crate) fn missing_bounds(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    params: &[u16],
    required: u16,
) -> u16 {
    if required == 0 {
        return 0;
    }
    let available = if required & ENCODED == 0 {
        bounds_inner::<false>(decls, t, ty, params)
    } else {
        bounds_inner::<true>(decls, t, ty, params)
    };
    let available = if required & INERT != 0 {
        available | inert(decls, t, ty, params)
    } else {
        available
    };
    required & !available
}

fn bounds_inner<const ENCODING: bool>(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    params: &[u16],
) -> u16 {
    let row = t.kind(ty);
    let all = PLAIN | SHAREABLE | SEND | CROSS | KEY | ORDERED;
    let plain_extra = if kind_row(decls, t, row, params) == Kind::Plain {
        let cross = match row {
            TyKind::Adt(..) if crate::foreign::holds_raw_handle(decls, t, ty) => 0,
            TyKind::Tuple(_) | TyKind::Anchored(..) | TyKind::Array(..) => 0,
            _ => CROSS,
        };
        let shareable = match row {
            TyKind::Adt(d, _)
                if decls[d.index()].role == Some(Role::ForeignHandle)
                    || decls[d.index()].structural =>
            {
                0
            }
            TyKind::Tuple(_) | TyKind::Anchored(..) => 0,
            _ => SHAREABLE,
        };
        PLAIN | shareable | SEND | cross
    } else {
        0
    };
    let meet = |xs: &[TyId]| {
        xs.iter().fold(all | CODEC, |b, &x| {
            b & bounds_inner::<ENCODING>(decls, t, x, params)
        })
    };
    let encoded = || {
        if ENCODING {
            encoding_bounds(decls, t, ty, params) & ENCODED
        } else {
            0
        }
    };
    plain_extra
        | match row {
            TyKind::Error | TyKind::Hole => all | INTEGER | FLOAT | ENCODED | CODEC,
            TyKind::Never | TyKind::Unit => all | CODEC,
            TyKind::Scalar(Scalar::F32 | Scalar::F64) => {
                PLAIN | SHAREABLE | SEND | CROSS | FLOAT | ENCODED | CODEC
            }
            TyKind::Scalar(s) if hirsel_core_ir::Ty::Scalar(*s).int().is_some() => {
                all | INTEGER | ENCODED | CODEC
            }
            TyKind::Scalar(Scalar::Bool | Scalar::Char) => all | ENCODED | CODEC,
            TyKind::Scalar(_) => all | ENCODED,
            TyKind::Fn(_, _, _, callable) => match callable {
                Callable::Plain => PLAIN | SHAREABLE | SEND | CROSS,
                Callable::OwnSend => SEND,
                Callable::Own | Callable::Scoped | Callable::ScopedView => 0,
            },
            TyKind::Tuple(xs) => meet(xs) | encoded(),
            TyKind::Region(_) => 0,
            TyKind::Anchored(x, _) => bounds_inner::<ENCODING>(decls, t, *x, params),
            TyKind::Array(x, _) => bounds_inner::<ENCODING>(decls, t, *x, params) & all,
            TyKind::Vector(_) => PLAIN | SHAREABLE | SEND | CROSS,
            TyKind::View(x) => bounds_inner::<ENCODING>(decls, t, *x, params) & (KEY | ORDERED),
            TyKind::Param(i) => params.get(*i as usize).copied().unwrap_or(0),
            TyKind::Adt(d, args) => {
                let decl = &decls[d.index()];
                let mut available = nominal_bounds(
                    decl,
                    || meet(args),
                    |i| bounds_inner::<ENCODING>(decls, t, args[i], params),
                );
                // Wire parameters and the parameters held by the value can differ with via.
                if available & CODEC != 0
                    && decl.codec().is_some()
                    && !holds_nothing(decls, t, ty, params)
                {
                    available &= !CODEC;
                }
                available | encoded()
            }
        }
}

fn nominal_bounds(decl: &TypeDecl, meet: impl Fn() -> u16, at: impl Fn(usize) -> u16) -> u16 {
    let declared = match decl.role {
        // L2 (mc-units-lang): an owner crosses as a flat copy of its content,
        // so only a list of plain elements and a string are `cross`.
        Some(Role::List) => {
            let elems = meet();
            elems & (KEY | ORDERED | SHAREABLE | SEND | CODEC)
                | if elems & (PLAIN | CROSS) == PLAIN | CROSS {
                    CROSS
                } else {
                    0
                }
        }
        Some(Role::String) => KEY | ORDERED | SHAREABLE | SEND | CROSS | CODEC,
        Some(Role::FrozenBytes) => SHAREABLE | SEND,
        Some(Role::Text) => KEY | ORDERED,
        Some(Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan) => meet() & (KEY | ORDERED),
        Some(Role::Child | Role::Unique | Role::Site) => PLAIN | SHAREABLE | SEND | CROSS | KEY,
        Some(Role::Args) => PLAIN | SHAREABLE | SEND | CROSS,
        Some(Role::Causes) => SEND,
        Some(Role::StaticFn) => PLAIN | SHAREABLE | SEND | CROSS | KEY,
        Some(Role::Rc) => SHAREABLE | (meet() & (KEY | ORDERED)),
        Some(Role::Arc) => SHAREABLE | SEND | (meet() & (KEY | ORDERED)),
        Some(Role::Box) => meet() & (SHAREABLE | SEND | KEY | ORDERED),
        Some(Role::Sender | Role::Receiver | Role::HostFile | Role::Latest | Role::Inbox) => SEND,
        Some(Role::Client) => SEND | CROSS,
        Some(Role::Socket) if matches!(decl.path.0.name.as_str(), "Socket" | "Listener") => {
            SEND | CROSS
        }
        Some(Role::Socket) => SEND,
        Some(Role::Map) => meet() & (SHAREABLE | SEND | CODEC),
        Some(Role::Capability) => PLAIN | SHAREABLE | SEND | CROSS,
        Some(Role::ForeignHandle) => PLAIN | SEND,
        Some(_) => 0,
        None if decl.structural => meet(),
        None => {
            let mut bounds = decl.declared & !PLAIN;
            // L2: a variant crosses only when plain, which `plain_extra` gives.
            if matches!(decl.body, DeclBody::Variant(_)) {
                bounds &= !CROSS;
            }
            for &i in &decl.summary.bound_params {
                bounds &= at(i as usize);
            }
            if let Some(codec) = decl.codec() {
                let mut codec_bound = decl.declared & CODEC;
                for &i in &codec.params {
                    codec_bound &= at(i as usize);
                }
                bounds = bounds & !CODEC | codec_bound;
            }
            bounds
        }
    };
    match decl.repr {
        Repr::Region => declared & !(SEND | CROSS | SHAREABLE | CODEC),
        Repr::Scoped => declared & !(SEND | CROSS | SHAREABLE | KEY | ORDERED | CODEC),
        // SEM-7: a resource is never compared, whatever it declares.
        Repr::Resource => declared & !(KEY | ORDERED | CODEC),
        Repr::Own => declared,
    }
}

/// Whether a declaration permits send when all its type arguments are plain
/// and meet every bound. Layout checks the actual fields separately.
pub(crate) fn permits_send(decl: &TypeDecl) -> bool {
    let plain = if let Some(role) = decl.role {
        role.kind() == Kind::Plain
    } else {
        decl.repr == Repr::Own && (decl.structural || decl.summary.kind_fixed == Some(Kind::Plain))
    };
    plain || nominal_bounds(decl, || u16::MAX, |_| u16::MAX) & SEND != 0
}

pub(crate) fn mode_of(k: NodeKind) -> Mode {
    match k {
        NodeKind::MutParameter => Mode::Mut,
        NodeKind::TakeParameter => Mode::Take,
        _ => Mode::View,
    }
}

/// Callable types retain the bytes mark; a declaration's binding mode remains view.
pub(crate) fn callable_mode_of(k: NodeKind) -> Mode {
    if k == NodeKind::BytesParameter {
        Mode::Bytes
    } else if k == NodeKind::StepsParameter {
        Mode::Steps
    } else {
        mode_of(k)
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

/// The canonical text of a source type, including its lifetime anchors.
/// Layout rows and instance recipes use `layout_name` instead.
pub fn canonical(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId) -> String {
    crate::cost_scope!("query", "canonical type name");
    let mut s = String::new();
    write_canonical::<false>(decls, t, ty, None, &mut s);
    s
}
/// A type as a diagnostic shows it: the canonical text with each region root named by
/// `root` (`Doc in other`), since the canonical text writes every root as `in r`.
pub(crate) fn shown(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    root: &dyn Fn(&Root, &mut String),
) -> String {
    let mut s = String::new();
    write_canonical::<false>(decls, t, ty, Some(root), &mut s);
    s
}
/// Layout identity erases lifetime anchors at every depth. Nested views remain
/// distinct because they change the field's cleanup and scoped representation.
pub(crate) fn layout_name(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId) -> String {
    crate::cost_scope!("query", "layout type name");
    t.layout_name(decls, ty)
}
pub(crate) fn with_layout_name<R>(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    read: impl FnOnce(std::borrow::Cow<'_, str>) -> R,
) -> R {
    crate::cost_scope!("query", "layout type name");
    t.with_layout_name(decls, ty, read)
}
pub(crate) fn uncached_layout_name(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
) -> String {
    let mut s = String::new();
    write_canonical::<true>(decls, t, ty, None, &mut s);
    s
}
fn write_canonical<const LAYOUT: bool>(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    root: Option<&dyn Fn(&Root, &mut String)>,
    s: &mut String,
) {
    use std::fmt::Write;
    let child = |ty, s: &mut String| {
        if LAYOUT {
            t.with_layout_name(decls, ty, |name| s.push_str(name.as_ref()));
        } else {
            write_canonical::<false>(decls, t, ty, root, s);
        }
    };
    let list = |xs: &[TyId], s: &mut String| {
        for (i, &x) in xs.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            child(x, s);
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
        // A root is a checker fact: every root of a type shares its layout row and its
        // instances, so a caller's local region and a callee's parameter name one row.
        TyKind::Region(r) => match root {
            Some(name) => name(r, s),
            None => s.push_str("in r"),
        },
        TyKind::Anchored(x, r) => {
            child(*x, s);
            if !LAYOUT {
                s.push(' ');
                child(*r, s);
            }
        }
        TyKind::Vector(v) => {
            let _ = write!(s, "Simd[{}, {}]", v.lane().name(), v.lanes());
        }
        TyKind::Array(x, n) => {
            s.push_str("Inline[");
            child(*x, s);
            let _ = write!(s, ", {n}]");
        }
        TyKind::View(x) => {
            s.push_str("View[");
            child(*x, s);
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
                Callable::ScopedView => "scoped[view] fn(",
            });
            for (i, (m, p)) in ps.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(match m {
                    Mode::View => "",
                    Mode::Mut => "mut ",
                    Mode::Take => "take ",
                    Mode::Bytes => "bytes ",
                    Mode::Steps => "steps ",
                });
                child(*p, s);
            }
            s.push_str(") -> ");
            child(*r, s);
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
    written_result_missing_anchor(p, at, n, &|_| true)
}

pub(crate) fn written_result_missing_anchor(
    p: &Program<'_>,
    at: ItemRef,
    n: NodeId,
    valid_root: &impl Fn(NodeId) -> bool,
) -> bool {
    let item = p.item(at);
    let kids = item.tree.children(n);
    match item.tree.node(n).kind {
        NodeKind::Anchor => !valid_root(*kids.last().unwrap()),
        NodeKind::ScopedCallableType => true,
        NodeKind::TupleType | NodeKind::GroupType => kids
            .iter()
            .any(|&c| written_result_missing_anchor(p, at, c, valid_root)),
        NodeKind::TypeArguments => {
            if let Some(Target::Definition(d)) = p.target(at, kids[0]) {
                if let Some(Def::Type(d)) = p.by_definition[d.0 as usize] {
                    let decl = &p.decls[d.index()];
                    if decl.structural {
                        return kids[1..]
                            .iter()
                            .any(|&c| written_result_missing_anchor(p, at, c, valid_root));
                    }
                    if decl.repr == Repr::Scoped
                        || decl.summary.kind_fixed == Some(Kind::Scoped)
                        || matches!(decl.body, DeclBody::Alias(ty) if scoped_payload(&p.decls, &p.types, ty))
                        || decl.role.is_some_and(|r| r.kind() == Kind::Scoped)
                    {
                        return !kids[1..].iter().any(|&c| {
                            item.tree.node(c).kind == NodeKind::Anchor
                                && item.tree.children(c).len() == 1
                                && valid_root(item.tree.children(c)[0])
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
                    decl.repr == Repr::Scoped
                        || decl.summary.kind_fixed == Some(Kind::Scoped)
                        || matches!(decl.body, DeclBody::Alias(ty) if scoped_payload(&p.decls, &p.types, ty))
                        || decl.role.is_some_and(|r| r.kind() == Kind::Scoped)
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
        TyKind::Anchored(x, _) => scoped_payload(decls, t, *x),
        TyKind::View(_) | TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView) => true,
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

/// Scoped roots are part of a callable's result contract; expression values keep
/// them in anchor facts instead. Nested callable signatures retain their contracts.
pub(crate) fn erase_scoped_result(
    decls: &crate::Rows<TypeDecl>,
    t: &mut impl TyRead,
    ty: TyId,
) -> TyId {
    if !scoped_payload(decls, t, ty) {
        return ty;
    }
    match t.kind(ty).clone() {
        TyKind::Anchored(x, _) => erase_scoped_result(decls, t, x),
        TyKind::Tuple(mut xs) => {
            for x in &mut xs {
                *x = erase_scoped_result(decls, t, *x);
            }
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, mut xs) => {
            for x in &mut xs {
                *x = erase_scoped_result(decls, t, *x);
            }
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::Array(x, n) => {
            let x = erase_scoped_result(decls, t, x);
            t.mk(TyKind::Array(x, n))
        }
        _ => ty,
    }
}

/// Every scoped result component must retain its declared region contract.
pub(crate) fn scoped_result_rooted(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    anchored: bool,
) -> bool {
    if !scoped_payload(decls, t, ty) {
        return true;
    }
    match t.kind(ty) {
        TyKind::Anchored(x, root) => scoped_result_rooted(
            decls,
            t,
            *x,
            anchored || matches!(t.kind(*root), TyKind::Region(_)),
        ),
        TyKind::Tuple(xs) => xs
            .iter()
            .all(|&x| scoped_result_rooted(decls, t, x, anchored)),
        TyKind::Adt(d, xs) if decls[d.index()].structural => xs
            .iter()
            .all(|&x| scoped_result_rooted(decls, t, x, anchored)),
        TyKind::Adt(_, xs) => {
            anchored || xs.iter().any(|&x| matches!(t.kind(x), TyKind::Region(_)))
        }
        _ => anchored,
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
                    && (decls[b.index()].repr == Repr::Region
                        || decls[b.index()]
                            .role
                            .is_some_and(|r| r.kind() == Kind::Region)))
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

fn region_field(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId, params: &[u16]) -> bool {
    let row = t.kind(ty);
    match row {
        TyKind::Anchored(_, root) if t.kind(*root) != &TyKind::Region(Root::Enclosing) => false,
        TyKind::Tuple(xs) => xs.iter().all(|&x| region_field(decls, t, x, params)),
        TyKind::Adt(d, xs) if is_core_prelude(&decls[d.index()].path, &["Option"]) => {
            xs.iter().all(|&x| region_field(decls, t, x, params))
        }
        _ => matches!(kind_row(decls, t, row, params), Kind::Plain | Kind::Region),
    }
}
fn unrooted_region(decls: &crate::Rows<TypeDecl>, t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Anchored(_, _) => false,
        TyKind::Tuple(xs) => xs.iter().any(|&x| unrooted_region(decls, t, x)),
        TyKind::Adt(d, xs) => {
            let decl = &decls[d.index()];
            decl.repr == Repr::Region
                || decl.role.is_some_and(|r| r.kind() == Kind::Region)
                || decl
                    .summary
                    .bound_params
                    .iter()
                    .any(|&i| unrooted_region(decls, t, xs[i as usize]))
        }
        TyKind::Array(x, _) => unrooted_region(decls, t, *x),
        _ => false,
    }
}

pub(crate) const VIEW_RESULT_RESERVED: &str = "scoped[view] fn results with anchors are reserved";

/// The view-only form reserves any result carrying a lexical or region anchor.
pub(crate) fn view_result_reserved(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
) -> bool {
    match t.kind(ty) {
        TyKind::Anchored(..) | TyKind::View(_) => true,
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => {
            scoped_payload(decls, t, ty) || xs.iter().any(|&x| view_result_reserved(decls, t, x))
        }
        TyKind::Array(x, _) => view_result_reserved(decls, t, *x),
        _ => scoped_payload(decls, t, ty),
    }
}

/// Read the callable promise and refuse a bracket at its first offending name.
pub(crate) fn callable_kind(
    item: hirsel_core_syntax::Item<'_>,
    n: NodeId,
) -> Result<Callable, (NodeId, &'static str)> {
    Ok(match item.tree.node(n).kind {
        NodeKind::ScopedCallableType | NodeKind::OwnCallableType => {
            let scoped = item.tree.node(n).kind == NodeKind::ScopedCallableType;
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
                            != if scoped { "view" } else { "send" }
                    {
                        return Err((
                            bound,
                            if scoped {
                                "a scoped callable bracket contains only view"
                            } else {
                                "an owning callable bracket contains only send"
                            },
                        ));
                    }
                }
                if scoped {
                    Callable::ScopedView
                } else {
                    Callable::OwnSend
                }
            } else if scoped {
                Callable::Scoped
            } else {
                Callable::Own
            }
        }
        _ => Callable::Plain,
    })
}

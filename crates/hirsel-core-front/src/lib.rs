//! The new core language's single check-and-lower traversal.
//!
//! [`Program::declare`] publishes one summary per type declaration (OWN-2), in
//! dependency order, and checks every signature once (LANG-8); it reads no body.
//! [`Program::check_lower`] then visits one definition's body once, forward, typing it,
//! checking ownership with the state OWN names, and emitting CoreIR as it goes. No fact
//! crosses definitions except through signatures and summaries, nothing is iterated to a
//! fixpoint, and nothing the pass emits is checked again.

#[cfg(test)]
hirsel_guard::install!();

mod body;
pub mod codec_decls;
mod constants;
mod recursion_decls;
pub use constants::{ConstData, ConstValue, Constant};
mod decls;
mod defaults;
mod element;
mod foreign;
pub use foreign::{AbiParameter, AbiResult, AbiSignature};
mod identity;
mod layer;
pub use layer::Declared;
mod layout;
mod library;
pub use library::{Library, Rows};
mod own;
mod test_functions;
mod types;

pub use body::flow::Facts as FlowFacts;
pub use body::{
    CoreCheck, Counts, Emitter, GLUE_OPERATIONS, GenericTemplate, NamePart, import_instance,
    is_glue, materialize, materialize_glue,
};
pub use decls::*;
pub use types::*;

use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{
    DefinitionId, ModuleId, NameIndex, Refusal, Resolution, ResolutionScratch, ResolvedUse, Site,
    Target,
};
use hirsel_core_syntax::{Item, NodeId, Span, Symbol, SyntaxFile};
use std::collections::HashMap;
use std::hash::BuildHasher;

/// One module of the program, already parsed.
pub struct ModuleInput {
    pub id: ModuleId,
    pub source: hirsel_core_resolve::SourceId,
    pub syntax: SyntaxFile,
}

/// An item of a module.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ItemRef {
    pub module: u32,
    pub item: u32,
}

/// A construct this implementation does not build yet: not a refusal of the program,
/// and never silently accepted either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gap {
    pub site: Site,
    pub what: &'static str,
}
impl Gap {
    pub fn new(site: Site, what: &'static str) -> Self {
        Self { site, what }
    }
}

/// The implementation's resource refusal (PROG-6): checking needs more than one of its
/// limits allows. It names no rule; the message names the amount and the limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceRefusal {
    pub site: Site,
    pub message: String,
}

pub(crate) fn refusal(rule: &str, primary: Site, message: impl Into<String>) -> Refusal {
    Refusal {
        rule: rule.into(),
        primary,
        causal: vec![],
        declared: None,
        required: None,
        message: message.into(),
        unresolved: None,
        detail: None,
    }
}

pub(crate) struct ItemInfo {
    pub resolution: std::sync::Arc<Resolution>,
    /// Node index to its entry in `resolution.names`, or `u32::MAX`.
    pub uses: Vec<u32>,
}

struct TupleCodecProtocol {
    of: DefPath,
    bytes: TyId,
    codec: TyId,
    decoded: TyId,
    written: TyId,
    schema: TyId,
}

/// The declarations of one program: summaries and signatures, published once.
pub struct Program<'a> {
    pub index: &'a NameIndex,
    pub modules: &'a [ModuleInput],
    pub(crate) items: Vec<Vec<ItemInfo>>,
    pub types: Types,
    pub decls: Rows<TypeDecl>,
    pub fns: Rows<FnDecl>,
    pub constants: Rows<Constant>,
    pub constant_parts: Rows<ConstValue>,
    pub(crate) constant_strings: Rows<Vec<u8>>,
    pub(crate) constant_ids: Rows<Option<u32>>,
    /// Each `match` scrutinee and `if` condition that is a constant expression, by its
    /// item and node: its value on each target (PROG-6).
    pub(crate) constant_branches:
        HashMap<(ItemRef, NodeId), [ConstValue; hirsel_core_layout::Target::ALL.len()]>,
    /// Each constant an `Inline` length may name, by definition, evaluated when a
    /// header first names it (SEM-9); `None` while its evaluation is under way.
    pub(crate) length_constants:
        HashMap<DefinitionId, Option<Result<ConstValue, constants::LengthWhy>>>,
    /// This program's own definitions by path. A path lives once, in its
    /// row: an entry is hashed and compared through its row's path.
    by_path: hashbrown::HashTable<Def>,
    pub by_definition: Rows<Option<Def>>,
    pub(crate) core_operations: Rows<Option<u16>>,
    pub(crate) library_paths: Option<std::sync::Arc<hirsel_core_ids::published::Map<DefPath, Def>>>,
    core_never: Option<DefinitionId>,
    /// Refusals of declarations and signatures, and the resolver's.
    pub refusals: Vec<Refusal>,
    pub gaps: Vec<Gap>,
    /// Declarations past an implementation limit (PROG-6): no rule, not a gap.
    pub resource_refusals: Vec<ResourceRefusal>,
    /// The program's entry, as its manifest names it (PROG-3); `main` is always `waits`
    /// (LANG-8).
    pub entry: Option<DefPath>,
    pub(crate) library_modules: usize,
    pub(crate) has_test_functions: bool,
    pub(crate) pending: Vec<decls::Pending>,
    pub(crate) alias_state: Vec<u8>,
    /// Aliases being lowered, each with its reference continuing the chain.
    pub(crate) alias_stack: Vec<(DeclId, Option<Site>)>,
    core_declarations: [std::sync::OnceLock<Option<DeclId>>; PRELUDE.len()],
    structural_codecs: std::sync::OnceLock<StructuralCodecs>,
    tuple_codec_protocol: std::sync::OnceLock<Option<TupleCodecProtocol>>,
    ordered_codec_protocol: std::sync::OnceLock<Option<TupleCodecProtocol>>,
    /// Published type spellings shared by body arenas; local types stay local.
    layout_type_names: std::sync::OnceLock<Vec<std::sync::OnceLock<String>>>,
    recipe_layouts: std::sync::Mutex<layout::RecipeLayouts>,
    /// Closed rows' closures as a fresh body builds them (body/row_closures.rs).
    row_closures: body::RowClosures,
    flow_holds: std::sync::OnceLock<Vec<std::sync::OnceLock<(u8, u64)>>>,
    closed_type_facts: std::sync::OnceLock<Vec<body::ClosedTypeFacts>>,
    foreign_headers: std::sync::OnceLock<foreign::CheckedHeaders>,
}

/// The core types lowering builds, matches or lays out without the program naming
/// them: a capacity failure, a string literal, a region, a `Causes` element, the
/// `Arc[T]` holder a `Latest` root drops. A
/// body's reuse key covers every one, since no resolved name of the body reaches it.
pub const PRELUDE: [&str; 15] = [
    "String",
    "Result",
    "Capacity",
    "List",
    "Region",
    "Map",
    "Text",
    "Cause",
    "BuildError",
    "Child",
    "FloatError",
    "Bytes",
    "Option",
    "OutOfTime",
    "Arc",
];
/// A core function body lowering calls by a path it builds, never through a
/// resolved name of the body. Lowering finds one only through
/// `Program::core_helper`, and the driver's reuse key hashes the closure of each
/// in [`SIBLING_READS`]' row of the callee being lowered, or its absence, so a
/// lowering site that calls a helper missing from its callee's row is the one
/// read the key does not cover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreHelper {
    ListGrow,
    ListGrowBy,
    HeapPreparePush,
    HeapFinishPreparedPush,
    DequePreparePush,
    DequeFinishPreparedPush,
    MapProbeStaged,
    MapCommitCrowded,
    MapCommit,
}
impl CoreHelper {
    /// The helper's module in core and its name there.
    pub const fn path(self) -> (&'static str, &'static str) {
        match self {
            Self::ListGrow => ("list", "grow"),
            Self::ListGrowBy => ("list", "grow_by"),
            Self::HeapPreparePush => ("heap", "prepare_push"),
            Self::HeapFinishPreparedPush => ("heap", "finish_prepared_push"),
            Self::DequePreparePush => ("deque", "prepare_push"),
            Self::DequeFinishPreparedPush => ("deque", "finish_prepared_push"),
            Self::MapProbeStaged => ("map", "probe_staged"),
            Self::MapCommitCrowded => ("map", "commit_crowded"),
            Self::MapCommit => ("map", "commit"),
        }
    }
}
/// What lowering a [`SIBLING_READS`] callee reads beyond the body's names.
pub enum SiblingReads {
    /// Core helpers lowering calls ([`CoreHelper`]).
    Core(&'static [CoreHelper]),
    /// Functions of one module of the callee's package, by name: std's built-in
    /// codec makers beside `codec.of`.
    Named(&'static str, &'static [&'static str]),
}
impl SiblingReads {
    /// Each read as (module, name) in the callee's package, in key order.
    pub fn paths(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        let (core, named) = match *self {
            Self::Core(helpers) => (helpers, None),
            Self::Named(module, names) => (&[][..], Some((module, names))),
        };
        core.iter().map(|h| h.path()).chain(
            named
                .into_iter()
                .flat_map(|(module, names)| names.iter().map(move |&name| (module, name))),
        )
    }
}
/// The items body lowering calls when it lowers a resolved callee:
/// (package, callee module, callee, reads). A body that names the callee reads
/// those items, though no resolved name of the body reaches them, so its reuse
/// key hashes their closures (`Keys::for_body`). Lowering builds no other path:
/// a new one is added here first.
pub const SIBLING_READS: [(&str, &str, &str, SiblingReads); 12] = {
    use CoreHelper::*;
    use SiblingReads::*;
    [
        ("core", "list", "push", Core(&[ListGrow])),
        (
            "core",
            "heap",
            "push",
            Core(&[HeapPreparePush, HeapFinishPreparedPush]),
        ),
        (
            "core",
            "deque",
            "push_front",
            Core(&[DequePreparePush, DequeFinishPreparedPush]),
        ),
        (
            "core",
            "deque",
            "push_back",
            Core(&[DequePreparePush, DequeFinishPreparedPush]),
        ),
        (
            "core",
            "map",
            "insert",
            Core(&[MapProbeStaged, MapCommitCrowded, MapCommit]),
        ),
        // `codec.of[T]` at a built-in T calls T's maker (body/calls.rs `codec_of`,
        // and body/templates.rs `codec_maker` at an instance).
        (
            "std",
            "codec",
            "of",
            Named("codec", &body::BUILTIN_CODEC_MAKERS),
        ),
        (
            "std",
            "codec",
            "key_of",
            Named("codec", &body::ordered_codec::MAKERS),
        ),
        ("core", "list", "reserve", Core(&[ListGrow, ListGrowBy])),
        ("core", "list", "append", Core(&[ListGrowBy])),
        ("core", "list", "fill", Core(&[ListGrowBy])),
        // String push and reserve use the List[u8] append and reserve kernels.
        ("core", "string", "push", Core(&[ListGrowBy])),
        ("core", "string", "reserve", Core(&[ListGrow, ListGrowBy])),
    ]
};
/// Codec row construction reads these signatures even when the source never
/// calls a maker. The driver's prelude component records them.
pub const TUPLE_CODEC_READS: [&str; 5] = [
    "take_with",
    "put_with",
    "schema_of",
    "take_key_with",
    "schema_key",
];
/// The core prelude types whose codec is std's structural maker, each with that
/// maker in `std.codec` (EXPANSION 9.1). Laying out a closed codec row of one of
/// them reads its maker (the row's codec hook, layout.rs `structural_codec`), and
/// so does `of` at one, so every body's reuse key hashes the makers' closures beside
/// the prelude's (the driver's `Keys::for_body`).
pub const STRUCTURAL_CODECS: [(&str, &str); 4] = [
    ("List", "of_list"),
    ("Map", "of_map"),
    ("Option", "of_option"),
    ("Result", "of_result"),
];
/// std's maker of `List[u8]`, which moves the bytes in bulk; every byte list's
/// codec is this one, read as [`STRUCTURAL_CODECS`]' makers are.
pub const BYTES_CODEC: &str = "of_bytes";
/// [`STRUCTURAL_CODECS`] and [`BYTES_CODEC`] as one program's function rows.
pub struct StructuralCodecs {
    rows: Vec<(DeclId, u32)>,
    /// List's declaration and `of_bytes`.
    bytes: Option<(DeclId, u32)>,
}
impl StructuralCodecs {
    pub const NONE: Self = Self {
        rows: Vec::new(),
        bytes: None,
    };
    /// Whether no type's codec is std's structural maker (the program has no std).
    pub fn rows_empty(&self) -> bool {
        self.rows.is_empty()
    }
    /// Whether `d`'s codec is std's structural maker.
    pub fn has(&self, d: DeclId) -> bool {
        self.rows.iter().any(|&(x, _)| x == d)
    }
    /// std's maker of `d` at `args` with the arguments it takes, when `d` is one of
    /// [`STRUCTURAL_CODECS`]' types: `of_bytes`, taking none, at `List[u8]`.
    pub fn maker(&self, types: &impl TyRead, d: DeclId, args: &[TyId]) -> Option<(u32, Vec<TyId>)> {
        if let Some((list, bytes)) = self.bytes
            && d == list
            && let [element] = args
            && matches!(
                types.kind(*element),
                TyKind::Scalar(hirsel_core_layout::Scalar::U8)
            )
        {
            return Some((bytes, vec![]));
        }
        let &(_, maker) = self.rows.iter().find(|&&(x, _)| x == d)?;
        Some((maker, args.to_vec()))
    }
}
/// The path of the core prelude type `name`, when the index has core.
fn prelude_path(index: &NameIndex, name: &str) -> Option<DefPath> {
    let package = &index.packages[index.core?.0 as usize];
    Some(DefPath(hirsel_core_ids::ItemPath {
        package: hirsel_core_ids::PackageId {
            name: index.symbols.text(package.name).into(),
            version: index.symbols.text(package.version).into(),
        },
        modules: vec!["core".into()],
        name: name.into(),
    }))
}

impl<'a> Program<'a> {
    /// LANG-7: infer a bare constructor in the current module, then the
    /// prelude. At either level, a record takes precedence over variants.
    fn bare_constructor(
        &self,
        from: ModuleId,
        name: Symbol,
    ) -> Result<(DeclId, Option<u32>), String> {
        let text = self.index.symbols.text(name);
        let mut best = None;
        let mut ambiguous = false;
        let mut elsewhere = None;
        for id in self.index.constructor_candidates(from, name) {
            let Some(Def::Type(d)) = self.by_definition[id.0 as usize] else {
                continue;
            };
            let decl = &self.decls[d.index()];
            let case = match &decl.body {
                DeclBody::Record(_) => None,
                DeclBody::Variant(cases) => {
                    let Some(i) = cases.iter().position(|c| c.name == text) else {
                        continue;
                    };
                    Some(i as u32)
                }
                DeclBody::Alias(_) => continue,
            };
            let outside = decl.site.module != from;
            if outside && (decl.opaque || !self.index.is_prelude(id)) {
                if case.is_some() && !decl.opaque {
                    elsewhere.get_or_insert(d);
                }
                continue;
            }
            let rank = (outside, case.is_some());
            match best {
                Some((first, other, _)) if first == rank => ambiguous |= other != d,
                Some((first, _, _)) if first < rank => {}
                _ => {
                    best = Some((rank, d, case));
                    ambiguous = false;
                }
            }
        }
        if ambiguous {
            return Err(format!(
                "two visible types declare the variant `{text}` at the same level"
            ));
        }
        if let Some((_, d, case)) = best {
            return Ok((d, case));
        }
        if let Some(d) = elsewhere {
            let decl = &self.decls[d.index()];
            let path = &decl.path.0;
            let package = self.index.module_package(decl.site.module);
            let qualify = package != self.index.module_package(from)
                && Some(package) != self.index.core
                && path.modules.first() != Some(&path.package.name);
            let spelling = qualify
                .then_some(path.package.name.as_str())
                .into_iter()
                .chain(path.modules.iter().map(String::as_str))
                .chain(std::iter::once(path.name.as_str()))
                .collect::<Vec<_>>()
                .join(".");
            return Err(format!(
                "variant `{text}` is outside the current module and prelude. Use `{spelling}` as its expected type"
            ));
        }
        Err(format!("no visible type declares `{text}`"))
    }

    /// The core prelude type named `name`, when the program has core. `name` is
    /// one of [`PRELUDE`].
    pub(crate) fn core_decl(&self, name: &str) -> Option<DeclId> {
        // Lowering asks on hot paths: a match, in `PRELUDE`'s order.
        let slot = match name {
            "String" => 0,
            "Result" => 1,
            "Capacity" => 2,
            "List" => 3,
            "Region" => 4,
            "Map" => 5,
            "Text" => 6,
            "Cause" => 7,
            "BuildError" => 8,
            "Child" => 9,
            "FloatError" => 10,
            "Bytes" => 11,
            "Option" => 12,
            "OutOfTime" => 13,
            "Arc" => 14,
            _ => panic!("`{name}` is not a core prelude type"),
        };
        debug_assert_eq!(PRELUDE[slot], name);
        *self.core_declarations[slot].get_or_init(|| self.core_decl_uncached(name))
    }
    /// The core function `module.name`, when the program has core.
    /// The function `helper` names in core, if core defines it.
    pub(crate) fn core_helper(&self, helper: CoreHelper) -> Option<usize> {
        let (module, name) = helper.path();
        let key = self.index.core?;
        let package = &self.index.packages[key.0 as usize];
        let path = DefPath(hirsel_core_ids::ItemPath {
            package: hirsel_core_ids::PackageId {
                name: self.index.symbols.text(package.name).into(),
                version: self.index.symbols.text(package.version).into(),
            },
            modules: vec![module.into()],
            name: name.into(),
        });
        match self.def(&path)? {
            Def::Fn(f) => Some(f as usize),
            Def::Type(_) => None,
        }
    }
    /// [`STRUCTURAL_CODECS`] as the program's rows: each core type's declaration with
    /// its std maker's function. Empty when the program has no std.
    pub(crate) fn structural_codecs(&self) -> &StructuralCodecs {
        self.structural_codecs.get_or_init(|| {
            let Some(package) = self
                .index
                .packages
                .iter()
                .find(|p| self.index.symbols.text(p.name) == "std")
            else {
                return StructuralCodecs::NONE;
            };
            let package = hirsel_core_ids::PackageId {
                name: "std".into(),
                version: self.index.symbols.text(package.version).into(),
            };
            let maker = |name: &str| {
                let path = DefPath(hirsel_core_ids::ItemPath {
                    package: package.clone(),
                    modules: vec!["codec".into()],
                    name: name.into(),
                });
                Some(self.function_id(&path)? as u32)
            };
            StructuralCodecs {
                rows: STRUCTURAL_CODECS
                    .into_iter()
                    .filter_map(|(ty, name)| Some((self.core_decl(ty)?, maker(name)?)))
                    .collect(),
                bytes: self.core_decl("List").zip(maker(BYTES_CODEC)),
            }
        })
    }
    /// Shared tuple codec signatures. The name index avoids demand-decoding the
    /// published function prefix just to find the three protocol declarations.
    fn tuple_codec_protocol(&self) -> Option<&TupleCodecProtocol> {
        self.tuple_codec_protocol
            .get_or_init(|| self.codec_protocol("take_with", "schema_of"))
            .as_ref()
    }

    fn ordered_codec_protocol(&self) -> Option<&TupleCodecProtocol> {
        self.ordered_codec_protocol
            .get_or_init(|| self.codec_protocol("take_key_with", "schema_key"))
            .as_ref()
    }

    fn codec_protocol(&self, take: &str, describe: &str) -> Option<TupleCodecProtocol> {
        let std = |name: &str| {
            self.index
                .packages
                .iter()
                .filter(|p| self.index.symbols.text(p.name) == "std")
                .filter_map(|p| {
                    self.function_id(&DefPath(hirsel_core_ids::ItemPath {
                        package: hirsel_core_ids::PackageId {
                            name: "std".into(),
                            version: self.index.symbols.text(p.version).into(),
                        },
                        modules: vec!["codec".into()],
                        name: name.into(),
                    }))
                })
                .min()
        };
        let (take, describe) = (std(take)?, std(describe)?);
        let fd = &self.fns[take];
        Some(TupleCodecProtocol {
            of: fd.path.clone(),
            bytes: fd.params[0].ty,
            codec: fd.params[2].ty,
            decoded: fd.result,
            written: self.fns[describe].result,
            schema: self.fns[describe].params[0].ty,
        })
    }
    fn core_decl_uncached(&self, name: &str) -> Option<DeclId> {
        let path = prelude_path(self.index, name)?;
        match self.def(&path)? {
            Def::Type(d) => Some(d),
            Def::Fn(_) => None,
        }
    }
    /// The path of a definition's row.
    pub(crate) fn def_path(&self, def: Def) -> &DefPath {
        def_path(&self.decls, &self.fns, def)
    }
    /// The definition `path` names, in this program or its library.
    pub(crate) fn def(&self, path: &DefPath) -> Option<Def> {
        let hash = rustc_hash::FxBuildHasher.hash_one(path);
        self.by_path
            .find(hash, |&d| self.def_path(d) == path)
            .copied()
            .or_else(|| self.library_paths.as_ref()?.get(path).copied())
    }
    /// Register `def` under its row's path unless the path names one already.
    pub(crate) fn add_path(&mut self, def: Def) {
        let (decls, fns) = (&self.decls, &self.fns);
        let path = def_path(decls, fns, def);
        let hash = rustc_hash::FxBuildHasher.hash_one(path);
        if let hashbrown::hash_table::Entry::Vacant(entry) = self.by_path.entry(
            hash,
            |&d| def_path(decls, fns, d) == path,
            |&d| rustc_hash::FxBuildHasher.hash_one(def_path(decls, fns, d)),
        ) {
            entry.insert(def);
        }
    }
    /// This program's own definitions with their paths, in no order.
    pub(crate) fn own_paths(&self) -> impl Iterator<Item = (&DefPath, Def)> {
        self.by_path.iter().map(|&d| (self.def_path(d), d))
    }

    /// Resolve every item, then publish summaries and signatures (slice a).
    pub fn declare(index: &'a NameIndex, modules: &'a [ModuleInput]) -> Self {
        let mut resolution_scratch = ResolutionScratch::default();
        let resolutions = modules
            .iter()
            .map(|m| {
                m.syntax
                    .items()
                    .enumerate()
                    .map(|(i, item)| {
                        std::sync::Arc::new(
                            resolution_scratch.resolve(index, m.id, m.source, i as u32, item),
                        )
                    })
                    .collect()
            })
            .collect();
        Self::declare_resolved(index, modules, resolutions)
    }

    /// Publish declarations from the resolver's completed rows without resolving again.
    /// The driver uses this boundary to attribute resolution and declaration work separately.
    pub fn declare_resolved(
        index: &'a NameIndex,
        modules: &'a [ModuleInput],
        resolutions: Vec<Vec<std::sync::Arc<Resolution>>>,
    ) -> Self {
        Self::declare_with_library(index, modules, resolutions, None)
    }

    /// Append only the request's declarations to a source-checked immutable library.
    pub fn declare_with_library(
        index: &'a NameIndex,
        modules: &'a [ModuleInput],
        resolutions: Vec<Vec<std::sync::Arc<Resolution>>>,
        library: Option<&Library>,
    ) -> Self {
        crate::cost_scope!("phase", "declaration summaries");
        let mut p = Self::open(index, modules, resolutions, library);
        p.register();
        p.lower_headers();
        p.check_foreign_signatures();
        p.check_recursive_instances();
        p.register_constants();
        p.summarize();
        p.evaluate_constants();
        p.decide_constant_branches();
        test_functions::check(&mut p);

        for f in p.fns.base_len()..p.fns.len() {
            let toolchain = p.index.supplied_by_toolchain(&p.fns[f].path.0.package);
            p.core_operations
                .push(body::core_ops::binding(&p.fns[f], toolchain));
        }
        p
    }

    /// The program before declaration: the resolver's rows and diagnostics,
    /// over the library's tables.
    fn open(
        index: &'a NameIndex,
        modules: &'a [ModuleInput],
        resolutions: Vec<Vec<std::sync::Arc<Resolution>>>,
        library: Option<&Library>,
    ) -> Self {
        let mut refusals = index.diagnostics.clone();
        let items = modules
            .iter()
            .zip(resolutions)
            .map(|(m, rows)| {
                m.syntax
                    .items()
                    .zip(rows)
                    .map(|(item, resolution)| {
                        refusals.extend(resolution.diagnostics.iter().cloned());
                        let mut uses = vec![
                            u32::MAX;
                            (item.range.nodes.end - item.range.nodes.start) as usize
                        ];
                        for (k, u) in resolution.names.iter().enumerate() {
                            uses[(u.node.0 - item.range.nodes.start) as usize] = k as u32;
                        }
                        ItemInfo { resolution, uses }
                    })
                    .collect()
            })
            .collect();
        Program {
            index,
            modules,
            items,
            types: library.map_or_else(Types::new, |l| {
                Types::overlay(std::sync::Arc::clone(&l.types))
            }),
            decls: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.decls))
            }),
            fns: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.fns))
            }),
            constants: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.constants))
            }),
            constant_parts: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.constant_parts))
            }),
            constant_strings: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.constant_strings))
            }),
            constant_ids: {
                let mut ids = library.map_or_else(Rows::default, |l| {
                    Rows::overlay(std::sync::Arc::clone(&l.constant_ids))
                });
                ids.resize(index.definitions.len(), None);
                ids
            },
            constant_branches: HashMap::new(),
            length_constants: HashMap::new(),
            by_path: hashbrown::HashTable::new(),
            library_paths: library.map(|l| std::sync::Arc::new(l.paths.clone())),
            core_never: index
                .symbols
                .find("Never")
                .zip(
                    index
                        .symbols
                        .find("core")
                        .and_then(|name| index.core_module(name)),
                )
                .and_then(|(name, module)| index.definition(module, name)),
            by_definition: {
                let mut rows = library.map_or_else(Rows::default, |l| {
                    Rows::overlay(std::sync::Arc::clone(&l.definitions))
                });
                rows.resize(index.definitions.len(), None);
                rows
            },
            core_operations: library.map_or_else(Rows::default, |l| {
                Rows::overlay(std::sync::Arc::clone(&l.core_operations))
            }),
            refusals,
            gaps: vec![],
            resource_refusals: vec![],
            entry: None,
            library_modules: 0,
            has_test_functions: library.is_some_and(|l| l.has_test_functions),
            pending: vec![],
            alias_state: vec![],
            alias_stack: vec![],
            core_declarations: std::array::from_fn(|_| std::sync::OnceLock::new()),
            structural_codecs: std::sync::OnceLock::new(),
            tuple_codec_protocol: std::sync::OnceLock::new(),
            ordered_codec_protocol: std::sync::OnceLock::new(),
            layout_type_names: std::sync::OnceLock::new(),
            recipe_layouts: Default::default(),
            row_closures: Default::default(),
            flow_holds: std::sync::OnceLock::new(),
            closed_type_facts: std::sync::OnceLock::new(),
            foreign_headers: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn item(&self, at: ItemRef) -> Item<'a> {
        let modules: &'a [ModuleInput] = self.modules;
        modules[at.module as usize].syntax.item(at.item as usize)
    }
    pub(crate) fn site(&self, at: ItemRef, n: NodeId) -> Site {
        self.site_span(at, self.item(at).tree.node(n).origin.span)
    }
    pub(crate) fn site_span(&self, at: ItemRef, span: Span) -> Site {
        Site {
            module: self.modules[at.module as usize].id,
            source: Some(self.modules[at.module as usize].source),
            item: at.item,
            span,
        }
    }
    pub(crate) fn target(&self, at: ItemRef, n: NodeId) -> Option<&Target> {
        Some(&self.resolved(at, n)?.target)
    }
    pub(crate) fn resolved(&self, at: ItemRef, n: NodeId) -> Option<&ResolvedUse> {
        let info = &self.items[at.module as usize][at.item as usize];
        let k = *info
            .uses
            .get((n.0 - self.item(at).range.nodes.start) as usize)?;
        (k != u32::MAX).then(|| &info.resolution.names[k as usize])
    }

    pub(crate) fn is_core_never(&self, id: DefinitionId) -> bool {
        self.core_never == Some(id)
    }
    pub fn decl(&self, path: &DefPath) -> Option<&TypeDecl> {
        match self.def(path)? {
            Def::Type(d) => Some(&self.decls[d.index()]),
            Def::Fn(_) => None,
        }
    }
    /// The row of the function `path` names, in this program or its library.
    pub fn function_id(&self, path: &DefPath) -> Option<usize> {
        match self.def(path)? {
            Def::Fn(f) => Some(f as usize),
            Def::Type(_) => None,
        }
    }
    pub fn function(&self, path: &DefPath) -> Option<&FnDecl> {
        match self.def(path)? {
            Def::Fn(f) => Some(&self.fns[f as usize]),
            Def::Type(_) => None,
        }
    }
    /// The kind of a closed type (OWN-3).
    pub fn kind(&self, ty: TyId) -> Kind {
        decls::kind(&self.decls, &self.types, ty, &[])
    }
}

fn def_path<'r>(decls: &'r Rows<TypeDecl>, fns: &'r Rows<FnDecl>, def: Def) -> &'r DefPath {
    match def {
        Def::Type(d) => &decls[d.index()].path,
        Def::Fn(f) => &fns[f as usize].path,
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "cost-profile")]
pub mod cost;

/// A diagnostic scope compiled out of production compiler units.
#[cfg(feature = "cost-profile")]
#[macro_export]
macro_rules! cost_scope {
    ($view:expr, $name:expr) => {
        let _cost_scope = $crate::cost::Scope::enter($view, || ($name).into());
    };
}
#[cfg(not(feature = "cost-profile"))]
#[macro_export]
macro_rules! cost_scope {
    ($view:expr, $name:expr) => {};
}

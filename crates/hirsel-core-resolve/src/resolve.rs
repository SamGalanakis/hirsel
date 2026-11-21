use crate::{index::path_tokens, *};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, Symbol, TokenId, sym};

/// Expression nesting the resolver walks before it refuses the item as a resource
/// refusal (PROG-6).
pub const RESOLVE_NESTING_LIMIT: u32 = 384;
// Two MiB of generation-stamped words at most. Larger scope/name products
// use the packed-key set, so this is a representation choice, not a tool limit.
const LOCAL_READ_WORD_LIMIT: usize = 131_072;
const MODULE_HEAD_COLUMN: usize = 8;
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BinderKind {
    Local,
    Parameter,
    TypeParameter,
    Region,
    Label,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Binder {
    pub id: u32,
    pub name: Symbol,
    pub kind: BinderKind,
    pub site: Site,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Target {
    Definition(DefinitionId),
    Module(ModuleId),
    Binder(u32),
    Builtin(Symbol),
    ContextualConstructor { name: Symbol },
    Missing,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LookupScope {
    Local(u32),
    Bind(u32),
    Label(u32),
    Module(ModuleId),
    Prelude(PackageKey),
    Modules(PackageKey),
    Children(ModuleId),
    Variants(ModuleId),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LookupResult {
    Found(Target),
    Missing,
    Private(DefinitionId),
    Opaque(DefinitionId),
    Shadowed(Target),
    Ambiguous,
}
/// One dependency fact per distinct scope/name/namespace/mode in this item.
/// The first site is retained; a use's field suffix stays on the original token tape.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lookup {
    pub node: NodeId,
    pub scope: LookupScope,
    pub symbol: Symbol,
    pub namespace: Namespace,
    pub constructor: bool,
    pub result: LookupResult,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedUse {
    pub node: NodeId,
    pub target: Target,
    pub projections: Span,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Resolution {
    pub names: Vec<ResolvedUse>,
    pub binders: Vec<Binder>,
    pub reads: Vec<Lookup>,
    pub diagnostics: Vec<Refusal>,
    /// The node whose nesting reaches [`RESOLVE_NESTING_LIMIT`], and that limit.
    pub resource_refusal: Option<(Site, u32)>,
}
pub(crate) fn builtin(n: Symbol) -> bool {
    matches!(
        n,
        sym::INLINE
            | sym::SIMD
            | sym::BOOL
            | sym::CHAR
            | sym::U8
            | sym::U16
            | sym::U32
            | sym::U64
            | sym::I8
            | sym::I16
            | sym::I32
            | sym::I64
            | sym::F32
            | sym::F64
    )
}
#[derive(Clone, Copy)]
struct Binding {
    symbol: Symbol,
    namespace: Namespace,
    previous: u32,
}
/// Resolve one borrowed item. The visitor allocates no child/path/scope objects.
/// Result tables and scratch storage are reserved once, before traversal.
pub fn resolve(
    index: &NameIndex,
    module: ModuleId,
    source: SourceId,
    ordinal: u32,
    item: Item<'_>,
) -> Resolution {
    ResolutionScratch::default().resolve(index, module, source, ordinal, item)
}

/// Request-owned work tables reused across items. Results own only their output rows.
#[derive(Default)]
pub struct ResolutionScratch {
    bindings: Vec<Binding>,
    slots: Vec<u32>,
    labels: Vec<(Symbol, u32, NodeKind)>,
    loops: Vec<NodeKind>,
    seen: ReadSet,
    recorded: Vec<[u32; 10]>,
    module_heads: Vec<Option<ModuleId>>,
    local_words: Vec<ReadWord>,
    local_stride: usize,
    local_limit: usize,
}
impl ResolutionScratch {
    pub fn resolve(
        &mut self,
        index: &NameIndex,
        module: ModuleId,
        source: SourceId,
        ordinal: u32,
        item: Item<'_>,
    ) -> Resolution {
        if item.range.has_errors {
            return Resolution::default();
        }
        let size = item.range.resolution_size;
        let queries = size.names as usize
            + size.binders as usize
            + size.labels as usize
            + size.path_extra as usize
            + size.exits as usize;
        let read_capacity = queries * 6;
        // Each name may be queried in lexical versions and in fixed index scopes.
        // Use the smaller proven key bound for the dedup table, rather than one bucket
        // per occurrence in a large body that repeats the same few names.
        let versions = size.scopes as usize + size.binders as usize + 1;
        let scope_keys = 5 * versions + 5 * index.modules.len() + index.packages.len() + 5;
        let dedup_capacity = read_capacity.min(index.symbols.len().saturating_mul(scope_keys));
        self.slots.resize(index.symbols.len() * 2, u32::MAX);
        self.bindings.reserve(size.binders as usize);
        self.labels.reserve(size.labels as usize);
        self.loops.reserve(size.loops as usize);
        self.seen.reset(dedup_capacity);
        self.local_stride = index.symbols.len().div_ceil(64);
        let local_limit = self.local_stride.saturating_mul(versions).saturating_mul(2);
        self.local_limit = if local_limit <= LOCAL_READ_WORD_LIMIT {
            local_limit
        } else {
            0
        };
        if self.local_words.len() < self.local_limit {
            self.local_words
                .resize(self.local_limit, ReadWord::default());
        }
        if self.seen.generation == 1 {
            self.local_words.fill(ReadWord::default());
        }
        self.recorded.resize(index.symbols.len(), [0; 10]);
        self.module_heads.resize(index.symbols.len(), None);
        if self.seen.generation == 1 {
            self.recorded.fill([0; 10]);
        }
        let package = index.module_package(module);
        let mut r = Resolver {
            index,
            module,
            package,
            item,
            source,
            ordinal,
            scratch: self,
            label_base: 0,
            loop_base: 0,
            numeric_parameter: false,
            numeric_declaration: Some(package) == index.core
                && matches!(
                    item.tree.node(item.root).kind,
                    NodeKind::Function
                        | NodeKind::CloneHook
                        | NodeKind::TypeDecl
                        | NodeKind::ResourceType
                        | NodeKind::ScopedType
                ),
            scope_id: 0,
            next_scope: 1,
            depth: 0,
            paths: [None; 8],

            out: Resolution {
                names: Vec::with_capacity(size.names as usize),
                binders: Vec::with_capacity(size.binders as usize),
                reads: Vec::with_capacity(read_capacity),
                ..Resolution::default()
            },
        };
        if Some(package) != index.core
            && !index.modules[module.0 as usize].foreign
            && item.tree.node(item.root).kind == NodeKind::Function
            && !item
                .tree
                .children(item.root)
                .iter()
                .any(|&c| item.tree.node(c).kind == NodeKind::Block)
        {
            let end = item.tree.node(item.root).origin.span.end;
            r.out.diagnostics.push(
                Refusal::new(
                    "LANG-5",
                    Site {
                        module,
                        source: Some(source),
                        item: ordinal,
                        span: Span { start: end, end },
                    },
                    "a function outside core requires a body",
                )
                .with_comparison(
                    "function without a body",
                    "function body outside package core",
                ),
            );
        }
        r.visit(item.root);
        while let Some(b) = r.scratch.bindings.pop() {
            r.scratch.slots[Resolver::slot(b.symbol, b.namespace)] = b.previous;
        }
        r.scratch.labels.clear();
        r.scratch.loops.clear();
        r.out
    }
}
struct Resolver<'a, 's> {
    index: &'a NameIndex,
    module: ModuleId,
    package: PackageKey,
    item: Item<'a>,
    source: SourceId,
    ordinal: u32,
    scratch: &'s mut ResolutionScratch,
    label_base: usize,
    loop_base: usize,
    numeric_parameter: bool,
    numeric_declaration: bool,
    scope_id: u32,
    next_scope: u32,
    depth: u32,
    paths: [Option<((u32, Symbol, bool, bool, bool), Target)>; 8],

    out: Resolution,
}
impl<'a> Resolver<'a, '_> {
    fn site(&self, n: NodeId) -> Site {
        Site {
            module: self.module,
            source: Some(self.source),
            item: self.ordinal,
            span: self.item.tree.node(n).origin.span,
        }
    }
    fn name(&self, t: TokenId) -> Symbol {
        self.item.symbol(t)
    }
    fn children(&self, n: NodeId) -> &'a [NodeId] {
        self.item.tree.children(n)
    }
    #[inline(always)]
    fn record(
        &mut self,
        n: NodeId,
        namespace: Namespace,
        scope: LookupScope,
        symbol: Symbol,
        constructor: bool,
        result: LookupResult,
    ) {
        // Current-module and prelude lookups have four namespace/mode keys.
        // Module-head and variant queries always use Value/false and Value/true.
        let mode = (namespace == Namespace::Value) as usize * 2 + constructor as usize;
        let dense = match scope {
            LookupScope::Module(module) if module == self.module => Some(mode),
            LookupScope::Prelude(_) => Some(4 + mode),
            LookupScope::Modules(_) => Some(MODULE_HEAD_COLUMN),
            LookupScope::Variants(_) => Some(9),
            _ => None,
        };
        let local = if let LookupScope::Local(id) = scope {
            (id as usize * 2 + (namespace == Namespace::Value) as usize)
                .saturating_mul(self.scratch.local_stride)
                .saturating_add(symbol.0 as usize / 64)
        } else {
            usize::MAX
        };
        let fresh = if local < self.scratch.local_limit {
            let word = &mut self.scratch.local_words[local];
            let bit = 1u64 << (symbol.0 & 63);
            if word.generation != self.scratch.seen.generation {
                word.generation = self.scratch.seen.generation;
                word.bits = 0;
            }
            if word.bits & bit != 0 {
                return;
            }
            word.bits |= bit;
            true
        } else if let Some(column) = dense {
            let stamp = &mut self.scratch.recorded[symbol.0 as usize][column];
            if *stamp == self.scratch.seen.generation {
                return;
            }
            *stamp = self.scratch.seen.generation;
            true
        } else {
            self.scratch
                .seen
                .insert(lookup_key(scope, symbol, namespace, constructor))
        };
        if fresh {
            self.out.reads.push(Lookup {
                node: n,
                scope,
                symbol,
                namespace,
                constructor,
                result,
            });
        }
    }
    fn slot(name: Symbol, ns: Namespace) -> usize {
        name.0 as usize * 2 + usize::from(ns == Namespace::Value)
    }
    fn local_in(&self, name: Symbol, ns: Namespace) -> Option<u32> {
        let b = self.scratch.slots[Self::slot(name, ns)];
        (b != u32::MAX).then_some(b)
    }
    fn scope(&mut self, f: impl FnOnce(&mut Self)) {
        let mark = self.scratch.bindings.len();
        let old = self.scope_id;
        self.scope_id = self.next_scope;
        self.next_scope += 1;
        f(self);
        for b in self.scratch.bindings.drain(mark..) {
            self.scratch.slots[Self::slot(b.symbol, b.namespace)] = b.previous;
        }
        self.scope_id = old;
    }
    fn bind(&mut self, n: NodeId, t: TokenId, kind: BinderKind) {
        let name = self.name(t);
        let ns = if kind == BinderKind::TypeParameter {
            Namespace::Type
        } else {
            Namespace::Value
        };
        if ns == Namespace::Type && builtin(name) {
            self.out.diagnostics.push(
                Refusal::new(
                    "LANG-7",
                    self.site(n),
                    "builtin type name cannot be redeclared",
                )
                .with_comparison(self.index.symbols.text(name), "a fresh type-parameter name"),
            );
            return;
        }
        let local = self.local_in(name, ns);
        self.record(
            n,
            ns,
            LookupScope::Local(self.scope_id),
            name,
            false,
            local.map_or(LookupResult::Missing, |b| {
                LookupResult::Found(Target::Binder(b))
            }),
        );
        let mut existing = local.map(Target::Binder);
        if existing.is_none() && ns == Namespace::Value {
            existing = self
                .module_lookup(n, self.module, name, ns, false)
                .or_else(|| self.prelude_lookup(n, name, ns, false));
        }
        if let Some(target) = existing {
            self.record(
                n,
                ns,
                LookupScope::Bind(self.scope_id),
                name,
                false,
                LookupResult::Shadowed(target),
            );
            let mut d = Refusal::new(
                "LANG-7",
                self.site(n),
                format!("`{}` shadows a visible name", self.index.symbols.text(name)),
            );
            match target {
                Target::Binder(b) => d.causal.push(self.out.binders[b as usize].site),
                Target::Definition(id) => d.causal.push(self.index.get(id).site),
                _ => {}
            }
            d.declared = Some(self.index.symbols.text(name).into());
            d.required = Some("a name not already visible in this namespace".into());
            self.out.diagnostics.push(d);
            return;
        }
        let id = self.out.binders.len() as u32;
        self.out.binders.push(Binder {
            id,
            name,
            kind,
            site: self.site(n),
        });
        let slot = Self::slot(name, ns);
        self.scratch.bindings.push(Binding {
            symbol: name,
            namespace: ns,
            previous: self.scratch.slots[slot],
        });
        self.scratch.slots[slot] = id;
        self.scope_id = self.next_scope;
        self.next_scope += 1;
    }
    fn selected(
        &mut self,
        n: NodeId,
        scope: LookupScope,
        name: Symbol,
        ns: Namespace,
        construct: bool,
        candidate: Option<(DefinitionId, &Definition)>,
    ) -> Option<Target> {
        let Some((id, d)) = candidate else {
            self.record(n, ns, scope, name, construct, LookupResult::Missing);
            return None;
        };
        let result = if d.site.module != self.module && !d.public {
            LookupResult::Private(id)
        } else if construct && d.site.module != self.module && d.opaque {
            LookupResult::Opaque(id)
        } else {
            LookupResult::Found(Target::Definition(id))
        };
        if matches!(result, LookupResult::Private(_) | LookupResult::Opaque(_)) {
            let mut refusal = Refusal::new(
                "PROG-2",
                self.site(n),
                format!("`{}` is private or opaque", self.index.definition_path(id)),
            );
            refusal.causal.push(d.site);
            if matches!(result, LookupResult::Private(_)) {
                refusal = refusal.with_comparison(
                    "private definition",
                    "public definition across module boundary",
                );
            } else {
                refusal = refusal.with_comparison(
                    "opaque type",
                    "transparent type for construction outside its module",
                );
            }
            self.out.diagnostics.push(refusal);
        }
        self.record(n, ns, scope, name, construct, result);
        Some(Target::Definition(id))
    }
    fn module_lookup(
        &mut self,
        n: NodeId,
        module: ModuleId,
        name: Symbol,
        ns: Namespace,
        construct: bool,
    ) -> Option<Target> {
        let candidate = self
            .index
            .member_row(module, name, ns, construct)
            .filter(|&(_, d)| {
                !construct || d.kind != DefinitionKind::Type || d.variants.start == d.variants.end
            });
        let target = candidate.map(|(id, _)| Target::Definition(id));
        // This is a direct member of the current module. Cross-module visibility
        // is handled by qualified_member, whose candidate can be an extension.
        self.record(
            n,
            ns,
            LookupScope::Module(module),
            name,
            construct,
            target.map_or(LookupResult::Missing, LookupResult::Found),
        );
        target
    }
    fn prelude_lookup(
        &mut self,
        n: NodeId,
        name: Symbol,
        ns: Namespace,
        construct: bool,
    ) -> Option<Target> {
        let core = self.index.core?;
        let candidate = self
            .index
            .prelude_row(name, ns, construct)
            .filter(|&(_, d)| {
                !construct || d.kind != DefinitionKind::Type || d.variants.start == d.variants.end
            });
        self.selected(
            n,
            LookupScope::Prelude(core),
            name,
            ns,
            construct,
            candidate,
        )
    }
    fn head_lookup(&mut self, n: NodeId, name: Symbol) -> Option<Target> {
        // The first lookup records the dependency. Its stamp also marks the
        // result for the rest of the item, whose package and index are fixed.
        let symbol = name.0 as usize;
        if self.scratch.recorded[symbol][MODULE_HEAD_COLUMN] == self.scratch.seen.generation {
            return self.scratch.module_heads[symbol].map(Target::Module);
        }
        let m = self.index.module_head_from(self.package, name);
        self.scratch.module_heads[symbol] = m;
        self.record(
            n,
            Namespace::Value,
            LookupScope::Modules(self.package),
            name,
            false,
            m.map_or(LookupResult::Missing, |m| {
                LookupResult::Found(Target::Module(m))
            }),
        );
        m.map(Target::Module)
    }
    fn has_variant(&self, name: Symbol) -> bool {
        self.index.variants(name).any(|id| {
            let d = self.index.get(id);
            d.site.module == self.module
                || d.public
                    && !d.opaque
                    && self
                        .index
                        .reachable_from(self.package, self.index.module_package(d.site.module))
        })
    }
    fn constructor(&mut self, n: NodeId, name: Symbol) -> Target {
        let target = Target::ContextualConstructor { name };
        self.record(
            n,
            Namespace::Value,
            LookupScope::Variants(self.module),
            name,
            true,
            LookupResult::Found(target),
        );
        target
    }
    fn variant_or_missing(&mut self, n: NodeId, name: Symbol) -> Target {
        if self.has_variant(name) {
            self.constructor(n, name)
        } else {
            self.record(
                n,
                Namespace::Value,
                LookupScope::Variants(self.module),
                name,
                true,
                LookupResult::Missing,
            );
            self.missing(n)
        }
    }
    fn missing(&mut self, n: NodeId) -> Target {
        self.missing_name(n, None)
    }
    fn missing_name(&mut self, n: NodeId, lookup: Option<(Symbol, NameScope)>) -> Target {
        let path = path_tokens(self.item, n)
            .map(|t| self.index.symbols.text(self.name(t)))
            .collect::<Vec<_>>()
            .join(".");
        let mut refusal = Refusal::new(
            "LANG-7",
            self.site(n),
            format!("name `{path}` was not found"),
        );
        if let Some((written, scope)) = lookup {
            refusal = refusal.with_unresolved(self.index.symbols.text(written), scope);
        }
        self.out.diagnostics.push(refusal);
        Target::Missing
    }
    fn qualified_member(
        &mut self,
        n: NodeId,
        module: ModuleId,
        token: TokenId,
        ns: Namespace,
        construct: bool,
    ) -> Option<Target> {
        let name = self.name(token);
        let mut candidates =
            self.index
                .member_rows_from(self.module, self.package, module, name, ns, false);
        let mut first = candidates.next();
        let mut second = candidates.next();
        if first.is_none() && construct {
            let mut cs = self.index.member_rows_from(
                self.module,
                self.package,
                module,
                name,
                Namespace::Type,
                true,
            );
            first = cs.next();
            second = cs.next();
        }
        if second.is_some() {
            self.record(
                n,
                ns,
                LookupScope::Module(module),
                name,
                construct,
                LookupResult::Ambiguous,
            );
            let mut d = Refusal::new("LANG-7", self.site(n), "ambiguous module extension member");
            d = d.with_comparison(
                "multiple visible extension members",
                "one unambiguous member",
            );
            d.causal.extend(
                self.index
                    .member_rows_from(self.module, self.package, module, name, ns, construct)
                    .map(|(_, d)| d.site),
            );
            self.out.diagnostics.push(d);
            Some(Target::Missing)
        } else {
            self.selected(n, LookupScope::Module(module), name, ns, construct, first)
        }
    }
    fn function_head(&mut self, n: NodeId, token: TokenId, id: DefinitionId) -> Target {
        if self.foreign_use(n, token, Target::Definition(id), false) == Target::Missing {
            return Target::Missing;
        }
        let mut diagnostic = Refusal::new(
            "LANG-7",
            self.site(n),
            "a function cannot be the head of a dotted path",
        );
        diagnostic.primary.span = self.item.token_span(token);
        diagnostic.causal.push(self.index.get(id).site);
        diagnostic = diagnostic.with_comparison(
            "function",
            "local, module, package or constant as a dotted-path head",
        );
        self.out.diagnostics.push(diagnostic);
        Target::Missing
    }
    fn bare_target<const TYPE_CONTEXT: bool, const CONSTRUCT: bool>(
        &mut self,
        n: NodeId,
        name: Symbol,
    ) -> Target {
        let type_context = TYPE_CONTEXT;
        let construct = CONSTRUCT;
        if type_context && builtin(name) {
            return Target::Builtin(name);
        }
        let ns = if type_context {
            Namespace::Type
        } else {
            Namespace::Value
        };
        let local = self.local_in(name, ns);
        self.record(
            n,
            ns,
            LookupScope::Local(self.scope_id),
            name,
            false,
            local.map_or(LookupResult::Missing, |b| {
                LookupResult::Found(Target::Binder(b))
            }),
        );
        if let Some(b) = local {
            return Target::Binder(b);
        }
        if let Some(t) = self
            .module_lookup(n, self.module, name, ns, false)
            .or_else(|| self.prelude_lookup(n, name, ns, false))
        {
            return t;
        }
        if construct && self.has_variant(name) {
            return self.constructor(n, name);
        }
        if construct {
            return self
                .module_lookup(n, self.module, name, Namespace::Type, true)
                .or_else(|| self.prelude_lookup(n, name, Namespace::Type, true))
                .unwrap_or_else(|| self.variant_or_missing(n, name));
        }
        if !type_context {
            self.head_lookup(n, name)
                .unwrap_or_else(|| self.variant_or_missing(n, name))
        } else {
            self.missing(n)
        }
    }
    fn module_path<const TYPE_CONTEXT: bool, const CONSTRUCT: bool>(
        &mut self,
        n: NodeId,
        mut module: ModuleId,
        mut tail: impl Iterator<Item = TokenId> + Clone,
        last: TokenId,
        projections: &mut Span,
    ) -> Target {
        let mut package_root = None;
        if let Some(p) = self.index.path_package(module)
            && let Some(root) = self.index.root_module(p)
        {
            let second = tail.clone().next().unwrap();
            if self.name(second) == p.name {
                let mut site = self.site(n);
                site.span = self.item.token_span(second);
                self.out.diagnostics.push(Refusal::new(
                    "LANG-7",
                    site,
                    format!("name `{}` was not found", self.index.symbols.text(p.name)),
                ));
                return Target::Missing;
            }
            module = root;
            package_root = Some(p.root);
        }
        while let Some(t) = tail.next() {
            if t == last {
                break;
            }
            let symbol = self.name(t);
            let (scope, child) = if let Some(root) = package_root.take() {
                (
                    root,
                    self.index
                        .child(root, symbol)
                        .or_else(|| self.index.child(module, symbol)),
                )
            } else {
                (module, self.index.child(module, symbol))
            };
            self.record(
                n,
                Namespace::Value,
                LookupScope::Children(scope),
                symbol,
                false,
                child.map_or(LookupResult::Missing, |m| {
                    LookupResult::Found(Target::Module(m))
                }),
            );
            if let Some(child) = child {
                module = child;
                continue;
            }
            let value = self
                .qualified_member(n, module, t, Namespace::Value, false)
                .unwrap_or_else(|| {
                    self.missing_name(
                        n,
                        Some((
                            symbol,
                            NameScope::ModulePrefix {
                                module: self.index.module_path(module),
                                children_of: self.index.module_path(scope),
                                namespace: if TYPE_CONTEXT {
                                    Namespace::Type
                                } else {
                                    Namespace::Value
                                },
                            },
                        )),
                    )
                });
            if let Target::Definition(id) = value {
                match self.index.get(id).kind {
                    DefinitionKind::Function => return self.function_head(n, t, id),
                    DefinitionKind::Constant => {
                        *projections = Span {
                            start: tail.next().unwrap().0,
                            end: self.item.tree.node(n).origin.tokens.end,
                        };
                    }
                    _ => {}
                }
            }
            return value;
        }
        let ns = if TYPE_CONTEXT {
            Namespace::Type
        } else {
            Namespace::Value
        };
        self.qualified_member(n, module, last, ns, CONSTRUCT)
            .unwrap_or_else(|| {
                self.missing_name(
                    n,
                    Some((
                        self.name(last),
                        NameScope::ModuleMember {
                            module: self.index.module_path(module),
                            namespace: ns,
                            constructor: CONSTRUCT,
                        },
                    )),
                )
            })
    }
    fn foreign_use(&mut self, n: NodeId, name: TokenId, target: Target, callee: bool) -> Target {
        let Target::Definition(id) = target else {
            return target;
        };
        let declared = self.index.get(id).site;
        let module = &self.index.modules[declared.module.0 as usize];
        if !module.foreign {
            return target;
        }
        let message = if module.package != self.package {
            "a foreign function is private to its package"
        } else if !callee {
            "a foreign function is only called"
        } else {
            return target;
        };
        let mut site = self.site(n);
        site.span = self.item.token_span(name);
        let mut refusal = Refusal::new("FFI-2", site, message);
        refusal.causal.push(declared);
        self.out.diagnostics.push(refusal);
        Target::Missing
    }

    fn use_path<const TYPE_CONTEXT: bool, const CONSTRUCT: bool, const CALLEE: bool>(
        &mut self,
        n: NodeId,
    ) {
        let type_context = TYPE_CONTEXT;
        let construct = CONSTRUCT;
        let node = self.item.tree.node(n);
        let simple = node.origin.tokens.end == node.origin.tokens.start + 1;
        let name = self.name(node.token.unwrap());
        if simple && type_context && builtin(name) {
            self.out.names.push(ResolvedUse {
                node: n,
                target: Target::Builtin(name),
                projections: Span::default(),
            });
            return;
        }
        let key = (self.scope_id, name, type_context, construct, CALLEE);
        if simple {
            if let Some((old, target)) = self.paths[key.1.0 as usize & 7] {
                if old == key {
                    self.out.names.push(ResolvedUse {
                        node: n,
                        target,
                        projections: Span::default(),
                    });
                    return;
                }
            }
        }
        if simple {
            let before = self.out.diagnostics.len();
            let target = self.bare_target::<TYPE_CONTEXT, CONSTRUCT>(n, key.1);
            let target = self.foreign_use(n, node.token.unwrap(), target, CALLEE);
            if self.out.diagnostics.len() == before && target != Target::Missing {
                self.paths[key.1.0 as usize & 7] = Some((key, target));
            }
            self.out.names.push(ResolvedUse {
                node: n,
                target,
                projections: Span::default(),
            });
            return;
        }
        let mut parts = path_tokens(self.item, n);
        let Some(head_token) = parts.next() else {
            return;
        };
        let name = self.name(head_token);
        let rest = parts.clone();
        let last = parts.clone().last();
        let ns = if type_context {
            Namespace::Type
        } else {
            Namespace::Value
        };
        let first_ns = if last.is_some() { Namespace::Value } else { ns };
        let mut projections = Span::default();
        let target = if type_context && last.is_none() && builtin(name) {
            Target::Builtin(name)
        } else {
            let local = self.local_in(name, first_ns);
            self.record(
                n,
                first_ns,
                LookupScope::Local(self.scope_id),
                name,
                false,
                local.map_or(LookupResult::Missing, |b| {
                    LookupResult::Found(Target::Binder(b))
                }),
            );
            if let Some(b) = local {
                if let Some(p) = rest.clone().next() {
                    projections = Span {
                        start: p.0,
                        end: self.item.tree.node(n).origin.tokens.end,
                    };
                }
                Target::Binder(b)
            } else if let Some(last) = last {
                let first = self
                    .head_lookup(n, name)
                    .or_else(|| self.module_lookup(n, self.module, name, Namespace::Value, false));
                match first {
                    Some(Target::Module(module)) => self.module_path::<TYPE_CONTEXT, CONSTRUCT>(
                        n,
                        module,
                        rest.clone(),
                        last,
                        &mut projections,
                    ),
                    Some(Target::Definition(id))
                        if self.index.get(id).kind == DefinitionKind::Function =>
                    {
                        self.function_head(n, head_token, id)
                    }
                    Some(t) => {
                        projections = Span {
                            start: rest.clone().next().unwrap().0,
                            end: self.item.tree.node(n).origin.tokens.end,
                        };
                        t
                    }
                    None => {
                        let message = if self.index.packages[self.package.0 as usize].name == name {
                            let bare = rest
                                .map(|t| self.index.symbols.text(self.name(t)))
                                .collect::<Vec<_>>()
                                .join(".");
                            format!(
                                "`{}` is this package; name its module bare: `{bare}`",
                                self.index.symbols.text(name)
                            )
                        } else {
                            format!(
                                "`{}` is not a local, module, package or constant",
                                self.index.symbols.text(name)
                            )
                        };
                        let mut diagnostic = Refusal::new("LANG-7", self.site(n), message);
                        diagnostic.primary.span = self.item.token_span(head_token);
                        self.out.diagnostics.push(diagnostic);
                        Target::Missing
                    }
                }
            } else if let Some(t) = self
                .module_lookup(n, self.module, name, ns, false)
                .or_else(|| self.prelude_lookup(n, name, ns, false))
            {
                t
            } else if construct && self.has_variant(name) {
                self.constructor(n, name)
            } else if construct {
                self.module_lookup(n, self.module, name, Namespace::Type, true)
                    .or_else(|| self.prelude_lookup(n, name, Namespace::Type, true))
                    .unwrap_or_else(|| self.variant_or_missing(n, name))
            } else if !type_context {
                self.head_lookup(n, name)
                    .unwrap_or_else(|| self.variant_or_missing(n, name))
            } else {
                self.missing(n)
            }
        };
        let target = self.foreign_use(n, last.unwrap_or(head_token), target, CALLEE);
        self.out.names.push(ResolvedUse {
            node: n,
            target,
            projections,
        });
    }
    fn pattern(&mut self, n: NodeId) {
        let node = self.item.tree.node(n);
        let token = node.token;
        match node.kind {
            NodeKind::BindPattern => {
                let t = token.unwrap();
                let name = self.name(t);
                if self.has_variant(name) {
                    let target = self.constructor(n, name);
                    self.out.names.push(ResolvedUse {
                        node: n,
                        target,
                        projections: Span::default(),
                    });
                } else {
                    self.record(
                        n,
                        Namespace::Value,
                        LookupScope::Variants(self.module),
                        name,
                        true,
                        LookupResult::Missing,
                    );
                    self.bind(n, t, BinderKind::Local);
                }
            }
            NodeKind::TakePattern | NodeKind::MutPattern => {
                self.bind(n, token.unwrap(), BinderKind::Local)
            }
            NodeKind::ConstructorPattern => {
                let c = self.children(n);
                if let Some((&path, fields)) = c.split_first() {
                    self.use_path::<false, true, false>(path);
                    for &f in fields {
                        self.pattern(f);
                    }
                }
            }
            NodeKind::PatternField | NodeKind::TuplePattern => {
                for &c in self.children(n) {
                    self.pattern(c)
                }
            }
            _ => {}
        }
    }
    fn function(&mut self, n: NodeId) {
        let old = self.label_base;
        let old_loop = self.loop_base;
        self.label_base = self.scratch.labels.len();
        self.loop_base = self.scratch.loops.len();
        self.scope(|r| {
            let children = r.children(n);
            for &c in children {
                if r.item.tree.node(c).kind == NodeKind::Generics {
                    r.visit(c);
                }
            }
            for &c in children {
                if r.item.tree.node(c).kind == NodeKind::Parameters {
                    for &p in r.children(c) {
                        if let Some(t) = r.item.tree.node(p).token {
                            r.bind(p, t, BinderKind::Parameter);
                        }
                    }
                }
            }
            for &c in children {
                if r.item.tree.node(c).kind != NodeKind::Generics {
                    r.visit(c);
                }
            }
        });
        self.label_base = old;
        self.loop_base = old_loop;
    }
    fn condition(&mut self, n: NodeId) {
        if self.item.tree.node(n).kind == NodeKind::ConditionLet {
            let c = self.children(n);
            self.visit(c[1]);
            self.pattern(c[0]);
        } else {
            self.visit(n);
        }
    }
    fn indexed_callee(&mut self, n: NodeId) {
        if self.depth >= RESOLVE_NESTING_LIMIT {
            self.out.resource_refusal = Some((self.site(n), RESOLVE_NESTING_LIMIT));
            return;
        }
        self.depth += 1;
        let c = self.item.tree.children(n);
        let before = self.out.names.len();
        self.visit(c[0]);
        let target = self.out.names[before..]
            .iter()
            .find(|u| u.node == c[0])
            .map(|u| u.target);
        if matches!(target, Some(Target::Definition(id)) if self.index.get(id).kind == DefinitionKind::Function)
        {
            let base = self.item.tree.node(c[0]);
            let bracket = self.item.tree.tokens[base.origin.tokens.end as usize..]
                .iter()
                .find(|t| t.symbol == sym::LBRACKET)
                .unwrap();
            let mut site = self.site(n);
            site.span.start = bracket.span.start - self.item.file_offset;
            // Carry the refusal into semantic recovery so this callee is not
            // checked again as a generic function value.
            self.out.names[before..]
                .iter_mut()
                .find(|u| u.node == c[0])
                .unwrap()
                .target = Target::Missing;
            self.out.diagnostics.push(Refusal::new("LANG-12", site,
            "type arguments are not written at a call; give the result type in an ascription `(callee() : ResultType)` or a binding, or give it in a typed argument"));
        } else {
            for &child in &c[1..] {
                self.visit(child);
            }
        }
        self.depth -= 1;
    }
    fn visit(&mut self, mut n: NodeId) {
        let initial_depth = self.depth;
        loop {
            if self.out.resource_refusal.is_some() {
                break;
            }
            match self.item.tree.node(n).kind {
                NodeKind::Literal => break,
                NodeKind::Path => {
                    self.use_path::<false, false, false>(n);
                    break;
                }
                NodeKind::TypePath => {
                    self.use_path::<true, false, false>(n);
                    break;
                }
                _ => {}
            }
            if self.depth >= RESOLVE_NESTING_LIMIT {
                self.out.resource_refusal = Some((self.site(n), RESOLVE_NESTING_LIMIT));
                break;
            }
            self.depth += 1;

            let node = self.item.tree.node(n);
            let kind = node.kind;
            let token = node.token;
            let c = self.children(n);
            match kind {
                NodeKind::Function
                | NodeKind::CloneHook
                | NodeKind::Closure
                | NodeKind::ScopedClosure
                | NodeKind::CallableType
                | NodeKind::ScopedCallableType
                | NodeKind::OwnCallableType => self.function(n),
                NodeKind::TypeDecl
                | NodeKind::ResourceType
                | NodeKind::ScopedType
                | NodeKind::Block => self.scope(|r| {
                    for &child in c {
                        r.visit(child)
                    }
                }),
                NodeKind::Generic | NodeKind::RegionGeneric => {
                    self.bind(
                        n,
                        token.unwrap(),
                        if kind == NodeKind::Generic {
                            BinderKind::TypeParameter
                        } else {
                            BinderKind::Region
                        },
                    );
                    let old = self.numeric_parameter;
                    self.numeric_parameter = kind == NodeKind::Generic;
                    for &child in c {
                        self.visit(child);
                    }
                    self.numeric_parameter = old;
                }
                NodeKind::Path => self.use_path::<false, false, false>(n),
                NodeKind::TypePath => self.use_path::<true, false, false>(n),
                NodeKind::Let | NodeKind::LetMut => {
                    for &child in &c[1..] {
                        self.visit(child);
                    }
                    self.pattern(c[0]);
                }
                NodeKind::For | NodeKind::ForTake => {
                    self.visit(c[1]);
                    self.scope(|r| {
                        r.pattern(c[0]);
                        r.scratch.loops.push(kind);
                        r.visit(c[2]);
                        r.scratch.loops.pop();
                    });
                }
                NodeKind::If | NodeKind::While => {
                    self.scope(|r| {
                        r.condition(c[0]);
                        if kind == NodeKind::While {
                            r.scratch.loops.push(kind);
                        }
                        r.visit(c[1]);
                        if kind == NodeKind::While {
                            r.scratch.loops.pop();
                        }
                    });
                    for &child in &c[2..] {
                        self.visit(child);
                    }
                }
                NodeKind::Loop => {
                    self.scratch.loops.push(kind);
                    for &child in c {
                        self.visit(child);
                    }
                    self.scratch.loops.pop();
                }
                NodeKind::Arm => self.scope(|r| {
                    r.pattern(c[0]);
                    for &child in &c[1..] {
                        r.visit(child);
                    }
                }),
                NodeKind::Region | NodeKind::Drop => self.scope(|r| {
                    r.bind(
                        n,
                        token.unwrap(),
                        if kind == NodeKind::Region {
                            BinderKind::Region
                        } else {
                            BinderKind::Parameter
                        },
                    );
                    for &child in c {
                        r.visit(child);
                    }
                }),
                NodeKind::Call => {
                    if let Some((&callee, args)) = c.split_first() {
                        let before = self.out.names.len();
                        if self.item.tree.node(callee).kind == NodeKind::Path {
                            self.use_path::<false, true, true>(callee);
                        } else if self.item.tree.node(callee).kind == NodeKind::Index {
                            self.indexed_callee(callee);
                        } else {
                            self.visit(callee);
                        }
                        for &arg in args {
                            self.visit(arg);
                        }
                        if args
                            .iter()
                            .any(|&a| self.item.tree.node(a).kind == NodeKind::LabelledArgument)
                        {
                            let target = self.out.names[before..]
                                .iter()
                                .find(|u| u.node == callee)
                                .map(|u| u.target);
                            if matches!(target,Some(Target::Definition(id)) if self.index.get(id).kind==DefinitionKind::Function)
                                || matches!(target, Some(Target::Binder(_)))
                            {
                                self.out.diagnostics.push(
                                    Refusal::new(
                                        "LANG-9",
                                        self.site(n),
                                        "ordinary calls have no labelled arguments",
                                    )
                                    .with_comparison(
                                        "labelled arguments",
                                        "positional arguments in an ordinary call",
                                    ),
                                );
                            }
                        }
                    }
                }
                NodeKind::Labelled => {
                    let name = self.name(token.unwrap());
                    let old = self.scratch.labels[self.label_base..]
                        .iter()
                        .rev()
                        .find(|&&(s, _, _)| s == name)
                        .map(|&(_, b, _)| b);
                    if let Some(b) = old {
                        let mut d =
                            Refusal::new("LANG-15", self.site(n), "active label is shadowed");
                        d.causal.push(self.out.binders[b as usize].site);
                        d = d.with_comparison(
                            self.index.symbols.text(name),
                            "a label name not already active in this function",
                        );
                        self.out.diagnostics.push(d);
                    }
                    let id = self.out.binders.len() as u32;
                    self.out.binders.push(Binder {
                        id,
                        name,
                        kind: BinderKind::Label,
                        site: self.site(n),
                    });
                    self.scratch
                        .labels
                        .push((name, id, self.item.tree.node(c[0]).kind));
                    for &child in c {
                        self.visit(child);
                    }
                    self.scratch.labels.pop();
                }
                NodeKind::Break | NodeKind::Continue => {
                    let target = if let Some(t) = token {
                        let name = self.name(t);
                        self.scratch.labels[self.label_base..]
                            .iter()
                            .rev()
                            .find(|&&(s, _, _)| s == name)
                            .map(|&(_, _, k)| k)
                    } else {
                        self.scratch.loops[self.loop_base..].last().copied()
                    };
                    if token.is_none() && target.is_none() {
                        self.out.diagnostics.push(Refusal::new(
                            "LANG-15",
                            self.site(n),
                            "exit has no enclosing loop",
                        ));
                    } else if kind == NodeKind::Continue && target == Some(NodeKind::Block) {
                        self.out.diagnostics.push(
                            Refusal::new("LANG-15", self.site(n), "continue target is a block")
                                .with_comparison("labelled block", "loop target for continue"),
                        );
                    } else if kind == NodeKind::Break
                        && !c.is_empty()
                        && matches!(
                            target,
                            Some(NodeKind::While | NodeKind::For | NodeKind::ForTake)
                        )
                    {
                        self.out.diagnostics.push(
                            Refusal::new(
                                "LANG-15",
                                self.site(n),
                                "while and for do not accept a break value",
                            )
                            .with_comparison(
                                "while/for target with a break value",
                                "loop-expression target for a valued break",
                            ),
                        );
                    }
                    if let Some(t) = token {
                        let name = self.name(t);
                        let b = self.scratch.labels[self.label_base..]
                            .iter()
                            .rev()
                            .find(|&&(s, _, _)| s == name)
                            .map(|&(_, b, _)| b);
                        self.record(
                            n,
                            Namespace::Value,
                            LookupScope::Label(self.scope_id),
                            name,
                            false,
                            b.map_or(LookupResult::Missing, |b| {
                                LookupResult::Found(Target::Binder(b))
                            }),
                        );
                        if b.is_none() {
                            self.out.diagnostics.push(Refusal::new(
                                "LANG-15",
                                self.site(n),
                                "exit label not found in this function",
                            ));
                        }
                    }
                    for &child in c {
                        self.visit(child);
                    }
                }
                NodeKind::BoundName => {
                    if matches!(self.name(token.unwrap()), sym::INTEGER | sym::FLOAT)
                        && !(self.numeric_declaration && self.numeric_parameter)
                    {
                        self.out.diagnostics.push(Refusal::new(
                        "OWN-4",
                        self.site(n),
                        "numeric bounds are only permitted on type parameters of core declarations",
                    ).with_comparison(
                        format!("{} bound on {} in package {}", self.index.symbols.text(self.name(token.unwrap())),
                            if self.numeric_parameter { "a type parameter" } else { "a declaration" }, self.index.module_path(self.module).package.name),
                        "numeric bound on a core declaration's type parameter"));
                    }
                }
                NodeKind::Error => {}
                _ => {
                    if let [child] = c {
                        n = *child;
                        continue;
                    }
                    for &child in c {
                        self.visit(child)
                    }
                }
            }
            break;
        }
        self.depth = initial_depth;
    }
}

#[inline]
fn lookup_key(scope: LookupScope, symbol: Symbol, ns: Namespace, construct: bool) -> u128 {
    let (tag, id) = match scope {
        LookupScope::Local(id) => (0u128, id),
        LookupScope::Bind(id) => (1, id),
        LookupScope::Label(id) => (2, id),
        LookupScope::Module(id) => (3, id.0),
        LookupScope::Prelude(id) => (4, id.0),
        LookupScope::Modules(id) => (5, id.0),
        LookupScope::Children(id) => (6, id.0),
        LookupScope::Variants(id) => (7, id.0),
    };
    ((id as u128) << 42)
        | (tag << 39)
        | ((construct as u128) << 38)
        | (((ns == Namespace::Value) as u128) << 37)
        | symbol.0 as u128
}

#[derive(Clone, Copy, Default)]
struct ReadBucket {
    low: u64,
    high: u32,
    generation: u32,
}
#[derive(Default)]
struct ReadSet {
    buckets: Vec<ReadBucket>,
    generation: u32,
}
impl ReadSet {
    fn reset(&mut self, bound: usize) {
        let capacity = bound.saturating_mul(2).max(16).next_power_of_two();
        if capacity > self.buckets.len() {
            self.buckets.resize(capacity, ReadBucket::default());
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.buckets.fill(ReadBucket::default());
            self.generation = 1;
        }
    }
    #[inline]
    fn insert(&mut self, key: u128) -> bool {
        let mixed = (key as u64) ^ ((key >> 64) as u64).wrapping_mul(0x517cc1b727220a95);
        let hash = mixed.rotate_left(26).wrapping_mul(0x517cc1b727220a95);
        let mask = self.buckets.len() - 1;
        let mut slot = (hash ^ (hash >> 32)) as usize & mask;
        loop {
            let b = &mut self.buckets[slot];
            if b.generation != self.generation {
                *b = ReadBucket {
                    low: key as u64,
                    high: (key >> 64) as u32,
                    generation: self.generation,
                };
                return true;
            }
            if b.low == key as u64 && b.high == (key >> 64) as u32 {
                return false;
            }
            slot = (slot + 1) & mask;
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ReadWord {
    bits: u64,
    generation: u32,
}

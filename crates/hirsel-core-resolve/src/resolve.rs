use crate::{index::path_tokens, *};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, Symbol, TokenId, sym};
use rustc_hash::FxHashSet;
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
    pub resource_refusal: Option<Site>,
}
pub(crate) fn builtin(n: Symbol) -> bool {
    matches!(
        n,
        sym::INLINE
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
pub fn resolve(index: &NameIndex, module: ModuleId, ordinal: u32, item: Item<'_>) -> Resolution {
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
    let mut r = Resolver {
        index,
        module,
        item,
        ordinal,
        bindings: Vec::with_capacity(size.binders as usize),
        slots: vec![u32::MAX; index.symbols.len() * 2],
        labels: Vec::with_capacity(size.labels as usize),
        label_base: 0,
        loop_base: 0,
        loops: Vec::with_capacity(size.loops as usize),
        numeric_parameter: false,
        numeric_declaration: Some(index.module_package(module)) == index.core
            && matches!(
                item.tree.node(item.root).kind,
                NodeKind::Function
                    | NodeKind::CloneHook
                    | NodeKind::TypeDecl
                    | NodeKind::StableType
                    | NodeKind::ResourceType
                    | NodeKind::ScopedType
            ),
        scope_id: 0,
        next_scope: 1,
        depth: 0,
        paths: [None; 8],
        seen: FxHashSet::with_capacity_and_hasher(dedup_capacity, Default::default()),
        out: Resolution {
            names: Vec::with_capacity(size.names as usize),
            binders: Vec::with_capacity(size.binders as usize),
            reads: Vec::with_capacity(read_capacity),
            ..Resolution::default()
        },
    };
    if Some(index.module_package(module)) != index.core
        && item.tree.node(item.root).kind == NodeKind::Function
        && !item
            .tree
            .children(item.root)
            .iter()
            .any(|&c| item.tree.node(c).kind == NodeKind::Block)
    {
        let end = item.tree.node(item.root).origin.span.end;
        r.out.diagnostics.push(Refusal::new(
            "LANG-5",
            Site {
                module,
                item: ordinal,
                span: Span { start: end, end },
            },
            "a function outside core requires a body",
        ));
    }
    r.visit(item.root);
    r.out
}
struct Resolver<'a> {
    index: &'a NameIndex,
    module: ModuleId,
    item: Item<'a>,
    ordinal: u32,
    bindings: Vec<Binding>,
    slots: Vec<u32>,
    labels: Vec<(Symbol, u32, NodeKind)>,
    label_base: usize,
    loops: Vec<NodeKind>,
    loop_base: usize,
    numeric_parameter: bool,
    numeric_declaration: bool,
    scope_id: u32,
    next_scope: u32,
    depth: usize,
    paths: [Option<((u32, Symbol, bool, bool), Target)>; 8],
    seen: FxHashSet<(LookupScope, Symbol, Namespace, bool)>,
    out: Resolution,
}
impl<'a> Resolver<'a> {
    fn site(&self, n: NodeId) -> Site {
        Site {
            module: self.module,
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
    fn record(
        &mut self,
        n: NodeId,
        namespace: Namespace,
        scope: LookupScope,
        symbol: Symbol,
        constructor: bool,
        result: LookupResult,
    ) {
        if self.seen.insert((scope, symbol, namespace, constructor)) {
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
        let b = self.slots[Self::slot(name, ns)];
        (b != u32::MAX).then_some(b)
    }
    fn scope(&mut self, f: impl FnOnce(&mut Self)) {
        let mark = self.bindings.len();
        let old = self.scope_id;
        self.scope_id = self.next_scope;
        self.next_scope += 1;
        f(self);
        for b in self.bindings.drain(mark..) {
            self.slots[Self::slot(b.symbol, b.namespace)] = b.previous;
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
            self.out.diagnostics.push(Refusal::new(
                "LANG-7",
                self.site(n),
                "builtin type name cannot be redeclared",
            ));
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
        self.bindings.push(Binding {
            symbol: name,
            namespace: ns,
            previous: self.slots[slot],
        });
        self.slots[slot] = id;
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
        id: Option<DefinitionId>,
    ) -> Option<Target> {
        let Some(id) = id else {
            self.record(n, ns, scope, name, construct, LookupResult::Missing);
            return None;
        };
        let d = self.index.get(id);
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
            .member(module, name, ns, construct)
            .filter(|&id| {
                !construct
                    || self.index.get(id).kind != DefinitionKind::Type
                    || self.index.variant_names(id).is_empty()
            });
        self.selected(
            n,
            LookupScope::Module(module),
            name,
            ns,
            construct,
            candidate,
        )
    }
    fn prelude_lookup(
        &mut self,
        n: NodeId,
        name: Symbol,
        ns: Namespace,
        construct: bool,
    ) -> Option<Target> {
        let candidate = self.index.prelude(name, ns, construct).filter(|&id| {
            !construct
                || self.index.get(id).kind != DefinitionKind::Type
                || self.index.variant_names(id).is_empty()
        });
        let Some(core) = self.index.core else {
            return None;
        };
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
        let m = self.index.module_head(self.module, name);
        self.record(
            n,
            Namespace::Value,
            LookupScope::Modules(self.index.module_package(self.module)),
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
                        .reachable(self.module, self.index.module_package(d.site.module))
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
        let path = path_tokens(self.item, n)
            .map(|t| self.index.symbols.text(self.name(t)))
            .collect::<Vec<_>>()
            .join(".");
        self.out.diagnostics.push(Refusal::new(
            "LANG-7",
            self.site(n),
            format!("name `{path}` was not found"),
        ));
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
        let mut candidates = self
            .index
            .members_named(self.module, module, name, ns, false);
        let mut first = candidates.next();
        let mut second = candidates.next();
        if first.is_none() && construct {
            let mut cs = self
                .index
                .members_named(self.module, module, name, Namespace::Type, true);
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
            d.causal.extend(
                self.index
                    .members_named(self.module, module, name, ns, construct)
                    .map(|id| self.index.get(id).site),
            );
            self.out.diagnostics.push(d);
            Some(Target::Missing)
        } else {
            self.selected(n, LookupScope::Module(module), name, ns, construct, first)
        }
    }
    fn function_head(&mut self, n: NodeId, token: TokenId, id: DefinitionId) -> Target {
        let mut diagnostic = Refusal::new(
            "LANG-7",
            self.site(n),
            "a function cannot be the head of a dotted path",
        );
        diagnostic.primary.span = self.item.token_span(token);
        diagnostic.causal.push(self.index.get(id).site);
        self.out.diagnostics.push(diagnostic);
        Target::Missing
    }
    fn use_path(&mut self, n: NodeId, type_context: bool, construct: bool) {
        let node = self.item.tree.node(n);
        let simple = node.origin.tokens.end == node.origin.tokens.start + 1;
        let key = (
            self.scope_id,
            self.name(node.token.unwrap()),
            type_context,
            construct,
        );
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
        let diagnostics_before = self.out.diagnostics.len();
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
                    Some(Target::Module(mut module)) => {
                        let mut tail = rest.clone();
                        let mut selected_value = None;
                        while let Some(t) = tail.next() {
                            if t == last {
                                break;
                            }
                            let symbol = self.name(t);
                            let child = self.index.child(module, symbol);
                            self.record(
                                n,
                                Namespace::Value,
                                LookupScope::Children(module),
                                symbol,
                                false,
                                child.map_or(LookupResult::Missing, |m| {
                                    LookupResult::Found(Target::Module(m))
                                }),
                            );
                            if let Some(child) = child {
                                module = child;
                            } else {
                                let value = self
                                    .qualified_member(n, module, t, Namespace::Value, false)
                                    .unwrap_or_else(|| self.missing(n));
                                selected_value = Some(match value {
                                    Target::Definition(id)
                                        if self.index.get(id).kind == DefinitionKind::Function =>
                                    {
                                        self.function_head(n, t, id)
                                    }
                                    Target::Definition(id)
                                        if self.index.get(id).kind == DefinitionKind::Constant =>
                                    {
                                        projections = Span {
                                            start: tail.next().unwrap().0,
                                            end: self.item.tree.node(n).origin.tokens.end,
                                        };
                                        Target::Definition(id)
                                    }
                                    _ => value,
                                });
                                break;
                            }
                        }
                        selected_value.unwrap_or_else(|| {
                            self.qualified_member(n, module, last, ns, construct)
                                .unwrap_or_else(|| self.missing(n))
                        })
                    }
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
                        let mut diagnostic = Refusal::new(
                            "LANG-7",
                            self.site(n),
                            format!(
                                "`{}` is not a local, module, package or constant",
                                self.index.symbols.text(name)
                            ),
                        );
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
        if simple && self.out.diagnostics.len() == diagnostics_before && target != Target::Missing {
            self.paths[key.1.0 as usize & 7] = Some((key, target));
        }
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
            NodeKind::TakePattern => self.bind(n, token.unwrap(), BinderKind::Local),
            NodeKind::ConstructorPattern => {
                let c = self.children(n);
                if let Some((&path, fields)) = c.split_first() {
                    self.use_path(path, false, true);
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
        self.label_base = self.labels.len();
        self.loop_base = self.loops.len();
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
    fn visit(&mut self, n: NodeId) {
        if self.out.resource_refusal.is_some() {
            return;
        }
        match self.item.tree.node(n).kind {
            NodeKind::Literal => return,
            NodeKind::Path => return self.use_path(n, false, false),
            NodeKind::TypePath => return self.use_path(n, true, false),
            _ => {}
        }
        if self.depth >= 384 {
            self.out.resource_refusal = Some(self.site(n));
            return;
        }
        self.depth += 1;
        self.visit_inner(n);
        self.depth -= 1;
    }
    fn visit_inner(&mut self, n: NodeId) {
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
            | NodeKind::StableType
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
            NodeKind::Path => self.use_path(n, false, false),
            NodeKind::TypePath => self.use_path(n, true, false),
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
                    r.loops.push(kind);
                    r.visit(c[2]);
                    r.loops.pop();
                });
            }
            NodeKind::If | NodeKind::While => {
                self.scope(|r| {
                    r.condition(c[0]);
                    if kind == NodeKind::While {
                        r.loops.push(kind);
                    }
                    r.visit(c[1]);
                    if kind == NodeKind::While {
                        r.loops.pop();
                    }
                });
                for &child in &c[2..] {
                    self.visit(child);
                }
            }
            NodeKind::Loop => {
                self.loops.push(kind);
                for &child in c {
                    self.visit(child);
                }
                self.loops.pop();
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
                        self.use_path(callee, false, true);
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
                            self.out.diagnostics.push(Refusal::new(
                                "LANG-9",
                                self.site(n),
                                "ordinary calls have no labelled arguments",
                            ));
                        }
                    }
                }
            }
            NodeKind::Labelled => {
                let name = self.name(token.unwrap());
                let old = self.labels[self.label_base..]
                    .iter()
                    .rev()
                    .find(|&&(s, _, _)| s == name)
                    .map(|&(_, b, _)| b);
                if let Some(b) = old {
                    let mut d = Refusal::new("LANG-15", self.site(n), "active label is shadowed");
                    d.causal.push(self.out.binders[b as usize].site);
                    self.out.diagnostics.push(d);
                }
                let id = self.out.binders.len() as u32;
                self.out.binders.push(Binder {
                    id,
                    name,
                    kind: BinderKind::Label,
                    site: self.site(n),
                });
                self.labels.push((name, id, self.item.tree.node(c[0]).kind));
                for &child in c {
                    self.visit(child);
                }
                self.labels.pop();
            }
            NodeKind::Break | NodeKind::Continue => {
                let target = if let Some(t) = token {
                    let name = self.name(t);
                    self.labels[self.label_base..]
                        .iter()
                        .rev()
                        .find(|&&(s, _, _)| s == name)
                        .map(|&(_, _, k)| k)
                } else {
                    self.loops[self.loop_base..].last().copied()
                };
                if token.is_none() && target.is_none() {
                    self.out.diagnostics.push(Refusal::new(
                        "LANG-15",
                        self.site(n),
                        "exit has no enclosing loop",
                    ));
                } else if kind == NodeKind::Continue && target == Some(NodeKind::Block) {
                    self.out.diagnostics.push(Refusal::new(
                        "LANG-15",
                        self.site(n),
                        "continue target is a block",
                    ));
                } else if kind == NodeKind::Break
                    && !c.is_empty()
                    && matches!(
                        target,
                        Some(NodeKind::While | NodeKind::For | NodeKind::ForTake)
                    )
                {
                    self.out.diagnostics.push(Refusal::new(
                        "LANG-15",
                        self.site(n),
                        "while and for do not accept a break value",
                    ));
                }
                if let Some(t) = token {
                    let name = self.name(t);
                    let b = self.labels[self.label_base..]
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
                    ));
                }
            }
            NodeKind::Error => {}
            _ => {
                for &child in c {
                    self.visit(child)
                }
            }
        }
    }
}

use crate::{index::path_parts, *};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_syntax::{Item, NodeId, NodeKind, TokenId};
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BinderKind {
    Local,
    Parameter,
    TypeParameter,
    Region,
    Label,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Binder {
    pub id: u32,
    pub name: String,
    pub kind: BinderKind,
    pub site: Site,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Target {
    Definition(DefPath),
    Module(ModuleId),
    Binder(u32),
    Builtin(String),
    ContextualConstructor {
        name: String,
        candidates: Vec<DefPath>,
    },
    Missing,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LookupScope {
    Local,
    Module(ModuleId),
    Prelude,
    Modules(PackageId),
    Variants,
    Dependency(String),
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LookupResult {
    Found(Target),
    Missing,
    Private(DefPath),
    Opaque(DefPath),
    Shadowed(Target),
    Ambiguous(Vec<DefPath>),
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lookup {
    pub node: NodeId,
    pub site: Site,
    pub namespace: Namespace,
    pub scope: LookupScope,
    pub spelling: Vec<String>,
    pub result: LookupResult,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedUse {
    pub node: NodeId,
    pub target: Target,
    pub projections: Vec<String>,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Resolution {
    pub names: Vec<ResolvedUse>,
    pub binders: Vec<Binder>,
    pub reads: Vec<Lookup>,
    pub diagnostics: Vec<Refusal>,
    pub resource_refusal: Option<Site>,
}
pub(crate) fn builtin(n: &str) -> bool {
    matches!(
        n,
        "Inline"
            | "bool"
            | "char"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "f32"
            | "f64"
    )
}
pub fn resolve(index: &NameIndex, module: &ModuleId, ordinal: u32, item: &Item) -> Resolution {
    let mut r = Resolver {
        index,
        module,
        item,
        ordinal,
        scopes: vec![BTreeMap::new()],
        labels: vec![],
        label_kinds: vec![],
        loops: vec![],
        numeric_parameter: false,
        numeric_declaration: index.environment.core.as_ref() == Some(&module.package)
            && matches!(
                item.tree.node(item.root).kind,
                NodeKind::Function
                    | NodeKind::TypeDecl
                    | NodeKind::StableType
                    | NodeKind::ResourceType
                    | NodeKind::ScopedType
            )
            && item.tree.children(item.root).iter().all(|&c| {
                matches!(
                    item.tree.node(c).kind,
                    NodeKind::Public
                        | NodeKind::Opaque
                        | NodeKind::Attribute
                        | NodeKind::Generics
                        | NodeKind::Bounds
                        | NodeKind::Parameters
                        | NodeKind::ResultType
                        | NodeKind::Waits
                        | NodeKind::Outlives
                )
            }),
        depth: 0,
        out: Resolution::default(),
    };
    r.visit(item.root);
    r.out
}
struct Resolver<'a> {
    index: &'a NameIndex,
    module: &'a ModuleId,
    item: &'a Item,
    ordinal: u32,
    scopes: Vec<BTreeMap<(Namespace, String), u32>>,
    labels: Vec<BTreeMap<String, u32>>,
    label_kinds: Vec<NodeKind>,
    loops: Vec<NodeKind>,
    numeric_parameter: bool,
    numeric_declaration: bool,
    depth: usize,
    out: Resolution,
}
impl Resolver<'_> {
    fn site(&self, n: NodeId) -> Site {
        Site {
            module: self.module.clone(),
            item: self.ordinal,
            span: self.item.tree.node(n).origin.span,
        }
    }
    fn name(&self, t: TokenId) -> String {
        self.item.tree.token_text(&self.item.text, t).into()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        self.item.tree.children(n).to_vec()
    }
    fn record(
        &mut self,
        n: NodeId,
        namespace: Namespace,
        scope: LookupScope,
        spelling: Vec<String>,
        result: LookupResult,
    ) {
        self.out.reads.push(Lookup {
            node: n,
            site: self.site(n),
            namespace,
            scope,
            spelling,
            result,
        });
    }
    fn read(&mut self, n: NodeId, scope: LookupScope, spelling: Vec<String>, result: LookupResult) {
        self.record(n, Namespace::Value, scope, spelling, result);
    }
    fn local_in(&self, name: &str, ns: Namespace) -> Option<u32> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(&(ns, name.to_owned())).copied())
    }
    fn bind(&mut self, n: NodeId, t: TokenId, kind: BinderKind) {
        let name = self.name(t);
        let ns = if kind == BinderKind::TypeParameter {
            Namespace::Type
        } else {
            Namespace::Value
        };
        if ns == Namespace::Type && builtin(&name) {
            self.out.diagnostics.push(Refusal::new(
                "LANG-7",
                self.site(n),
                "builtin type name cannot be redeclared",
            ));
            return;
        }
        let local = self.local_in(&name, ns);
        self.record(
            n,
            ns,
            LookupScope::Local,
            vec![name.clone()],
            local.map_or(LookupResult::Missing, |b| {
                LookupResult::Found(Target::Binder(b))
            }),
        );
        let mut existing = local.map(Target::Binder);
        if existing.is_none() && ns == Namespace::Value {
            existing = self.module_lookup(n, self.module.clone(), &name, ns, false);
            if existing.is_none() {
                existing = self.prelude_lookup(n, &name, ns, false);
            }
            if existing.is_none() {
                existing = self.head_lookup(n, &name);
            }
        }
        if let Some(target) = existing {
            self.record(
                n,
                ns,
                LookupScope::Local,
                vec![name.clone()],
                LookupResult::Shadowed(target.clone()),
            );
            let mut d = Refusal::new(
                "LANG-7",
                self.site(n),
                format!("`{name}` shadows a visible name"),
            );
            match target {
                Target::Binder(b) => d.causal.push(self.out.binders[b as usize].site.clone()),
                Target::Definition(p) => {
                    if let Some(def) = self.index.get(&p) {
                        d.causal.push(def.site.clone());
                    }
                }
                _ => {}
            }
            self.out.diagnostics.push(d);
            return;
        }
        let id = self.out.binders.len() as u32;
        self.out.binders.push(Binder {
            id,
            name: name.clone(),
            kind,
            site: self.site(n),
        });
        self.scopes.last_mut().unwrap().insert((ns, name), id);
    }
    fn select(
        &mut self,
        n: NodeId,
        scope: LookupScope,
        name: &str,
        ns: Namespace,
        construct: bool,
        candidates: Vec<&Definition>,
    ) -> Option<Target> {
        if candidates.is_empty() {
            self.record(n, ns, scope, vec![name.into()], LookupResult::Missing);
            return None;
        }
        if candidates.len() > 1 {
            let paths = candidates.iter().map(|d| d.path.clone()).collect();
            self.record(
                n,
                ns,
                scope,
                vec![name.into()],
                LookupResult::Ambiguous(paths),
            );
            let mut d = Refusal::new("LANG-7", self.site(n), "ambiguous module extension member");
            d.causal.extend(candidates.iter().map(|d| d.site.clone()));
            self.out.diagnostics.push(d);
            return Some(Target::Missing);
        }
        let d = candidates[0];
        let path = d.path.clone();
        let result = if d.site.module != *self.module && !d.public {
            LookupResult::Private(path.clone())
        } else if construct && d.site.module != *self.module && d.opaque {
            LookupResult::Opaque(path.clone())
        } else {
            LookupResult::Found(Target::Definition(path.clone()))
        };
        if matches!(result, LookupResult::Private(_) | LookupResult::Opaque(_)) {
            let mut refusal = Refusal::new(
                "PROG-2",
                self.site(n),
                format!("`{path}` is private or opaque"),
            );
            refusal.causal.push(d.site.clone());
            self.out.diagnostics.push(refusal);
        }
        self.record(n, ns, scope, vec![name.into()], result);
        Some(Target::Definition(path))
    }
    fn module_lookup(
        &mut self,
        n: NodeId,
        module: ModuleId,
        name: &str,
        ns: Namespace,
        construct: bool,
    ) -> Option<Target> {
        let candidates = self
            .index
            .member(&module, name, ns, construct)
            .filter(|d| !construct || d.kind != DefinitionKind::Type || d.variants.is_empty())
            .into_iter()
            .collect();
        self.select(
            n,
            LookupScope::Module(module),
            name,
            ns,
            construct,
            candidates,
        )
    }
    fn prelude_lookup(
        &mut self,
        n: NodeId,
        name: &str,
        ns: Namespace,
        construct: bool,
    ) -> Option<Target> {
        let path = self.index.environment.prelude.get(name);
        let candidate = path.and_then(|p| self.index.get(p)).filter(|d| match ns {
            Namespace::Type => d.kind == DefinitionKind::Type,
            Namespace::Value => {
                d.kind == DefinitionKind::Function || construct && d.kind == DefinitionKind::Type
            }
        });
        let candidates = candidate
            .filter(|d| !construct || d.kind != DefinitionKind::Type || d.variants.is_empty())
            .into_iter()
            .collect();
        let found = self.select(n, LookupScope::Prelude, name, ns, construct, candidates);
        if found.is_none() && path.is_some() && path.and_then(|p| self.index.get(p)).is_none() {
            let p = path.unwrap();
            self.record(
                n,
                ns,
                LookupScope::Module(ModuleId {
                    package: p.0.package.clone(),
                    modules: p.0.modules.clone(),
                }),
                vec![p.0.name.clone()],
                LookupResult::Missing,
            );
        }
        found
    }
    fn head_lookup(&mut self, n: NodeId, name: &str) -> Option<Target> {
        let m = self.index.module_head(self.module, name);
        self.record(
            n,
            Namespace::Value,
            LookupScope::Modules(self.module.package.clone()),
            vec![name.into()],
            m.clone().map_or(LookupResult::Missing, |m| {
                LookupResult::Found(Target::Module(m))
            }),
        );
        m.map(Target::Module)
    }
    fn use_path(&mut self, n: NodeId, type_context: bool, construct: bool) {
        let parts = path_parts(self.item, n);
        if parts.is_empty() {
            return;
        }
        let ns = if type_context {
            Namespace::Type
        } else {
            Namespace::Value
        };
        if type_context && parts.len() == 1 && builtin(&parts[0]) {
            self.out.names.push(ResolvedUse {
                node: n,
                target: Target::Builtin(parts[0].clone()),
                projections: vec![],
            });
            return;
        }
        let first_ns = if parts.len() > 1 {
            Namespace::Value
        } else {
            ns
        };
        let local = self.local_in(&parts[0], first_ns);
        self.record(
            n,
            first_ns,
            LookupScope::Local,
            vec![parts[0].clone()],
            local.map_or(LookupResult::Missing, |b| {
                LookupResult::Found(Target::Binder(b))
            }),
        );
        let mut projections = vec![];
        let target = if let Some(b) = local {
            projections = parts[1..].to_vec();
            Target::Binder(b)
        } else if parts.len() == 1 {
            let name = &parts[0];
            if let Some(t) = self
                .module_lookup(n, self.module.clone(), name, ns, false)
                .or_else(|| self.prelude_lookup(n, name, ns, false))
            {
                t
            } else if type_context && builtin(name) {
                Target::Builtin(name.clone())
            } else if construct && !self.visible_variants(name).is_empty() {
                let mut candidates = self.visible_variants(name);
                let record = self
                    .index
                    .member(self.module, name, Namespace::Type, true)
                    .or_else(|| {
                        self.index
                            .environment
                            .prelude
                            .get(name)
                            .and_then(|p| self.index.get(p))
                    })
                    .filter(|d| d.kind == DefinitionKind::Type && d.variants.is_empty());
                if let Some(record) = record {
                    candidates.push(record.path.clone());
                }
                let target = Target::ContextualConstructor {
                    name: name.clone(),
                    candidates,
                };
                self.read(
                    n,
                    LookupScope::Variants,
                    vec![name.clone()],
                    LookupResult::Found(target.clone()),
                );
                target
            } else if construct {
                if let Some(t) = self
                    .module_lookup(n, self.module.clone(), name, Namespace::Type, true)
                    .or_else(|| self.prelude_lookup(n, name, Namespace::Type, true))
                {
                    t
                } else {
                    self.variant_or_missing(n, name)
                }
            } else if !type_context {
                if let Some(t) = self.head_lookup(n, name) {
                    t
                } else {
                    self.variant_or_missing(n, name)
                }
            } else {
                self.missing(n, &parts)
            }
        } else {
            let first = self
                .module_lookup(n, self.module.clone(), &parts[0], Namespace::Value, false)
                .or_else(|| self.prelude_lookup(n, &parts[0], Namespace::Value, false))
                .or_else(|| self.head_lookup(n, &parts[0]));
            match first {
                Some(Target::Module(mut module)) => {
                    module.modules.extend_from_slice(&parts[1..parts.len() - 1]);
                    let name = parts.last().unwrap();
                    let mut cs = self
                        .index
                        .members_named(self.module, &module, name, ns, false);
                    if cs.is_empty() && construct {
                        cs = self.index.members_named(
                            self.module,
                            &module,
                            name,
                            Namespace::Type,
                            true,
                        );
                    }
                    self.select(n, LookupScope::Module(module), name, ns, construct, cs)
                        .unwrap_or_else(|| self.missing(n, &parts))
                }
                Some(t) => {
                    projections = parts[1..].to_vec();
                    t
                }
                None => self.missing(n, &parts),
            }
        };
        self.out.names.push(ResolvedUse {
            node: n,
            target,
            projections,
        });
    }
    fn missing(&mut self, n: NodeId, parts: &[String]) -> Target {
        self.out.diagnostics.push(Refusal::new(
            "LANG-7",
            self.site(n),
            format!("name `{}` was not found", parts.join(".")),
        ));
        Target::Missing
    }
    fn visible_variants(&self, name: &str) -> Vec<DefPath> {
        self.index
            .variants(name)
            .filter(|d| {
                d.site.module == *self.module
                    || d.public && !d.opaque && self.index.reachable(self.module, &d.path.0.package)
            })
            .map(|d| d.path.clone())
            .collect()
    }
    fn variant_or_missing(&mut self, n: NodeId, name: &str) -> Target {
        let candidates = self.visible_variants(name);
        if candidates.is_empty() {
            self.read(
                n,
                LookupScope::Variants,
                vec![name.into()],
                LookupResult::Missing,
            );
            self.missing(n, &[name.into()])
        } else {
            let t = Target::ContextualConstructor {
                name: name.into(),
                candidates,
            };
            self.read(
                n,
                LookupScope::Variants,
                vec![name.into()],
                LookupResult::Found(t.clone()),
            );
            t
        }
    }
    fn pattern(&mut self, n: NodeId) {
        let node = self.item.tree.node(n);
        let kind = node.kind;
        let token = node.token;
        match kind {
            NodeKind::BindPattern => {
                let t = token.unwrap();
                let name = self.name(t);
                if !self.visible_variants(&name).is_empty()
                    || matches!(name.as_str(), "None" | "Ok" | "Some" | "Error")
                {
                    // The checker decides bare constructor versus binding from expected type.
                    self.use_bare_pattern(n, &name);
                } else {
                    self.read(n, LookupScope::Variants, vec![name], LookupResult::Missing);
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
                for c in self.children(n) {
                    self.pattern(c);
                }
            }
            _ => {}
        }
    }
    fn use_bare_pattern(&mut self, n: NodeId, name: &str) {
        let target = Target::ContextualConstructor {
            name: name.into(),
            candidates: self.visible_variants(name),
        };
        self.read(
            n,
            LookupScope::Variants,
            vec![name.into()],
            LookupResult::Found(target.clone()),
        );
        self.out.names.push(ResolvedUse {
            node: n,
            target,
            projections: vec![],
        });
    }
    fn scope(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(BTreeMap::new());
        f(self);
        self.scopes.pop();
    }
    fn function(&mut self, n: NodeId) {
        let old_labels = std::mem::take(&mut self.labels);
        let old_kinds = std::mem::take(&mut self.label_kinds);
        let old_loops = std::mem::take(&mut self.loops);
        self.scope(|r| {
            let children = r.children(n);
            for &c in &children {
                if r.item.tree.node(c).kind == NodeKind::Generics {
                    r.visit(c);
                }
            }
            for &c in &children {
                if r.item.tree.node(c).kind == NodeKind::Parameters {
                    for p in r.children(c) {
                        if let Some(t) = r.item.tree.node(p).token {
                            r.bind(p, t, BinderKind::Parameter);
                        }
                    }
                }
            }
            for c in children {
                if r.item.tree.node(c).kind != NodeKind::Generics {
                    r.visit(c);
                }
            }
        });
        self.labels = old_labels;
        self.label_kinds = old_kinds;
        self.loops = old_loops;
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
            NodeKind::Function => self.function(n),
            NodeKind::Closure | NodeKind::ScopedClosure => self.function(n),
            NodeKind::CallableType | NodeKind::ScopedCallableType | NodeKind::OwnCallableType => {
                self.function(n)
            }
            NodeKind::TypeDecl
            | NodeKind::StableType
            | NodeKind::ResourceType
            | NodeKind::ScopedType => self.scope(|r| {
                for child in c {
                    r.visit(child);
                }
            }),
            NodeKind::Block => self.scope(|r| {
                for &child in &c {
                    r.visit(child);
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
                for child in c {
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
                for child in c {
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
            NodeKind::Region => self.scope(|r| {
                r.bind(n, token.unwrap(), BinderKind::Region);
                for &child in &c {
                    r.visit(child);
                }
            }),
            NodeKind::Drop => self.scope(|r| {
                r.bind(n, token.unwrap(), BinderKind::Parameter);
                for &child in &c {
                    r.visit(child);
                }
            }),
            NodeKind::Call => {
                if let Some((&callee, args)) = c.split_first() {
                    let labelled = args
                        .iter()
                        .any(|&a| self.item.tree.node(a).kind == NodeKind::LabelledArgument);
                    if self.item.tree.node(callee).kind == NodeKind::Path {
                        self.use_path(callee, false, true);
                    } else {
                        self.visit(callee);
                    }
                    for &arg in args {
                        self.visit(arg);
                    }
                    if labelled {
                        let target = self
                            .out
                            .names
                            .iter()
                            .find(|u| u.node == callee)
                            .map(|u| &u.target);
                        let ordinary = matches!(target,Some(Target::Definition(path)) if self.index.get(path).is_some_and(|d|d.kind==DefinitionKind::Function))
                            || matches!(target, Some(Target::Binder(_)));
                        if ordinary {
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
                let old = self.labels.iter().rev().find_map(|s| s.get(&name).copied());
                if let Some(b) = old {
                    let mut d = Refusal::new("LANG-15", self.site(n), "active label is shadowed");
                    d.causal.push(self.out.binders[b as usize].site.clone());
                    self.out.diagnostics.push(d);
                }
                let id = self.out.binders.len() as u32;
                self.out.binders.push(Binder {
                    id,
                    name: name.clone(),
                    kind: BinderKind::Label,
                    site: self.site(n),
                });
                self.labels.push(BTreeMap::from([(name, id)]));
                self.label_kinds.push(self.item.tree.node(c[0]).kind);
                for child in c {
                    self.visit(child);
                }
                self.labels.pop();
                self.label_kinds.pop();
            }
            NodeKind::Break | NodeKind::Continue => {
                let target = if let Some(t) = token {
                    let name = self.name(t);
                    self.labels
                        .iter()
                        .enumerate()
                        .rev()
                        .find_map(|(i, s)| s.contains_key(&name).then_some(self.label_kinds[i]))
                } else {
                    self.loops.last().copied()
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
                    let b = self.labels.iter().rev().find_map(|s| s.get(&name).copied());
                    self.read(
                        n,
                        LookupScope::Local,
                        vec![name],
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
                for child in c {
                    self.visit(child);
                }
            }
            NodeKind::BoundName => {
                if matches!(self.name(token.unwrap()).as_str(), "integer" | "float")
                    && !(self.numeric_declaration && self.numeric_parameter)
                {
                    self.out.diagnostics.push(Refusal::new("OWN-4",self.site(n),"numeric bounds are only permitted on type parameters of bodiless core declarations"));
                }
            }
            NodeKind::Error => {}
            _ => {
                for child in c {
                    self.visit(child);
                }
            }
        }
    }
}

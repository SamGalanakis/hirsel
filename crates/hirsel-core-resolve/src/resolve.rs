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
    Brand,
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
    /// Expected-type selection belongs to checking. Never choose an owner here.
    ContextualVariant {
        name: String,
        candidates: Vec<DefPath>,
    },
    Field {
        owner: DefPath,
        name: String,
    },
    Missing,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LookupScope {
    Local,
    Module(ModuleId),
    Prelude,
    Fields(DefPath),
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
}
/// Every attempted lookup, successful or negative. A driver can key reads without rescanning syntax.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lookup {
    pub node: NodeId,
    pub site: Site,
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

fn builtin(name: &str) -> bool {
    matches!(
        name,
        "bool"
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
            | "Never"
            | "Option"
            | "Result"
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
        owner: None,
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
    scopes: Vec<BTreeMap<String, u32>>,
    labels: Vec<BTreeMap<String, u32>>,
    owner: Option<DefPath>,
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
        self.item.tree.token_text(&self.item.text, t).to_owned()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        self.item.tree.children(n).to_vec()
    }
    fn read(&mut self, n: NodeId, scope: LookupScope, spelling: Vec<String>, result: LookupResult) {
        self.out.reads.push(Lookup {
            node: n,
            site: self.site(n),
            scope,
            spelling,
            result,
        });
    }
    fn local(&self, name: &str) -> Option<u32> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }
    fn bind(&mut self, n: NodeId, t: TokenId, kind: BinderKind) {
        let name = self.name(t);
        let path = self.module.definition(&name);
        let mut existing = self.local(&name).map(Target::Binder).or_else(|| {
            self.index
                .get(&path)
                .map(|_| Target::Definition(path.clone()))
        });
        self.read(
            n,
            LookupScope::Local,
            vec![name.clone()],
            self.local(&name).map_or(LookupResult::Missing, |b| {
                LookupResult::Found(Target::Binder(b))
            }),
        );
        self.read(
            n,
            LookupScope::Module(self.module.clone()),
            vec![name.clone()],
            self.index.get(&path).map_or(LookupResult::Missing, |_| {
                LookupResult::Found(Target::Definition(path.clone()))
            }),
        );
        let head = self.index.has_module_head(&self.module.package, &name);
        let module_target = Target::Module(ModuleId {
            package: self.module.package.clone(),
            modules: vec![name.clone()],
        });
        self.read(
            n,
            LookupScope::Modules(self.module.package.clone()),
            vec![name.clone()],
            if head {
                LookupResult::Found(module_target.clone())
            } else {
                LookupResult::Missing
            },
        );
        if existing.is_none() && head {
            existing = Some(module_target);
        }
        let dependency = self
            .index
            .environment
            .dependencies
            .get(&self.module.package)
            .and_then(|e| e.get(&name))
            .cloned();
        self.read(
            n,
            LookupScope::Dependency(name.clone()),
            vec![name.clone()],
            dependency.clone().map_or(LookupResult::Missing, |m| {
                LookupResult::Found(Target::Module(m))
            }),
        );
        if existing.is_none() {
            existing = dependency.map(Target::Module);
        }
        let core = self
            .index
            .environment
            .prelude
            .get(&name)
            .cloned()
            .filter(|p| self.index.get(p).is_some());
        self.read(
            n,
            LookupScope::Prelude,
            vec![name.clone()],
            core.clone().map_or(LookupResult::Missing, |p| {
                LookupResult::Found(Target::Definition(p))
            }),
        );
        if existing.is_none() {
            existing = core
                .map(Target::Definition)
                .or_else(|| builtin(&name).then(|| Target::Builtin(name.clone())));
        }
        if let Some(target) = existing {
            self.read(
                n,
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
                Target::Definition(path) => {
                    d.causal.push(self.index.get(&path).unwrap().site.clone())
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
        self.scopes.last_mut().unwrap().insert(name, id);
    }
    fn use_path(&mut self, n: NodeId, type_context: bool, construct: bool) {
        let parts = path_parts(self.item, n);
        if parts.is_empty() {
            return;
        }
        let mut projections = vec![];
        let target = if let Some(b) = self.local(&parts[0]).filter(|b| {
            !type_context || self.out.binders[*b as usize].kind == BinderKind::TypeParameter
        }) {
            self.read(
                n,
                LookupScope::Local,
                vec![parts[0].clone()],
                LookupResult::Found(Target::Binder(b)),
            );
            projections.extend(parts[1..].iter().cloned());
            Target::Binder(b)
        } else {
            self.read(
                n,
                LookupScope::Local,
                vec![parts[0].clone()],
                LookupResult::Missing,
            );
            self.global(n, &parts, type_context, construct)
        };
        if projections.is_empty()
            && parts.len() > 1
            && matches!(&target,Target::Definition(p) if p==&self.module.definition(&parts[0]))
        {
            projections = parts[1..].to_vec();
        }
        self.out.names.push(ResolvedUse {
            node: n,
            target,
            projections,
        });
    }
    fn lookup_definition(
        &mut self,
        n: NodeId,
        module: ModuleId,
        name: &str,
        construct: bool,
    ) -> Option<Target> {
        let path = module.definition(name);
        let scope = LookupScope::Module(module.clone());
        let Some(def) = self.index.get(&path) else {
            self.read(n, scope, vec![name.into()], LookupResult::Missing);
            return None;
        };
        let result = if module != *self.module && !def.public {
            LookupResult::Private(path.clone())
        } else if module != *self.module && construct && def.opaque {
            LookupResult::Opaque(path.clone())
        } else {
            LookupResult::Found(Target::Definition(path.clone()))
        };
        if matches!(result, LookupResult::Private(_) | LookupResult::Opaque(_)) {
            self.out.diagnostics.push(Refusal::new(
                "PROG-2",
                self.site(n),
                format!("`{path}` is private or opaque"),
            ));
        }
        self.read(n, scope, vec![name.into()], result);
        Some(Target::Definition(path))
    }
    fn global(
        &mut self,
        n: NodeId,
        parts: &[String],
        type_context: bool,
        construct: bool,
    ) -> Target {
        let name = parts.last().unwrap();
        if parts.len() > 1 {
            if let Some(target) = self.lookup_definition(n, self.module.clone(), &parts[0], false) {
                return target;
            }
            let local_module = self.index.has_module_head(&self.module.package, &parts[0]);
            self.read(
                n,
                LookupScope::Modules(self.module.package.clone()),
                vec![parts[0].clone()],
                if local_module {
                    LookupResult::Found(Target::Module(ModuleId {
                        package: self.module.package.clone(),
                        modules: vec![parts[0].clone()],
                    }))
                } else {
                    LookupResult::Missing
                },
            );
            let mut module = ModuleId {
                package: self.module.package.clone(),
                modules: parts[..parts.len() - 1].to_vec(),
            };
            if let Some(exposures) = self
                .index
                .environment
                .dependencies
                .get(&self.module.package)
            {
                if let Some(exposure) = exposures.get(&parts[0]).filter(|_| !local_module) {
                    module = exposure.clone();
                    module.modules.extend_from_slice(&parts[1..parts.len() - 1]);
                    self.read(
                        n,
                        LookupScope::Dependency(parts[0].clone()),
                        vec![parts[0].clone()],
                        LookupResult::Found(Target::Definition(module.definition(name))),
                    );
                } else {
                    self.read(
                        n,
                        LookupScope::Dependency(parts[0].clone()),
                        vec![parts[0].clone()],
                        LookupResult::Missing,
                    );
                }
            }
            if let Some(t) = self.lookup_definition(n, module, name, construct) {
                return t;
            }
        } else {
            if let Some(t) = self.lookup_definition(n, self.module.clone(), name, construct) {
                return t;
            }
            let module_head = self.index.has_module_head(&self.module.package, name);
            let module = ModuleId {
                package: self.module.package.clone(),
                modules: vec![name.clone()],
            };
            self.read(
                n,
                LookupScope::Modules(self.module.package.clone()),
                parts.to_vec(),
                if module_head {
                    LookupResult::Found(Target::Module(module.clone()))
                } else {
                    LookupResult::Missing
                },
            );
            if module_head {
                return Target::Module(module);
            }
            let dependency = self
                .index
                .environment
                .dependencies
                .get(&self.module.package)
                .and_then(|e| e.get(name))
                .cloned();
            self.read(
                n,
                LookupScope::Dependency(name.clone()),
                parts.to_vec(),
                dependency.clone().map_or(LookupResult::Missing, |m| {
                    LookupResult::Found(Target::Module(m))
                }),
            );
            if let Some(module) = dependency {
                return Target::Module(module);
            }
            if let Some(path) = self.index.environment.prelude.get(name).cloned() {
                self.read(
                    n,
                    LookupScope::Prelude,
                    parts.to_vec(),
                    LookupResult::Found(Target::Definition(path.clone())),
                );
                let module = ModuleId {
                    package: path.0.package.clone(),
                    modules: path.0.modules.clone(),
                };
                if let Some(target) = self.lookup_definition(n, module, &path.0.name, construct) {
                    return target;
                }
            }
            self.read(
                n,
                LookupScope::Prelude,
                parts.to_vec(),
                LookupResult::Missing,
            );
            if type_context
                && matches!(
                    name.as_str(),
                    "bool"
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
                        | "Never"
                        | "Option"
                        | "Result"
                )
            {
                return Target::Builtin(name.clone());
            }
            if !type_context {
                let candidates = self.visible_variants(name);
                if !candidates.is_empty()
                    || matches!(name.as_str(), "Some" | "None" | "Ok" | "Error")
                {
                    let target = Target::ContextualVariant {
                        name: name.clone(),
                        candidates,
                    };
                    self.read(
                        n,
                        LookupScope::Variants,
                        parts.to_vec(),
                        LookupResult::Found(target.clone()),
                    );
                    return target;
                }
                self.read(
                    n,
                    LookupScope::Variants,
                    parts.to_vec(),
                    LookupResult::Missing,
                );
            }
        }
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
            .iter()
            .filter(|path| {
                self.index.get(path).is_some_and(|d| {
                    let here = d.site.module == *self.module;
                    here || (d.public
                        && !d.opaque
                        && (path.0.package == self.module.package
                            || self
                                .index
                                .environment
                                .dependencies
                                .get(&self.module.package)
                                .is_some_and(|e| e.values().any(|m| m.package == path.0.package))))
                })
            })
            .cloned()
            .collect::<Vec<_>>()
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
        let target = Target::ContextualVariant {
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
            | NodeKind::ScopedType => {
                let owner = self.module.definition(&self.name(token.unwrap()));
                let old = self.owner.replace(owner);
                self.scope(|r| {
                    for &child in &c {
                        if r.item.tree.node(child).kind == NodeKind::Brand {
                            let path = r.children(child)[0];
                            let path_node = r.item.tree.node(path);
                            if path_node.kind == NodeKind::Path {
                                r.bind(path, path_node.token.unwrap(), BinderKind::Brand);
                                continue;
                            }
                        }
                        r.visit(child);
                    }
                });
                self.owner = old;
            }
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
                for child in c {
                    self.visit(child);
                }
            }
            NodeKind::FreshBrand => {
                let name = self.name(token.unwrap());
                if let Some(owner) = self.owner.clone() {
                    let target = Target::Field {
                        owner: owner.clone(),
                        name: name.clone(),
                    };
                    if self
                        .index
                        .get(&owner)
                        .is_some_and(|d| d.fields.contains(&name))
                    {
                        self.read(
                            n,
                            LookupScope::Fields(owner),
                            vec![name],
                            LookupResult::Found(target.clone()),
                        );
                        self.out.names.push(ResolvedUse {
                            node: n,
                            target,
                            projections: vec![],
                        });
                    } else {
                        self.read(
                            n,
                            LookupScope::Fields(owner),
                            vec![name],
                            LookupResult::Missing,
                        );
                        self.out.diagnostics.push(Refusal::new(
                            "LANG-7",
                            self.site(n),
                            "sibling brand field was not found",
                        ));
                    }
                } else {
                    self.out.diagnostics.push(Refusal::new(
                        "LANG-7",
                        self.site(n),
                        "sibling brand outside a record",
                    ));
                }
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
                    r.visit(c[2]);
                });
            }
            NodeKind::If | NodeKind::While => {
                self.scope(|r| {
                    r.condition(c[0]);
                    r.visit(c[1]);
                });
                for &child in &c[2..] {
                    self.visit(child);
                }
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
                for child in c {
                    self.visit(child);
                }
                self.labels.pop();
            }
            NodeKind::Break | NodeKind::Continue => {
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
            NodeKind::Error | NodeKind::BoundName => {}
            _ => {
                for child in c {
                    self.visit(child);
                }
            }
        }
    }
}

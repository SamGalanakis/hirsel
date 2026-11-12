use hirsel_core_ids::{DefPath, ItemPath, PackageId};
use hirsel_core_syntax::{Diagnostic, Item, NodeId, NodeKind, Span, TokenKind};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize)]
pub struct ModuleId {
    pub package: PackageId,
    pub modules: Vec<String>,
}
impl ModuleId {
    pub fn definition(&self, name: &str) -> DefPath {
        DefPath(ItemPath {
            package: self.package.clone(),
            modules: self.modules.clone(),
            name: name.into(),
        })
    }
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Site {
    pub module: ModuleId,
    pub item: u32,
    pub span: Span,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Refusal {
    pub rule: String,
    pub primary: Site,
    pub causal: Vec<Site>,
    pub declared: Option<String>,
    pub required: Option<String>,
    pub message: String,
}
impl Refusal {
    pub(crate) fn new(rule: &str, primary: Site, message: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            primary,
            causal: vec![],
            declared: None,
            required: None,
            message: message.into(),
        }
    }
    pub fn syntax(module: ModuleId, diagnostic: &Diagnostic) -> Self {
        Self {
            rule: diagnostic.rule.clone(),
            primary: Site {
                module,
                item: u32::MAX,
                span: diagnostic.primary,
            },
            causal: vec![],
            declared: diagnostic.declared.clone(),
            required: diagnostic.required.clone(),
            message: diagnostic.message.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DefinitionKind {
    Function,
    Constant,
    Type,
}
/// Index entries contain only declared headers. No body, AST or signature provider.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Definition {
    pub path: DefPath,
    pub kind: DefinitionKind,
    pub public: bool,
    pub opaque: bool,
    pub site: Site,
    pub fields: Vec<String>,
    pub variants: Vec<String>,
}
/// Namespace is chosen by syntax (LANG-7), never inferred from a type.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub enum Namespace {
    Type,
    Value,
}
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub prelude: BTreeMap<String, DefPath>,
    pub dependencies: BTreeMap<PackageId, BTreeMap<String, ModuleId>>,
    pub core: Option<PackageId>,
}
/// Header rows and module membership are built once, before any definition is resolved.
#[derive(Clone, Debug, Default)]
pub struct NameIndex {
    definitions: Vec<Definition>,
    paths: BTreeMap<DefPath, usize>,
    members: BTreeMap<ModuleId, BTreeMap<String, usize>>,
    heads: BTreeMap<String, BTreeSet<ModuleId>>,
    variants: BTreeMap<String, Vec<usize>>,
    pub environment: Environment,
    pub diagnostics: Vec<Refusal>,
}
impl NameIndex {
    pub fn new(environment: Environment) -> Self {
        Self {
            environment,
            ..Self::default()
        }
    }
    pub fn add_module(&mut self, module: &ModuleId) {
        if !self.members.contains_key(module)
            && module
                .modules
                .iter()
                .any(|n| hirsel_core_syntax::Keyword::from_spelling(n).is_some())
        {
            self.diagnostics.push(Refusal::new(
                "LANG-2",
                Site {
                    module: module.clone(),
                    item: u32::MAX,
                    span: Span::default(),
                },
                "module name is a reserved keyword",
            ));
        }
        self.members.entry(module.clone()).or_default();
        if let Some(head) = module.modules.first() {
            self.heads
                .entry(head.clone())
                .or_default()
                .insert(module.clone());
        }
    }
    pub fn has_module_head(&self, package: &PackageId, name: &str) -> bool {
        self.heads
            .get(name)
            .is_some_and(|ms| ms.iter().any(|m| &m.package == package))
    }
    pub fn definitions(&self) -> impl Iterator<Item = &Definition> {
        self.definitions.iter()
    }
    pub fn get(&self, path: &DefPath) -> Option<&Definition> {
        self.paths.get(path).map(|&i| &self.definitions[i])
    }
    pub fn member(
        &self,
        module: &ModuleId,
        name: &str,
        namespace: Namespace,
        constructor: bool,
    ) -> Option<&Definition> {
        let &i = self.members.get(module)?.get(name)?;
        let d = &self.definitions[i];
        (match namespace {
            Namespace::Type => d.kind == DefinitionKind::Type,
            Namespace::Value => d.kind != DefinitionKind::Type || constructor,
        })
        .then_some(d)
    }
    pub fn variants(&self, name: &str) -> impl Iterator<Item = &Definition> {
        self.variants
            .get(name)
            .into_iter()
            .flatten()
            .map(|&i| &self.definitions[i])
    }
    pub fn reachable(&self, from: &ModuleId, package: &PackageId) -> bool {
        package == &from.package
            || self.environment.core.as_ref() == Some(package)
            || self
                .environment
                .dependencies
                .get(&from.package)
                .is_some_and(|ds| ds.values().any(|m| &m.package == package))
    }
    pub fn core_module(&self, name: &str) -> Option<ModuleId> {
        let package = self.environment.core.as_ref()?;
        self.heads
            .get(name)?
            .iter()
            .find(|m| &m.package == package && m.modules.len() == 1)
            .cloned()
    }
    pub fn module_head(&self, from: &ModuleId, name: &str) -> Option<ModuleId> {
        if self.has_module_head(&from.package, name) {
            return Some(ModuleId {
                package: from.package.clone(),
                modules: vec![name.into()],
            });
        }
        self.environment
            .dependencies
            .get(&from.package)
            .and_then(|d| d.get(name))
            .cloned()
            .or_else(|| self.core_module(name))
    }
    /// Merge core module extensions by name while retaining every member's identity.
    pub fn members_named(
        &self,
        from: &ModuleId,
        module: &ModuleId,
        name: &str,
        ns: Namespace,
        constructor: bool,
    ) -> Vec<&Definition> {
        if module.modules.len() == 1 && self.core_module(&module.modules[0]).is_some() {
            self.heads
                .get(&module.modules[0])
                .into_iter()
                .flatten()
                .filter(|m| m.modules.len() == 1 && self.reachable(from, &m.package))
                .filter_map(|m| self.member(m, name, ns, constructor))
                .filter(|d| {
                    d.site.module == *from
                        || d.public
                        || self.environment.core.as_ref() == Some(&d.path.0.package)
                })
                .collect()
        } else {
            self.member(module, name, ns, constructor)
                .into_iter()
                .collect()
        }
    }
    pub fn configure_core(&mut self, package: PackageId) {
        self.environment.core = Some(package.clone());
        let prelude = ModuleId {
            package,
            modules: vec!["core".into()],
        };
        for d in &self.definitions {
            if d.site.module == prelude && d.public {
                self.environment
                    .prelude
                    .insert(d.path.0.name.clone(), d.path.clone());
            }
        }
        for d in &self.definitions {
            if d.site.module.modules.len() != 1
                || Some(&d.path.0.package) == self.environment.core.as_ref()
            {
                continue;
            }
            let Some(core) = self.core_module(&d.site.module.modules[0]) else {
                continue;
            };
            if let Some(&i) = self.members.get(&core).and_then(|m| m.get(&d.path.0.name)) {
                let mut r = Refusal::new(
                    "LANG-7",
                    d.site.clone(),
                    "extension redeclares a core member",
                );
                r.causal.push(self.definitions[i].site.clone());
                self.diagnostics.push(r);
            }
        }
    }
    pub fn add(&mut self, definition: Definition) {
        self.add_module(&definition.site.module);
        if definition.kind == DefinitionKind::Type
            && crate::resolve::builtin(&definition.path.0.name)
        {
            self.diagnostics.push(Refusal::new(
                "LANG-7",
                definition.site.clone(),
                "builtin type name cannot be redeclared",
            ));
            return;
        }
        if let Some(old) = self.get(&definition.path) {
            let mut d = Refusal::new("LANG-7", definition.site.clone(), "duplicate definition");
            d.causal.push(old.site.clone());
            self.diagnostics.push(d);
            return;
        }
        let i = self.definitions.len();
        self.members
            .get_mut(&definition.site.module)
            .unwrap()
            .insert(definition.path.0.name.clone(), i);
        for name in &definition.variants {
            self.variants.entry(name.clone()).or_default().push(i);
        }
        self.paths.insert(definition.path.clone(), i);
        self.definitions.push(definition);
    }
    pub fn index_item(&mut self, module: &ModuleId, ordinal: u32, item: &Item) {
        let root = item.tree.node(item.root);
        let kind = match root.kind {
            NodeKind::Function => DefinitionKind::Function,
            NodeKind::Constant => DefinitionKind::Constant,
            NodeKind::TypeDecl
            | NodeKind::StableType
            | NodeKind::ResourceType
            | NodeKind::ScopedType => DefinitionKind::Type,
            _ => return,
        };
        let Some(name) = root.token else { return };
        let name_span = item.tree.tokens[name.0 as usize].span;
        let name = item.tree.token_text(&item.text, name);
        let mut d = Definition {
            path: module.definition(name),
            kind,
            public: false,
            opaque: false,
            site: Site {
                module: module.clone(),
                item: ordinal,
                span: name_span,
            },
            fields: vec![],
            variants: vec![],
        };
        for &child in item.tree.children(item.root) {
            let n = item.tree.node(child);
            match n.kind {
                NodeKind::Public => d.public = true,
                NodeKind::Opaque => d.opaque = true,
                NodeKind::Fields | NodeKind::Variants => {
                    for &field in item.tree.children(child) {
                        let n = item.tree.node(field);
                        if let Some(t) = n.token {
                            let name = item.tree.token_text(&item.text, t).into();
                            if n.kind == NodeKind::Field {
                                d.fields.push(name);
                            } else if n.kind == NodeKind::Variant {
                                d.variants.push(name);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.add(d);
    }
}
pub(crate) fn path_parts(item: &Item, node: NodeId) -> Vec<String> {
    let r = &item.tree.node(node).origin.tokens;
    item.tree.tokens[r.start as usize..r.end as usize]
        .iter()
        .filter(|t| t.kind == TokenKind::Ident)
        .map(|t| item.text[t.span.start as usize..t.span.end as usize].to_owned())
        .collect()
}

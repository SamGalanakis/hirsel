use hirsel_core_ids::published::{Map, Publisher, Table};
use hirsel_core_ids::{DefPath, ItemPath, PackageId};
use hirsel_core_syntax::{Diagnostic, Item, NodeId, NodeKind, Span, Symbol, Symbols};
use std::sync::Arc;
macro_rules! ids { ($($name:ident),*) => {$(
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize)]
    pub struct $name(pub u32);
)*}; }
ids!(ModuleId, PackageKey, DefinitionId);
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rows {
    pub start: u32,
    pub end: u32,
}
impl Rows {
    pub fn slice<T: Clone>(self, rows: &Table<T>) -> &[T] {
        rows.slice(self.start as usize..self.end as usize)
    }
}
/// External discovery input. Strings stop at the compilation's symbol table.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModulePath {
    pub package: PackageId,
    pub modules: Vec<String>,
}
/// One source-file occurrence in this index, including files of the same module.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SourceId(pub u32);
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Site {
    pub module: ModuleId,
    /// The file the site is in: `item` is that file's item ordinal and `span`
    /// is relative to the item, or to the file when `item` is `u32::MAX`. None
    /// only for a site of a module itself (its name), which is in no file.
    pub source: Option<SourceId>,
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
    /// The declared/written fact and the fact required by the cited rule.
    pub fn with_comparison(
        mut self,
        declared: impl Into<String>,
        required: impl Into<String>,
    ) -> Self {
        self.declared = Some(declared.into());
        self.required = Some(required.into());
        self
    }
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
    pub fn syntax(module: ModuleId, source: SourceId, diagnostic: &Diagnostic) -> Self {
        Self {
            rule: diagnostic.rule.clone(),
            primary: Site {
                module,
                source: Some(source),
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
    /// A core type's clone body (MEM-17): named by no path, so it is found by its
    /// item ([`NameIndex::item_definition`]); its name is the type's, and its path
    /// `<module>/<Type>.$clone`.
    Clone,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Definition {
    pub name: Symbol,
    pub kind: DefinitionKind,
    pub public: bool,
    pub opaque: bool,
    pub site: Site,
    pub fields: Rows,
    pub variants: Rows,
}
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Namespace {
    Type,
    Value,
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Module {
    pub package: PackageKey,
    pub parent: Option<ModuleId>,
    pub name: Symbol,
    pub depth: u32,
    merge: Rows,
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Package {
    pub name: Symbol,
    pub version: Symbol,
    pub root: ModuleId,
    dependencies: Rows,
}
/// One compilation's flat header index. No body or owning path is stored here.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NameIndex {
    pub symbols: Arc<Symbols>,
    pub modules: Table<Module>,
    pub packages: Table<Package>,
    pub definitions: Table<Definition>,
    members: Map<(ModuleId, Symbol), DefinitionId>,
    /// Clone bodies by source-file occurrence and item ordinal; no name finds one.
    clone_items: Map<(SourceId, u32), DefinitionId>,
    sources: u32,
    children: Map<(ModuleId, Symbol), ModuleId>,
    package_ids: Map<(Symbol, Symbol), PackageKey>,
    dependencies: Table<PackageKey>,
    member_names: Table<Symbol>,
    variants: Vec<(Symbol, DefinitionId)>,
    published_variants: Map<Symbol, Vec<DefinitionId>>,
    extensions: Table<ModuleId>,
    prelude: Map<Symbol, DefinitionId>,
    pub core: Option<PackageKey>,
    pub diagnostics: Vec<Refusal>,
}
impl NameIndex {
    pub fn publish(&self, writer: &mut impl Publisher) -> std::io::Result<Self> {
        let mut result = self.clone();
        result.symbols = Arc::new(self.symbols.publish(writer)?);
        result.modules = writer.table(&self.modules)?;
        result.packages = writer.table(&self.packages)?;
        result.definitions = writer.table(&self.definitions)?;
        result.members = writer.map(&self.members)?;
        result.clone_items = writer.map(&self.clone_items)?;
        result.children = writer.map(&self.children)?;
        result.package_ids = writer.map(&self.package_ids)?;
        result.dependencies = writer.table(&self.dependencies)?;
        result.member_names = writer.table(&self.member_names)?;
        result.extensions = writer.table(&self.extensions)?;
        result.prelude = writer.map(&self.prelude)?;
        let mut variants: std::collections::BTreeMap<Symbol, Vec<DefinitionId>> = self
            .published_variants
            .iter()
            .map(|(&s, v)| (s, v.clone()))
            .collect();
        for &(s, d) in &self.variants {
            variants.entry(s).or_default().push(d);
        }
        result.published_variants = writer.map(&variants.into_iter().collect())?;
        result.variants.clear();
        Ok(result)
    }
    pub fn new(symbols: Arc<Symbols>) -> Self {
        Self {
            symbols,
            modules: Table::default(),
            packages: Table::default(),
            definitions: Table::default(),
            members: Map::default(),
            clone_items: Map::default(),
            sources: 0,
            children: Map::default(),
            package_ids: Map::default(),
            dependencies: Table::default(),
            member_names: Table::default(),
            variants: vec![],
            published_variants: Map::default(),
            extensions: Table::default(),
            prelude: Map::default(),
            core: None,
            diagnostics: vec![],
        }
    }
    pub fn add_package(&mut self, name: Symbol, version: Symbol) -> PackageKey {
        if let Some(&p) = self.package_ids.get(&(name, version)) {
            return p;
        }
        let id = PackageKey(self.packages.len() as u32);
        let root = ModuleId(self.modules.len() as u32);
        self.modules.push(Module {
            package: id,
            parent: None,
            name,
            depth: 0,
            merge: Rows::default(),
        });
        self.packages.push(Package {
            name,
            version,
            root,
            dependencies: Rows::default(),
        });
        self.package_ids.insert((name, version), id);
        id
    }
    pub fn package(&self, name: Symbol, version: Symbol) -> Option<PackageKey> {
        self.package_ids.get(&(name, version)).copied()
    }
    pub fn set_dependencies(&mut self, p: PackageKey, ds: impl IntoIterator<Item = PackageKey>) {
        let start = self.dependencies.len() as u32;
        self.dependencies.extend(ds);
        self.packages[p.0 as usize].dependencies = Rows {
            start,
            end: self.dependencies.len() as u32,
        };
    }
    pub fn add_module(&mut self, package: PackageKey, path: &[Symbol]) -> ModuleId {
        let mut parent = self.packages[package.0 as usize].root;
        for &name in path {
            if let Some(&id) = self.children.get(&(parent, name)) {
                parent = id;
                continue;
            }
            let id = ModuleId(self.modules.len() as u32);
            self.modules.push(Module {
                package,
                parent: Some(parent),
                name,
                depth: self.modules[parent.0 as usize].depth + 1,
                merge: Rows::default(),
            });
            self.children.insert((parent, name), id);
            // An overlay can add a module after the root's members are indexed.
            if !self.definitions.is_empty()
                && let Some(package) = self.path_package(parent)
                && let Some(root) = self.root_module(package)
                && let Some(member) = self.definition(root, name)
            {
                self.refuse_root_member(self.get(member).site, name, id);
            }
            if hirsel_core_syntax::keyword_symbol(name) {
                self.diagnostics.push(
                    Refusal::new(
                        "LANG-2",
                        Site {
                            module: id,
                            source: None,
                            item: u32::MAX,
                            span: Span::default(),
                        },
                        "module name is a reserved keyword",
                    )
                    .with_comparison(self.symbols.text(name), "non-keyword module name"),
                );
            }
            parent = id;
        }
        parent
    }
    pub fn child(&self, parent: ModuleId, name: Symbol) -> Option<ModuleId> {
        self.children.get(&(parent, name)).copied()
    }
    /// The naming scope at a package head, whether found as a dependency or
    /// through its root module. Discovery still uses the physical module paths.
    pub(crate) fn path_package(&self, module: ModuleId) -> Option<Package> {
        let row = &self.modules[module.0 as usize];
        if row.depth > 1 {
            return None;
        }
        let package = self.packages[row.package.0 as usize];
        (module == package.root || row.name == package.name).then_some(package)
    }
    pub(crate) fn root_module(&self, package: Package) -> Option<ModuleId> {
        self.child(package.root, package.name)
    }
    fn refuse_root_member(&mut self, site: Site, name: Symbol, conflict: ModuleId) {
        let mut refusal = Refusal::new(
            "LANG-7",
            site,
            "root module member has the name of a module of its package",
        )
        .with_comparison(
            self.symbols.text(name),
            "a name distinct from the package's modules",
        );
        refusal.causal.push(Site {
            module: conflict,
            source: None,
            item: u32::MAX,
            span: Span::default(),
        });
        self.diagnostics.push(refusal);
    }
    pub fn module_package(&self, module: ModuleId) -> PackageKey {
        self.modules[module.0 as usize].package
    }
    pub fn reachable(&self, from: ModuleId, package: PackageKey) -> bool {
        self.reachable_from(self.module_package(from), package)
    }
    pub(crate) fn reachable_from(&self, own: PackageKey, package: PackageKey) -> bool {
        package == own
            || Some(package) == self.core
            || self.packages[own.0 as usize]
                .dependencies
                .slice(&self.dependencies)
                .contains(&package)
    }
    pub fn core_module(&self, name: Symbol) -> Option<ModuleId> {
        self.child(self.packages[self.core?.0 as usize].root, name)
    }
    pub fn module_head(&self, from: ModuleId, name: Symbol) -> Option<ModuleId> {
        self.module_head_from(self.module_package(from), name)
    }
    pub(crate) fn module_head_from(&self, package: PackageKey, name: Symbol) -> Option<ModuleId> {
        let p = &self.packages[package.0 as usize];
        self.child(p.root, name)
            .or_else(|| {
                p.dependencies
                    .slice(&self.dependencies)
                    .iter()
                    .find_map(|d| {
                        let p = &self.packages[d.0 as usize];
                        (p.name == name).then_some(p.root)
                    })
            })
            .or_else(|| self.core_module(name))
    }
    pub fn get(&self, id: DefinitionId) -> &Definition {
        &self.definitions[id.0 as usize]
    }
    pub fn definitions(&self) -> impl Iterator<Item = (DefinitionId, &Definition)> {
        self.definitions
            .iter()
            .enumerate()
            .map(|(i, d)| (DefinitionId(i as u32), d))
    }
    pub fn member(
        &self,
        module: ModuleId,
        name: Symbol,
        ns: Namespace,
        constructor: bool,
    ) -> Option<DefinitionId> {
        self.member_row(module, name, ns, constructor)
            .map(|(id, _)| id)
    }
    pub(crate) fn member_row(
        &self,
        module: ModuleId,
        name: Symbol,
        ns: Namespace,
        constructor: bool,
    ) -> Option<(DefinitionId, &Definition)> {
        let id = *self.members.get(&(module, name))?;
        let d = self.get(id);
        (match ns {
            Namespace::Type => d.kind == DefinitionKind::Type,
            Namespace::Value => d.kind != DefinitionKind::Type || constructor,
        })
        .then_some((id, d))
    }
    pub fn prelude(&self, name: Symbol, ns: Namespace, constructor: bool) -> Option<DefinitionId> {
        self.prelude_row(name, ns, constructor).map(|(id, _)| id)
    }
    pub(crate) fn prelude_row(
        &self,
        name: Symbol,
        ns: Namespace,
        constructor: bool,
    ) -> Option<(DefinitionId, &Definition)> {
        let id = *self.prelude.get(&name)?;
        let d = self.get(id);
        (match ns {
            Namespace::Type => d.kind == DefinitionKind::Type,
            Namespace::Value => {
                d.kind == DefinitionKind::Function || constructor && d.kind == DefinitionKind::Type
            }
        })
        .then_some((id, d))
    }
    pub fn field_names(&self, d: DefinitionId) -> &[Symbol] {
        self.get(d).fields.slice(&self.member_names)
    }
    pub fn variant_names(&self, d: DefinitionId) -> &[Symbol] {
        self.get(d).variants.slice(&self.member_names)
    }
    pub fn variants(&self, name: Symbol) -> impl Iterator<Item = DefinitionId> + '_ {
        let start = self.variants.partition_point(|&(s, _)| s < name);
        self.published_variants
            .get(&name)
            .into_iter()
            .flatten()
            .copied()
            .chain(
                self.variants[start..]
                    .iter()
                    .take_while(move |&&(s, _)| s == name)
                    .map(|&(_, d)| d),
            )
    }
    pub fn constructor_candidates(
        &self,
        from: ModuleId,
        name: Symbol,
    ) -> impl Iterator<Item = DefinitionId> + '_ {
        let own = self.module_package(from);
        let variants = self.variants(name).filter(move |&id| {
            let d = self.get(id);
            d.site.module == from
                || d.public
                    && !d.opaque
                    && self.reachable_from(own, self.module_package(d.site.module))
        });
        let record = self
            .member_row(from, name, Namespace::Type, true)
            .or_else(|| self.prelude_row(name, Namespace::Type, true))
            .filter(|&(_, d)| d.variants.start == d.variants.end)
            .map(|(id, _)| id);
        variants.chain(record)
    }
    pub fn members_named(
        &self,
        from: ModuleId,
        module: ModuleId,
        name: Symbol,
        ns: Namespace,
        constructor: bool,
    ) -> impl Iterator<Item = DefinitionId> + '_ {
        self.member_rows_from(
            from,
            self.module_package(from),
            module,
            name,
            ns,
            constructor,
        )
        .map(|(id, _)| id)
    }
    pub(crate) fn member_rows_from(
        &self,
        from: ModuleId,
        own: PackageKey,
        module: ModuleId,
        name: Symbol,
        ns: Namespace,
        constructor: bool,
    ) -> impl Iterator<Item = (DefinitionId, &Definition)> + '_ {
        let module_row = &self.modules[module.0 as usize];
        let merge = module_row.merge;
        let direct = if merge.start == merge.end && self.reachable_from(own, module_row.package) {
            self.member_row(module, name, ns, constructor)
        } else {
            None
        };
        direct
            .into_iter()
            .chain(merge.slice(&self.extensions).iter().filter_map(move |&m| {
                let package = self.modules[m.0 as usize].package;
                if !self.reachable_from(own, package) {
                    return None;
                }
                let (id, d) = self.member_row(m, name, ns, constructor)?;
                (d.site.module == from || d.public || Some(package) == self.core).then_some((id, d))
            }))
    }
    pub fn configure_core(&mut self, package: PackageKey) {
        self.core = Some(package);
        let name = self.symbols.find("core").unwrap();
        if let Some(module) = self.child(self.packages[package.0 as usize].root, name) {
            for (i, d) in self
                .definitions
                .own_iter()
                .enumerate()
                .map(|(i, d)| (self.definitions.base_len() + i, d))
            {
                if d.site.module == module && d.public {
                    self.prelude.insert(d.name, DefinitionId(i as u32));
                }
            }
        }
        self.variants.sort_unstable_by_key(|&(s, d)| (s, d));
        let mut heads: Vec<_> = self
            .modules
            .own_iter()
            .enumerate()
            .map(|(i, m)| (self.modules.base_len() + i, m))
            .filter(|(_, m)| m.depth == 1)
            .map(|(i, m)| (m.name, ModuleId(i as u32)))
            .collect();
        heads.sort_unstable();
        let core_heads: Vec<_> = heads
            .iter()
            .filter_map(|&(name, _)| self.core_module(name).map(|m| (name, m)))
            .collect();
        for (name, core) in core_heads {
            let merge = self.modules[core.0 as usize].merge;
            if merge.start == merge.end {
                heads.push((name, core));
            } else {
                heads.extend(
                    merge
                        .slice(&self.extensions)
                        .iter()
                        .copied()
                        .map(|m| (name, m)),
                );
            }
        }
        heads.sort_unstable();
        heads.dedup();
        for group in heads.chunk_by(|a, b| a.0 == b.0) {
            if self.core_module(group[0].0).is_none() {
                continue;
            }
            let start = self.extensions.len() as u32;
            self.extensions.extend(group.iter().map(|&(_, m)| m));
            let range = Rows {
                start,
                end: self.extensions.len() as u32,
            };
            for &(_, m) in group {
                self.modules[m.0 as usize].merge = range;
            }
        }
        for definitions in self
            .definitions
            .own_slice()
            .chunk_by(|a, b| a.site.module == b.site.module)
        {
            let m = &self.modules[definitions[0].site.module.0 as usize];
            if m.depth != 1 || m.package == package {
                continue;
            }
            let Some(core) = self.core_module(m.name) else {
                continue;
            };
            for d in definitions {
                if let Some(&old) = self.members.get(&(core, d.name)) {
                    let mut r =
                        Refusal::new("LANG-7", d.site, "extension redeclares a core member");
                    r.causal.push(self.get(old).site);
                    r = r.with_comparison(
                        self.symbols.text(d.name),
                        "an extension member name absent from the core module",
                    );
                    self.diagnostics.push(r);
                }
            }
        }
    }
    /// Allocate after retained image sources, so new files cannot reuse their identities.
    pub fn add_source(&mut self) -> SourceId {
        let source = SourceId(self.sources);
        self.sources += 1;
        source
    }
    pub fn index_item(&mut self, module: ModuleId, source: SourceId, ordinal: u32, item: Item<'_>) {
        let root = item.tree.node(item.root);
        let kind = match root.kind {
            NodeKind::Function => DefinitionKind::Function,
            NodeKind::Constant => DefinitionKind::Constant,
            NodeKind::TypeDecl | NodeKind::ResourceType | NodeKind::ScopedType => {
                DefinitionKind::Type
            }
            NodeKind::CloneHook => DefinitionKind::Clone,
            _ => return,
        };
        let Some(t) = root.token else { return };
        let name = if kind == DefinitionKind::Clone {
            // The type the body copies: the last name of its parameter's type path. A
            // body without one still gets an identity, so the checker refuses it.
            clone_type_name(item).unwrap_or_else(|| item.symbol(t))
        } else {
            item.symbol(t)
        };
        let site = Site {
            module,
            source: Some(source),
            item: ordinal,
            span: item.token_span(t),
        };
        if kind != DefinitionKind::Clone
            && let Some(package) = self.path_package(module)
            && module != package.root
            && let Some(conflict) = self
                .child(package.root, name)
                .or_else(|| self.child(module, name))
        {
            self.refuse_root_member(site, name, conflict);
            return;
        }
        if kind == DefinitionKind::Type && crate::resolve::builtin(name) {
            self.diagnostics.push(
                Refusal::new("LANG-7", site, "builtin type name cannot be redeclared")
                    .with_comparison(self.symbols.text(name), "a fresh type-definition name"),
            );
            return;
        }
        let old = if kind == DefinitionKind::Clone {
            self.clone_items
                .iter()
                .map(|(_, &d)| d)
                .find(|&d| self.get(d).site.module == module && self.get(d).name == name)
        } else if module.0 as usize >= self.modules.base_len() {
            // New modules cannot have members in the published prefix.
            match self.members.entry((module, name)) {
                std::collections::hash_map::Entry::Occupied(e) => Some(*e.get()),
                std::collections::hash_map::Entry::Vacant(_) => None,
            }
        } else {
            self.members.get(&(module, name)).copied()
        };
        if let Some(old) = old {
            let mut r = Refusal::new("LANG-7", site, "duplicate definition");
            r.causal.push(self.get(old).site);
            r = r.with_comparison(
                self.symbols.text(name),
                "one definition of this name in the module namespace",
            );
            self.diagnostics.push(r);
            return;
        }
        let mut d = Definition {
            name,
            kind,
            public: false,
            opaque: false,
            site,
            fields: Rows::default(),
            variants: Rows::default(),
        };
        let id = DefinitionId(self.definitions.len() as u32);
        for &child in item.tree.children(item.root) {
            let n = item.tree.node(child);
            match n.kind {
                NodeKind::Public => d.public = true,
                NodeKind::Opaque => d.opaque = true,
                NodeKind::Fields | NodeKind::Variants => {
                    let start = self.member_names.len() as u32;
                    for &child in item.tree.children(child) {
                        if matches!(
                            item.tree.node(child).kind,
                            NodeKind::RemovedField | NodeKind::RemovedVariant
                        ) {
                            continue;
                        }
                        if let Some(t) = item.tree.node(child).token {
                            let s = item.symbol(t);
                            self.member_names.push(s);
                            if n.kind == NodeKind::Variants {
                                self.variants.push((s, id));
                            }
                        }
                    }
                    let range = Rows {
                        start,
                        end: self.member_names.len() as u32,
                    };
                    if n.kind == NodeKind::Fields {
                        d.fields = range;
                    } else {
                        d.variants = range;
                    }
                }
                _ => {}
            }
        }
        if kind == DefinitionKind::Clone {
            self.clone_items.insert((source, ordinal), id);
        } else {
            self.members.insert((module, name), id);
        }
        self.definitions.push(d);
    }
    /// The definition declared by this source-file occurrence and local ordinal:
    /// by its name, or for a clone body by its occurrence.
    pub fn item_definition(
        &self,
        module: ModuleId,
        source: SourceId,
        ordinal: u32,
        item: Item<'_>,
    ) -> Option<DefinitionId> {
        let root = item.tree.node(item.root);
        if root.kind == NodeKind::CloneHook {
            return self.clone_items.get(&(source, ordinal)).copied();
        }
        self.definition(module, item.symbol(root.token?))
    }
    /// Materialize neutral paths only at driver/checker/output boundaries.
    pub fn module_path(&self, module: ModuleId) -> ModulePath {
        let mut parts = vec![];
        let mut m = module;
        while let Some(parent) = self.modules[m.0 as usize].parent {
            parts.push(
                self.symbols
                    .text(self.modules[m.0 as usize].name)
                    .to_owned(),
            );
            m = parent;
        }
        parts.reverse();
        let p = self.packages[self.module_package(module).0 as usize];
        ModulePath {
            package: PackageId {
                name: self.symbols.text(p.name).into(),
                version: self.symbols.text(p.version).into(),
            },
            modules: parts,
        }
    }
    pub fn definition_path(&self, id: DefinitionId) -> DefPath {
        let d = self.get(id);
        let m = self.module_path(d.site.module);
        let name = self.symbols.text(d.name);
        DefPath(ItemPath {
            package: m.package,
            modules: m.modules,
            name: if d.kind == DefinitionKind::Clone {
                format!("{name}.$clone")
            } else {
                name.into()
            },
        })
    }
    pub fn definition(&self, module: ModuleId, name: Symbol) -> Option<DefinitionId> {
        self.members.get(&(module, name)).copied()
    }
}
/// The last name of a clone body's parameter type (`Map` in `x: core.Map[K, V]`).
fn clone_type_name(item: Item<'_>) -> Option<Symbol> {
    let params = item
        .tree
        .children(item.root)
        .iter()
        .copied()
        .find(|&c| item.tree.node(c).kind == NodeKind::Parameters)?;
    let param = *item.tree.children(params).first()?;
    let ty = *item.tree.children(param).first()?;
    let path = if item.tree.node(ty).kind == NodeKind::TypePath {
        ty
    } else {
        *item.tree.children(ty).first()?
    };
    path_tokens(item, path).last().map(|t| item.symbol(t))
}
pub(crate) fn path_tokens(
    item: Item<'_>,
    n: NodeId,
) -> impl Iterator<Item = hirsel_core_syntax::TokenId> + Clone + '_ {
    let r = &item.tree.node(n).origin.tokens;
    (r.start..r.end)
        .filter(move |&i| item.tree.tokens[i as usize].kind == hirsel_core_syntax::TokenKind::Ident)
        .map(hirsel_core_syntax::TokenId)
}

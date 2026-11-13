//! Definition keys keep implementation edits separate from signature dependencies.
use super::*;
use hirsel_core_resolve::{LookupResult, LookupScope, Target};
use hirsel_core_syntax::{Item, NodeKind};
use std::collections::{BTreeSet, HashSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DefinitionKey {
    pub body: ContentHash,
    pub signatures: BTreeMap<DefPath, ContentHash>,
    pub lookups: ContentHash,
    pub environment: ContentHash,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Header {
    digest: ContentHash,
    reads: Vec<DefPath>,
    anchor: (ModuleId, u32),
}
pub(crate) struct Keys {
    pub(crate) headers: BTreeMap<DefPath, Header>,
    environment: ContentHash,
    paths: Vec<DefPath>,
    modules: Vec<ModulePath>,
    packages: Vec<PackageId>,
}
impl Keys {
    pub fn new(
        index: &NameIndex,
        modules: &[ModuleInput],
        resolutions: &[Vec<Arc<Resolution>>],
        inputs: &CorpusManifest,
        compiler: ContentHash,
        image_headers: Option<&BTreeMap<DefPath, Header>>,
        entry: Option<&DefPath>,
    ) -> io::Result<Self> {
        let paths: Vec<_> = index
            .definitions()
            .map(|(id, _)| index.definition_path(id))
            .collect();
        let mut headers = image_headers.cloned().unwrap_or_default();
        let mut namespaces: Vec<_> = inputs
            .packages
            .iter()
            .map(|p| (&p.package, &p.dependencies))
            .collect();
        namespaces.sort_by_key(|p| p.0);
        let environment = ContentHash::of_parts(
            "hirsel.core-environment.v1",
            [&encode(&(compiler, &inputs.core, namespaces, entry))?[..]],
        );
        for (m, rows) in modules.iter().zip(resolutions) {
            for (ordinal, (item, resolution)) in m.syntax.items().zip(rows).enumerate() {
                let Some(t) = item.tree.node(item.root).token else {
                    continue;
                };
                let path =
                    paths[index.definition(m.id, item.symbol(t)).unwrap().0 as usize].clone();
                let end = header_end(item);
                let digest = ContentHash::of_bytes(&item.source_text().as_bytes()[..end]);
                let mut reads = BTreeSet::new();
                for name in &resolution.names {
                    if (item.tree.node(name.node).origin.span.start as usize) < end {
                        targets(index, &paths, m.id, name.target, &mut reads);
                    }
                }
                headers.insert(
                    path,
                    Header {
                        digest,
                        reads: reads.into_iter().collect(),
                        anchor: (m.id, ordinal as u32),
                    },
                );
            }
        }
        Ok(Self {
            headers,
            environment,
            paths,
            modules: (0..index.modules.len())
                .map(|i| index.module_path(ModuleId(i as u32)))
                .collect(),
            packages: index
                .packages
                .iter()
                .map(|p| PackageId {
                    name: index.symbols.text(p.name).into(),
                    version: index.symbols.text(p.version).into(),
                })
                .collect(),
        })
    }
    pub fn capture_anchors(
        &self,
        check: &CoreCheck,
        incomplete: &[Incomplete],
    ) -> BTreeMap<(ModuleId, u32), DefPath> {
        let mut anchors = BTreeMap::new();
        let mut capture = |site: &Site| {
            let at = (site.module, site.item);
            if let Some((path, _)) = self.headers.iter().find(|(_, h)| h.anchor == at) {
                anchors.insert(at, path.clone());
            }
        };
        for r in &check.refusals {
            capture(&r.primary);
            for site in &r.causal {
                capture(site);
            }
        }
        for g in incomplete {
            capture(&g.primary);
        }
        anchors
    }
    pub fn bind_site(&self, anchors: &BTreeMap<(ModuleId, u32), DefPath>, site: &mut Site) {
        if let Some(path) = anchors.get(&(site.module, site.item))
            && let Some(h) = self.headers.get(path)
        {
            site.module = h.anchor.0;
            site.item = h.anchor.1;
        }
    }
    pub fn for_body(
        &self,
        index: &NameIndex,
        path: &DefPath,
        item: Item<'_>,
        resolution: &Resolution,
    ) -> io::Result<DefinitionKey> {
        let module = self.headers[path].anchor.0;
        let mut pending = BTreeSet::from([path.clone()]);
        for name in &resolution.names {
            targets(index, &self.paths, module, name.target, &mut pending);
        }
        // Lookup results retain negative, ambiguous and shadowed dependencies, even if
        // no corresponding call was emitted. Header closure supplies transitive aliases.
        for read in &resolution.reads {
            match &read.result {
                LookupResult::Found(t) | LookupResult::Shadowed(t) => {
                    targets(index, &self.paths, module, *t, &mut pending)
                }
                LookupResult::Private(p) | LookupResult::Opaque(p) => {
                    pending.insert(self.paths[p.0 as usize].clone());
                }
                LookupResult::Ambiguous => {}
                LookupResult::Missing => {}
            }
        }
        let mut signatures = BTreeMap::new();
        let mut seen = HashSet::new();
        while let Some(p) = pending.pop_first() {
            if !seen.insert(p.clone()) {
                continue;
            }
            let h = self.headers.get(&p).ok_or_else(|| {
                io::Error::other(format!(
                    "core signature demand outside reached definitions: {p}"
                ))
            })?;
            signatures.insert(p, h.digest);
            pending.extend(h.reads.iter().cloned());
        }
        let lookups: Vec<_> = resolution
            .reads
            .iter()
            .map(|r| {
                let (kind, local, module_scope, package_scope) = match r.scope {
                    LookupScope::Local(n) => (0, n, None, None),
                    LookupScope::Bind(n) => (1, n, None, None),
                    LookupScope::Label(n) => (2, n, None, None),
                    LookupScope::Module(m) => (3, 0, Some(&self.modules[m.0 as usize]), None),
                    LookupScope::Prelude(p) => (4, 0, None, Some(&self.packages[p.0 as usize])),
                    LookupScope::Modules(p) => (5, 0, None, Some(&self.packages[p.0 as usize])),
                    LookupScope::Children(m) => (6, 0, Some(&self.modules[m.0 as usize]), None),
                    LookupScope::Variants(m) => (7, 0, Some(&self.modules[m.0 as usize]), None),
                };
                let result = match r.result {
                    LookupResult::Found(t) => (0, Some(self.target_key(index, module, t))),
                    LookupResult::Missing => (1, None),
                    LookupResult::Private(d) => (
                        2,
                        Some(self.target_key(index, module, Target::Definition(d))),
                    ),
                    LookupResult::Opaque(d) => (
                        3,
                        Some(self.target_key(index, module, Target::Definition(d))),
                    ),
                    LookupResult::Shadowed(t) => (4, Some(self.target_key(index, module, t))),
                    LookupResult::Ambiguous => (5, None),
                };
                (
                    r.namespace,
                    kind,
                    local,
                    module_scope,
                    package_scope,
                    index.symbols.text(r.symbol),
                    r.constructor,
                    result,
                )
            })
            .collect();
        Ok(DefinitionKey {
            body: ContentHash::of_bytes(item.source_text().as_bytes()),
            signatures,
            lookups: ContentHash::of_bytes(&encode(&lookups)?),
            environment: self.environment,
        })
    }
}
#[derive(Serialize)]
enum TargetKey<'a> {
    Definition(&'a DefPath),
    Module(&'a ModulePath),
    Binder(u32),
    Builtin(&'a str),
    Constructor(&'a str, Vec<&'a DefPath>),
    Missing,
}
impl Keys {
    fn target_key<'a>(
        &'a self,
        index: &'a NameIndex,
        module: ModuleId,
        target: Target,
    ) -> TargetKey<'a> {
        match target {
            Target::Definition(d) => TargetKey::Definition(&self.paths[d.0 as usize]),
            Target::Module(m) => TargetKey::Module(&self.modules[m.0 as usize]),
            Target::Binder(b) => TargetKey::Binder(b),
            Target::Builtin(s) => TargetKey::Builtin(index.symbols.text(s)),
            Target::ContextualConstructor { name } => TargetKey::Constructor(
                index.symbols.text(name),
                index
                    .constructor_candidates(module, name)
                    .map(|d| &self.paths[d.0 as usize])
                    .collect(),
            ),
            Target::Missing => TargetKey::Missing,
        }
    }
}
fn targets(
    index: &NameIndex,
    paths: &[DefPath],
    module: ModuleId,
    target: Target,
    out: &mut BTreeSet<DefPath>,
) {
    match target {
        Target::Definition(d) => {
            out.insert(paths[d.0 as usize].clone());
        }
        Target::ContextualConstructor { name } => out.extend(
            index
                .constructor_candidates(module, name)
                .map(|d| paths[d.0 as usize].clone()),
        ),
        _ => {}
    }
}
fn header_end(item: Item<'_>) -> usize {
    if item.tree.node(item.root).kind != NodeKind::Function {
        return item.source_text().len();
    }
    item.tree
        .children(item.root)
        .iter()
        .find_map(|&c| {
            let n = item.tree.node(c);
            (n.kind == NodeKind::Block).then_some(n.origin.span.start as usize)
        })
        .unwrap_or(item.source_text().len())
}

pub(crate) fn compiler_identity() -> io::Result<ContentHash> {
    static ID: std::sync::LazyLock<io::Result<ContentHash>> = std::sync::LazyLock::new(|| {
        // Hash the running executable once per process, including its format and code.
        // Linux's proc link remains bound to the loaded inode across binary replacement.
        let path = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            std::env::current_exe()?
        };
        std::fs::read(path).map(|b| ContentHash::of_bytes(&b))
    });
    ID.as_ref()
        .copied()
        .map_err(|e| io::Error::other(e.to_string()))
}

//! Definition keys keep implementation edits separate from signature dependencies.
use super::*;
use hirsel_core_resolve::{LookupResult, Target};
use hirsel_core_syntax::{Item, NodeKind};
use std::collections::{BTreeSet, HashSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DefinitionKey {
    pub body: ContentHash,
    pub signatures: BTreeMap<DefPath, ContentHash>,
    pub lookups: ContentHash,
    pub environment: ContentHash,
}
struct Header {
    digest: ContentHash,
    reads: Vec<DefPath>,
    anchor: (ModuleId, u32),
}
pub(crate) struct Keys {
    headers: BTreeMap<DefPath, Header>,
    environment: ContentHash,
}
impl Keys {
    pub fn new(
        modules: &[ModuleInput],
        resolutions: &[Vec<Arc<Resolution>>],
        inputs: &CorpusManifest,
        compiler: ContentHash,
    ) -> io::Result<Self> {
        let mut headers = BTreeMap::new();
        let mut namespaces: Vec<_> = inputs
            .packages
            .iter()
            .map(|p| (&p.package, &p.dependencies))
            .collect();
        namespaces.sort_by_key(|p| p.0);
        let environment = ContentHash::of_parts(
            "hirsel.core-environment.v1",
            [&encode(&(compiler, &inputs.core, namespaces))?[..]],
        );
        for (m, rows) in modules.iter().zip(resolutions) {
            for (ordinal, (item, resolution)) in m.syntax.items.iter().zip(rows).enumerate() {
                let Some(t) = item.tree.node(item.root).token else {
                    continue;
                };
                let path = m.id.definition(item.tree.token_text(&item.text, t));
                let end = header_end(item);
                let digest = ContentHash::of_bytes(&item.text.as_bytes()[..end]);
                let mut reads = BTreeSet::new();
                for name in &resolution.names {
                    if (item.tree.node(name.node).origin.span.start as usize) < end {
                        targets(&name.target, &mut reads);
                    }
                }
                headers.insert(
                    path,
                    Header {
                        digest,
                        reads: reads.into_iter().collect(),
                        anchor: (m.id.clone(), ordinal as u32),
                    },
                );
            }
        }
        Ok(Self {
            headers,
            environment,
        })
    }
    pub fn capture_anchors(
        &self,
        check: &CoreCheck,
        incomplete: &[Incomplete],
    ) -> BTreeMap<(ModuleId, u32), DefPath> {
        let mut anchors = BTreeMap::new();
        let mut capture = |site: &Site| {
            let at = (site.module.clone(), site.item);
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
        if let Some(path) = anchors.get(&(site.module.clone(), site.item))
            && let Some(h) = self.headers.get(path)
        {
            site.module = h.anchor.0.clone();
            site.item = h.anchor.1;
        }
    }
    pub fn for_body(
        &self,
        path: &DefPath,
        item: &Item,
        resolution: &Resolution,
    ) -> io::Result<DefinitionKey> {
        let mut pending = BTreeSet::from([path.clone()]);
        for name in &resolution.names {
            targets(&name.target, &mut pending);
        }
        // Lookup results retain negative, ambiguous and shadowed dependencies, even if
        // no corresponding call was emitted. Header closure supplies transitive aliases.
        for read in &resolution.reads {
            match &read.result {
                LookupResult::Found(t) | LookupResult::Shadowed(t) => targets(t, &mut pending),
                LookupResult::Private(p) | LookupResult::Opaque(p) => {
                    pending.insert(p.clone());
                }
                LookupResult::Ambiguous(ps) => pending.extend(ps.iter().cloned()),
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
            .map(|r| (&r.namespace, &r.scope, &r.spelling, &r.result))
            .collect();
        Ok(DefinitionKey {
            body: ContentHash::of_bytes(item.text.as_bytes()),
            signatures,
            lookups: ContentHash::of_bytes(&encode(&lookups)?),
            environment: self.environment,
        })
    }
}
fn targets(t: &Target, out: &mut BTreeSet<DefPath>) {
    match t {
        Target::Definition(p) => {
            out.insert(p.clone());
        }
        Target::ContextualConstructor { candidates, .. } => out.extend(candidates.iter().cloned()),
        _ => {}
    }
}
fn header_end(item: &Item) -> usize {
    if item.tree.node(item.root).kind != NodeKind::Function {
        return item.text.len();
    }
    item.tree
        .children(item.root)
        .iter()
        .find_map(|&c| {
            let n = item.tree.node(c);
            (n.kind == NodeKind::Block).then_some(n.origin.span.start as usize)
        })
        .unwrap_or(item.text.len())
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

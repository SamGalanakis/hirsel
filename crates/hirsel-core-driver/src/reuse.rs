//! Definition keys keep implementation edits separate from signature dependencies.
use super::*;
use hirsel_core_resolve::{LookupResult, LookupScope, Target};
use hirsel_core_syntax::{Item, NodeKind};
use std::collections::BTreeSet;

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
    pub(crate) reads: Vec<DefPath>,
    pub(crate) body_reads: Vec<DefPath>,
    pub(crate) has_body: bool,
    pub(crate) header_end: u32,
    pub(crate) site: Site,
    pub(crate) anchor: (ModuleId, u32),
}
pub(crate) struct Keys {
    pub(crate) headers: BTreeMap<DefPath, Header>,
    environment: ContentHash,
    paths: Vec<DefPath>,
    modules: Vec<ModulePath>,
    packages: Vec<PackageId>,
    dependencies: Vec<Option<HeaderRow>>,
    anchors: BTreeMap<(ModuleId, u32), DefPath>,
}
struct HeaderRow {
    digest: ContentHash,
    reads: Vec<Result<usize, DefPath>>,
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
                let mut body_reads = BTreeSet::new();
                for name in &resolution.names {
                    if (item.tree.node(name.node).origin.span.start as usize) < end {
                        targets(index, &paths, m.id, name.target, &mut reads);
                    } else {
                        targets(index, &paths, m.id, name.target, &mut body_reads);
                    }
                }
                headers.insert(
                    path,
                    Header {
                        digest,
                        has_body: end < item.source_text().len(),
                        header_end: end as u32,
                        site: index
                            .get(index.definition(m.id, item.symbol(t)).unwrap())
                            .site,
                        body_reads: body_reads.into_iter().collect(),
                        reads: reads.into_iter().collect(),
                        anchor: (m.id, ordinal as u32),
                    },
                );
            }
        }
        let ids: BTreeMap<_, _> = paths.iter().enumerate().map(|(i, p)| (p, i)).collect();
        let dependencies = paths
            .iter()
            .map(|path| {
                headers.get(path).map(|h| HeaderRow {
                    digest: h.digest,
                    reads: h
                        .reads
                        .iter()
                        .map(|p| ids.get(p).copied().ok_or_else(|| p.clone()))
                        .collect(),
                })
            })
            .collect();
        let anchors = headers.iter().map(|(p, h)| (h.anchor, p.clone())).collect();
        Ok(Self {
            dependencies,
            anchors,
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
        hirsel_core_front::cost_scope!("phase", "driver keys/anchors");
        let mut anchors = BTreeMap::new();
        let mut capture = |site: &Site| {
            let at = (site.module, site.item);
            if let Some(path) = self.anchors.get(&at) {
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
        hirsel_core_front::cost_scope!("phase", "driver keys/anchors");
        let module = self.headers[path].anchor.0;
        let token = item.tree.node(item.root).token.unwrap();
        let own = index.definition(module, item.symbol(token)).unwrap();
        let mut pending = vec![own.0 as usize];
        for name in &resolution.names {
            target_ids(index, module, name.target, &mut pending);
        }
        // Negative, ambiguous and shadowed lookup dependencies remain unchanged.
        for read in &resolution.reads {
            match &read.result {
                LookupResult::Found(t) | LookupResult::Shadowed(t) => {
                    target_ids(index, module, *t, &mut pending);
                }
                LookupResult::Private(p) | LookupResult::Opaque(p) => {
                    pending.push(p.0 as usize);
                }
                LookupResult::Ambiguous | LookupResult::Missing => {}
            }
        }
        let mut signatures = BTreeMap::new();
        let mut seen = vec![false; self.paths.len()];
        while let Some(id) = pending.pop() {
            if std::mem::replace(&mut seen[id], true) {
                continue;
            }
            let h = self.dependencies[id].as_ref().ok_or_else(|| {
                io::Error::other(format!(
                    "core signature demand outside reached definitions: {}",
                    self.paths[id]
                ))
            })?;
            signatures.insert(self.paths[id].clone(), h.digest);
            for read in &h.reads {
                match read {
                    Ok(id) => pending.push(*id),
                    Err(path) => {
                        return Err(io::Error::other(format!(
                            "core signature demand outside reached definitions: {path}"
                        )));
                    }
                }
            }
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
fn target_ids(index: &NameIndex, module: ModuleId, target: Target, out: &mut Vec<usize>) {
    match target {
        Target::Definition(d) => out.push(d.0 as usize),
        Target::ContextualConstructor { name } => {
            out.extend(
                index
                    .constructor_candidates(module, name)
                    .map(|d| d.0 as usize),
            );
        }
        _ => {}
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
        hirsel_core_front::cost_scope!("non-check", "compiler executable identity");
        // The linker has already identified this executable. Read that immutable
        // note, rather than scanning and hashing the whole compiler on every launch.
        // /proc/self/exe stays bound to the loaded inode across binary replacement.
        let path = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            std::env::current_exe()?
        };
        compiler_stream_identity(&mut std::fs::File::open(path)?)
    });
    ID.as_ref()
        .copied()
        .map_err(|e| io::Error::other(e.to_string()))
}

fn compiler_stream_identity(reader: &mut (impl io::Read + io::Seek)) -> io::Result<ContentHash> {
    use object::Object as _;
    let linked = {
        let cache = object::read::ReadCache::new(&mut *reader);
        object::File::parse(&cache).ok().and_then(|binary| {
            binary
                .build_id()
                .ok()
                .flatten()
                .filter(|id| !id.is_empty())
                .map(|id| ContentHash::of_parts("hirsel.compiler-link-id.v1", [id]))
        })
    };
    if let Some(identity) = linked {
        return Ok(identity);
    }
    // Linkers/formats without a build ID retain the complete executable identity.
    reader.rewind()?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(ContentHash::of_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiler_link_id_changes_identity_and_missing_notes_keep_full_hashing() {
        // Minimal ELF64 executable: one PT_NOTE at byte 120, a GNU build ID of
        // twenty bytes. The note and ELF field offsets are from the ELF ABI.
        let mut elf = vec![0u8; 156];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[58..60].copy_from_slice(&64u16.to_le_bytes());
        elf[64..68].copy_from_slice(&4u32.to_le_bytes());
        elf[72..80].copy_from_slice(&120u64.to_le_bytes());
        elf[96..104].copy_from_slice(&36u64.to_le_bytes());
        elf[104..112].copy_from_slice(&36u64.to_le_bytes());
        elf[112..120].copy_from_slice(&4u64.to_le_bytes());
        elf[120..124].copy_from_slice(&4u32.to_le_bytes());
        elf[124..128].copy_from_slice(&20u32.to_le_bytes());
        elf[128..132].copy_from_slice(&3u32.to_le_bytes());
        elf[132..136].copy_from_slice(b"GNU\0");
        for (i, b) in elf[136..].iter_mut().enumerate() {
            *b = i as u8;
        }
        // Independent SHA-256 of the domain and ID, each framed by u64 LE length.
        assert_eq!(
            compiler_stream_identity(&mut io::Cursor::new(&elf))
                .unwrap()
                .0,
            [
                169, 29, 73, 236, 7, 183, 253, 11, 157, 25, 184, 241, 93, 180, 53, 99, 6, 172, 106,
                55, 33, 198, 174, 102, 215, 24, 56, 121, 46, 189, 104, 222
            ]
        );
        elf[136] = 255;
        assert_eq!(
            compiler_stream_identity(&mut io::Cursor::new(&elf))
                .unwrap()
                .0,
            [
                178, 69, 107, 16, 225, 205, 99, 73, 2, 221, 87, 102, 200, 207, 27, 15, 58, 15, 242,
                147, 200, 57, 12, 20, 206, 186, 7, 15, 122, 208, 179, 22
            ]
        );
        assert_eq!(
            compiler_stream_identity(&mut io::Cursor::new(b"compiler-without-a-note"))
                .unwrap()
                .0,
            [
                88, 28, 171, 172, 129, 86, 81, 147, 24, 134, 60, 221, 161, 178, 132, 193, 24, 62,
                80, 145, 143, 22, 81, 140, 69, 22, 97, 225, 130, 98, 156, 238
            ]
        );
    }
}

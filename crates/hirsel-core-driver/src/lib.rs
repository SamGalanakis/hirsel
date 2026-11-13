//! One core-only request path from an immutable source snapshot to published products.
//! Sources and signatures are explicit inputs; no old compiler or std image is loaded.
mod meter;
mod reuse;
mod store;
use bincode::Options;
pub use meter::{Measurements, Meter, Stage};
pub use reuse::DefinitionKey;
use reuse::{Keys, compiler_identity};
pub use store::Store;

use hirsel_core_front::{CoreCheck, Emitter, ModuleInput, Program};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use hirsel_core_ir::Module;
use hirsel_core_resolve::{
    CorpusManifest, Definition, ModuleId, ModulePath, NameIndex, PackageManifest, Refusal,
    Resolution, Site, resolve,
};
use hirsel_core_syntax::{ParseMode, Symbols, SyntaxFile, parse_bytes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use store::Record;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

pub fn package(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

/// All producers are enumerated before any stage runs. Explicit manifests can select
/// another source convention; the CLI's directory convention names a module per file.
pub struct CheckRequest {
    pub inputs: CorpusManifest,
    pub store: PathBuf,
}
impl CheckRequest {
    pub fn directory(dir: &Path, libraries: &Path, store: &Path) -> io::Result<Self> {
        let core = PackageManifest::directory(&libraries.join("core"), package("core"), true)?;
        let mut std = PackageManifest::directory(&libraries.join("std"), package("std"), true)?;
        std.dependencies.push(package("core"));
        let mut app = PackageManifest::directory(dir, package("app"), false)?;
        app.dependencies.extend([package("core"), package("std")]);
        Ok(Self {
            inputs: CorpusManifest {
                core: package("core"),
                packages: vec![core, std, app],
            },
            store: store.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileProduct {
    pub module: ModuleId,
    pub path: PathBuf,
    pub source: ContentHash,
    pub syntax: SyntaxFile,
    pub names: Vec<Definition>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Incomplete {
    pub primary: Site,
    pub message: String,
}

/// A body and its result are kept together: CoreIR ids refer only to this module.
/// Callee rows are signature-only imports, so a callee-body edit does not alter it.
pub struct DefinitionProduct {
    pub definition: DefPath,
    pub check: CoreCheck,
    pub ir: Module,
    pub resolved: bool,
    pub key: DefinitionKey,
    pub anchors: BTreeMap<(ModuleId, u32), DefPath>,
    pub incomplete: Vec<Incomplete>,
}
#[derive(Clone)]
pub struct CheckProduct {
    pub index: Arc<NameIndex>,
    pub files: Vec<Arc<FileProduct>>,
    pub definitions: BTreeMap<DefPath, Arc<DefinitionProduct>>,
    pub diagnostics: Vec<Refusal>,
    pub incomplete: Vec<Incomplete>,
    pub bytes: usize,
    pub rechecked: Vec<DefPath>,
    pub root: ContentHash,
    pub snapshot: ContentHash,
}
impl CheckProduct {
    fn empty(index: Arc<NameIndex>) -> Self {
        Self {
            index,
            files: vec![],
            definitions: BTreeMap::new(),
            diagnostics: vec![],
            incomplete: vec![],
            bytes: 0,
            rechecked: vec![],
            root: ContentHash::default(),
            snapshot: ContentHash::default(),
        }
    }
    pub fn accepted(&self) -> bool {
        self.diagnostics.is_empty() && self.incomplete.is_empty()
    }
    pub fn demand(&self, definition: &DefPath) -> io::Result<&DefinitionProduct> {
        self.definitions
            .get(definition)
            .map(Arc::as_ref)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "core demand outside reached definitions: {definition}"
                ))
            })
    }
}

#[derive(Default)]
pub struct Driver {
    pub published: Option<Arc<CheckProduct>>,
    store: Option<PathBuf>,
}

// Borrow the native result tables for output. No cloned IR, resolver scratch or text.
#[derive(Serialize)]
struct DiskCheckRef<'a> {
    anchors: &'a BTreeMap<(ModuleId, u32), DefPath>,
    key: &'a DefinitionKey,
    definition: &'a DefPath,
    refusals: &'a [Refusal],
    incomplete: &'a [Incomplete],
    counts: [u64; 6],
    function: Option<u32>,
    ir: &'a Module,
    resolved: bool,
}
#[derive(Deserialize)]
struct DiskCheck {
    anchors: BTreeMap<(ModuleId, u32), DefPath>,
    key: DefinitionKey,
    definition: DefPath,
    refusals: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
    counts: [u64; 6],
    function: Option<u32>,
    ir: Module,
    resolved: bool,
}
#[derive(Serialize, Deserialize)]
struct Root {
    snapshot: ContentHash,
    bytes: usize,
    identities: Record,
    files: Vec<Record>,
    definitions: BTreeMap<DefPath, Record>,
    diagnostics: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
}
struct Cache {
    bytes: Vec<u8>,
    root: Root,
}
impl Cache {
    fn open(store: &Store, hash: ContentHash) -> io::Result<Option<Self>> {
        let Some(bytes) = store.get(hash)? else {
            return Ok(None);
        };
        let Some(offset) = bytes.len().checked_sub(8) else {
            return Ok(None);
        };
        let start = u64::from_le_bytes(bytes[offset..].try_into().unwrap());
        let Some(len) = (offset as u64).checked_sub(start) else {
            return Ok(None);
        };
        let Some(slice) = (Record { start, len }).bytes(&bytes) else {
            return Ok(None);
        };
        let Some(root) = decode(slice) else {
            return Ok(None);
        };
        Ok(Some(Self { bytes, root }))
    }
    fn definition(&self, path: &DefPath, key: &DefinitionKey) -> Option<DefinitionProduct> {
        let disk: DiskCheck = decode(self.root.definitions.get(path)?.bytes(&self.bytes)?)?;
        (disk.definition == *path && disk.key == *key).then(|| from_disk(disk))
    }
    fn product(&self, hash: ContentHash) -> Option<CheckProduct> {
        let mut result = CheckProduct {
            diagnostics: self.root.diagnostics.clone(),
            incomplete: self.root.incomplete.clone(),
            root: hash,
            snapshot: self.root.snapshot,
            bytes: self.root.bytes,
            ..CheckProduct::empty(Arc::new(decode(self.root.identities.bytes(&self.bytes)?)?))
        };
        for &record in &self.root.files {
            result
                .files
                .push(Arc::new(decode(record.bytes(&self.bytes)?)?));
        }
        for (path, &record) in &self.root.definitions {
            result.definitions.insert(
                path.clone(),
                Arc::new(from_disk(decode(record.bytes(&self.bytes)?)?)),
            );
        }
        Some(result)
    }
}

impl Driver {
    pub fn check(
        &mut self,
        mut request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        request
            .inputs
            .packages
            .sort_by(|a, b| a.package.cmp(&b.package));
        for p in &mut request.inputs.packages {
            p.dependencies.sort();
            p.sources.sort_by(|a, b| {
                (&a.modules, &a.file, a.signatures).cmp(&(&b.modules, &b.file, b.signatures))
            });
        }
        // Read source bytes exactly once. The source digest is reused by file publication.
        let mut sources = vec![];
        let mut bytes_total = 0;
        for p in &request.inputs.packages {
            for s in &p.sources {
                let bytes = std::fs::read(&s.file)?;
                let hash = ContentHash::of_bytes(&bytes);
                bytes_total += bytes.len();
                sources.push((
                    ModulePath {
                        package: p.package.clone(),
                        modules: s.modules.clone(),
                    },
                    s.file.clone(),
                    s.signatures,
                    bytes,
                    hash,
                ));
            }
        }
        let source_stamps: Vec<_> = sources.iter().map(|s| s.4).collect();
        let snapshot = ContentHash::of_bytes(&encode(&(
            compiler_identity()?,
            &request.inputs,
            source_stamps,
        ))?);
        let store = Store::new(&request.store)?;
        meter.finish("load/hash");
        let same_store = self.store.as_ref() == Some(&request.store);
        if let Some(old) = self
            .published
            .as_ref()
            .filter(|p| p.snapshot == snapshot && same_store)
        {
            let mut reused = old.as_ref().clone();
            reused.rechecked.clear();
            let reused = Arc::new(reused);
            self.published = Some(Arc::clone(&reused));
            meter.finish("snapshot reuse");
            return Ok(reused);
        }
        let prior_hash = store.selection("check-root")?;
        let prior = prior_hash
            .map(|hash| Cache::open(&store, hash))
            .transpose()?
            .flatten();
        if let Some(prior) = &prior
            && prior.root.snapshot == snapshot
            && let Some(cached) = prior.product(prior_hash.unwrap())
        {
            let cached = Arc::new(cached);
            self.published = Some(Arc::clone(&cached));
            self.store = Some(request.store.clone());
            meter.finish("snapshot reuse");
            return Ok(cached);
        }
        let mut symbols = Symbols::default();
        for p in &request.inputs.packages {
            p.intern(&mut symbols);
        }
        let mut modules = vec![];
        let mut syntax_diagnostics = vec![];
        let mut syntax_incomplete = vec![];
        for (path, _, signatures, bytes, _) in &sources {
            let output = parse_bytes(
                bytes,
                if *signatures {
                    ParseMode::Signatures
                } else {
                    ParseMode::Source
                },
                &mut symbols,
            );
            syntax_diagnostics.extend(output.diagnostics.into_iter().map(|d| (path, d)));
            if let Some(r) = output.resource_refusal {
                syntax_incomplete.push((path, format!("parser resource refusal: {r:?}")));
            }
            modules.push((path, output.syntax));
        }
        meter.finish("parse");
        let mut index = NameIndex::new(Arc::new(symbols));
        index.add_manifests(&request.inputs.packages);
        let modules: Vec<_> = modules
            .into_iter()
            .map(|(path, syntax)| ModuleInput {
                id: index.module(path).unwrap(),
                syntax,
            })
            .collect();
        for m in &modules {
            for (i, item) in m.syntax.items().enumerate() {
                index.index_item(m.id, i as u32, item);
            }
        }
        if let Some(core) = index
            .symbols
            .find(&request.inputs.core.name)
            .zip(index.symbols.find(&request.inputs.core.version))
            .and_then(|(n, v)| index.package(n, v))
        {
            index.configure_core(core);
        }
        let index = Arc::new(index);
        let mut result = CheckProduct::empty(Arc::clone(&index));
        result.bytes = bytes_total;
        result.snapshot = snapshot;
        for (path, d) in syntax_diagnostics {
            result
                .diagnostics
                .push(Refusal::syntax(index.module(path).unwrap(), &d));
        }
        for (path, message) in syntax_incomplete {
            result.incomplete.push(Incomplete {
                primary: Site {
                    module: index.module(path).unwrap(),
                    item: u32::MAX,
                    span: Default::default(),
                },
                message,
            });
        }
        meter.finish("index");
        let resolutions: Vec<Vec<Arc<Resolution>>> = modules
            .iter()
            .map(|m| {
                m.syntax
                    .items()
                    .enumerate()
                    .map(|(i, item)| Arc::new(resolve(&index, m.id, i as u32, item)))
                    .collect()
            })
            .collect();
        for rows in &resolutions {
            for row in rows {
                if let Some(site) = &row.resource_refusal {
                    result.incomplete.push(Incomplete {
                        primary: *site,
                        message: "resolver resource refusal".into(),
                    });
                }
            }
        }
        meter.finish("resolve");
        let keys = Keys::new(
            &index,
            &modules,
            &resolutions,
            &request.inputs,
            compiler_identity()?,
        )?;
        {
            let p = Program::declare_resolved(&index, &modules, resolutions.clone());
            result.diagnostics.extend(p.refusals.iter().cloned());
            result.incomplete.extend(p.gaps.iter().map(|g| Incomplete {
                primary: g.site,
                message: g.what.into(),
            }));
            meter.finish("declarations/layout");
            for (f, fd) in p.fns.iter().enumerate() {
                if fd.body.is_none() {
                    continue;
                }
                let resolution =
                    Arc::clone(&resolutions[fd.at.module as usize][fd.at.item as usize]);
                let item = modules[fd.at.module as usize]
                    .syntax
                    .item(fd.at.item as usize);
                let key = keys.for_body(&index, &fd.path, item, &resolution)?;
                let resident = self
                    .published
                    .as_ref()
                    .and_then(|p| p.definitions.get(&fd.path))
                    .filter(|d| d.key == key)
                    .cloned();
                let cached = if resident.is_some() {
                    resident
                } else {
                    prior
                        .as_ref()
                        .and_then(|p| p.definition(&fd.path, &key))
                        .map(Arc::new)
                };
                let product = if let Some(product) = cached {
                    product
                } else {
                    let mut emitter = Emitter::new();
                    let check = if !resolution.diagnostics.is_empty()
                        || resolution.resource_refusal.is_some()
                    {
                        CoreCheck {
                            refusals: resolution.diagnostics.clone(),
                            ..Default::default()
                        }
                    } else {
                        p.check_lower(f, &mut emitter)
                    };
                    let incomplete: Vec<Incomplete> = check
                        .gaps
                        .iter()
                        .map(|g| Incomplete {
                            primary: g.site,
                            message: g.what.into(),
                        })
                        .collect();
                    let anchors = keys.capture_anchors(&check, &incomplete);
                    result.rechecked.push(fd.path.clone());
                    Arc::new(DefinitionProduct {
                        definition: fd.path.clone(),
                        check,
                        ir: emitter.module,
                        resolved: resolution.diagnostics.is_empty(),
                        key,
                        anchors,
                        incomplete,
                    })
                };
                if product.resolved {
                    for r in &product.check.refusals {
                        let mut r = r.clone();
                        keys.bind_site(&product.anchors, &mut r.primary);
                        for site in &mut r.causal {
                            keys.bind_site(&product.anchors, site);
                        }
                        result.diagnostics.push(r);
                    }
                }
                for g in &product.incomplete {
                    let mut g = g.clone();
                    keys.bind_site(&product.anchors, &mut g.primary);
                    result.incomplete.push(g);
                }
                result.definitions.insert(fd.path.clone(), product);
            }
            drop(p);
        }
        meter.finish("check/lower");
        let mut names: BTreeMap<ModuleId, Vec<Definition>> = BTreeMap::new();
        for (_, d) in index.definitions() {
            names.entry(d.site.module).or_default().push(*d);
        }
        for (m, (_, path, _, _, source)) in modules.into_iter().zip(sources) {
            let module_names = names.remove(&m.id).unwrap_or_default();
            result.files.push(Arc::new(FileProduct {
                module: m.id,
                path,
                source,
                syntax: m.syntax,
                names: module_names,
            }));
        }
        meter.finish("publish/assemble products");
        result.root = store.archive(|writer| {
            let identities = writer.record(index.as_ref())?;
            let mut files = Vec::with_capacity(result.files.len());
            for file in &result.files {
                files.push(writer.record(file.as_ref())?);
            }
            meter.finish("publish/file tables");
            let mut definitions = BTreeMap::new();
            for (path, d) in &result.definitions {
                let c = d.check.counts;
                let disk = DiskCheckRef {
                    anchors: &d.anchors,
                    key: &d.key,
                    definition: path,
                    refusals: &d.check.refusals,
                    incomplete: &d.incomplete,
                    counts: [c.nodes, c.uses, c.places, c.calls, c.types, c.bytes],
                    function: d.check.function.map(|f| f.0),
                    ir: &d.ir,
                    resolved: d.resolved,
                };
                definitions.insert(path.clone(), writer.record(&disk)?);
            }
            meter.finish("publish/definition tables");
            // Root rows use borrowed diagnostics too. Footer owns only the offset index.
            let footer = (
                result.snapshot,
                result.bytes,
                identities,
                files,
                definitions,
                &result.diagnostics,
                &result.incomplete,
            );
            let record = writer.record(&footer)?;
            use std::io::Write;
            writer.write_all(&record.start.to_le_bytes())?;
            meter.finish("publish/footer");
            Ok(())
        })?;
        store.publish_root(result.root)?;
        let result = Arc::new(result);
        self.published = Some(Arc::clone(&result));
        self.store = Some(request.store.clone());
        meter.finish("publish/commit and Arc move");
        drop(index);
        drop(resolutions);
        drop(keys);
        drop(prior);
        drop(request);
        meter.finish("publish/scratch cleanup");
        Ok(result)
    }
}
fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    bincode::serialize(value).map_err(io::Error::other)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    bincode::DefaultOptions::new()
        .with_varint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()
}
fn from_disk(disk: DiskCheck) -> DefinitionProduct {
    let [nodes, uses, places, calls, types, bytes] = disk.counts;
    DefinitionProduct {
        definition: disk.definition,
        anchors: disk.anchors,
        key: disk.key,
        incomplete: disk.incomplete,
        resolved: disk.resolved,
        ir: disk.ir,
        check: CoreCheck {
            refusals: disk.refusals,
            gaps: vec![],
            function: disk.function.map(hirsel_core_ir::FuncId),
            counts: hirsel_core_front::Counts {
                nodes,
                uses,
                places,
                calls,
                types,
                bytes,
            },
        },
    }
}

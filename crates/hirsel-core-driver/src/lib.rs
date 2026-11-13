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
use hirsel_core_ir::{Module, text};
use hirsel_core_resolve::{
    CorpusManifest, Definition, Environment, ModuleId, NameIndex, PackageManifest, Refusal,
    Resolution, Site, resolve,
};
use hirsel_core_syntax::{ParseMode, SyntaxFile, parse_bytes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub resolutions: Vec<Arc<Resolution>>,
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
    pub resolution: Arc<Resolution>,
    pub key: DefinitionKey,
    pub anchors: BTreeMap<(ModuleId, u32), DefPath>,
    pub incomplete: Vec<Incomplete>,
    stored: std::sync::OnceLock<ContentHash>,
}
#[derive(Clone, Default)]
pub struct CheckProduct {
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

#[derive(Serialize, Deserialize)]
struct DiskCheck {
    anchors: BTreeMap<(ModuleId, u32), DefPath>,
    key: DefinitionKey,
    definition: DefPath,
    refusals: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
    counts: [u64; 6],
    function: Option<u32>,
    ir: String,
    resolution: Resolution,
}
#[derive(Serialize, Deserialize)]
struct Root {
    snapshot: ContentHash,
    bytes: usize,
    files: Vec<ContentHash>,
    definitions: Vec<(DefPath, ContentHash)>,
    diagnostics: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
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
        let mut result = CheckProduct::default();
        for p in &request.inputs.packages {
            for s in &p.sources {
                let bytes = std::fs::read(&s.file)?;
                let hash = ContentHash::of_bytes(&bytes);
                result.bytes += bytes.len();
                sources.push((
                    ModuleId {
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
        result.snapshot = ContentHash::of_bytes(&encode(&(
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
            .filter(|p| p.snapshot == result.snapshot && same_store)
        {
            let mut reused = old.as_ref().clone();
            reused.rechecked.clear();
            let reused = Arc::new(reused);
            self.published = Some(Arc::clone(&reused));
            meter.finish("snapshot reuse");
            return Ok(reused);
        }
        if let Some(root) = store.selection("check-root")?
            && let Some(cached) = load_root(&store, root, result.snapshot)?
        {
            let cached = Arc::new(cached);
            self.published = Some(Arc::clone(&cached));
            self.store = Some(request.store.clone());
            meter.finish("snapshot reuse");
            return Ok(cached);
        }
        let mut modules = vec![];
        for (id, _, signatures, bytes, _) in &sources {
            let output = parse_bytes(
                bytes,
                if *signatures {
                    ParseMode::Signatures
                } else {
                    ParseMode::Source
                },
            );
            result.diagnostics.extend(
                output
                    .diagnostics
                    .iter()
                    .map(|d| Refusal::syntax(id.clone(), d)),
            );
            if let Some(r) = output.resource_refusal {
                result.incomplete.push(Incomplete {
                    primary: Site {
                        module: id.clone(),
                        item: u32::MAX,
                        span: Default::default(),
                    },
                    message: format!("parser resource refusal: {r:?}"),
                });
            }
            modules.push(ModuleInput {
                id: id.clone(),
                syntax: output.syntax,
            });
        }
        meter.finish("parse");
        let mut environment = Environment::default();
        for p in &request.inputs.packages {
            for dependency in &p.dependencies {
                environment
                    .dependencies
                    .entry(p.package.clone())
                    .or_default()
                    .insert(
                        dependency.name.clone(),
                        ModuleId {
                            package: dependency.clone(),
                            modules: vec![],
                        },
                    );
            }
        }
        let mut index = NameIndex::new(environment);
        for m in &modules {
            index.add_module(&m.id);
            for (i, item) in m.syntax.items.iter().enumerate() {
                index.index_item(&m.id, i as u32, item);
            }
        }
        index.configure_core(request.inputs.core.clone());
        meter.finish("index");
        let resolutions: Vec<Vec<Arc<Resolution>>> = modules
            .iter()
            .map(|m| {
                m.syntax
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| Arc::new(resolve(&index, &m.id, i as u32, item)))
                    .collect()
            })
            .collect();
        for rows in &resolutions {
            for row in rows {
                if let Some(site) = &row.resource_refusal {
                    result.incomplete.push(Incomplete {
                        primary: site.clone(),
                        message: "resolver resource refusal".into(),
                    });
                }
            }
        }
        meter.finish("resolve");
        let keys = Keys::new(
            &modules,
            &resolutions,
            &request.inputs,
            compiler_identity()?,
        )?;
        {
            let p = Program::declare_resolved(&index, &modules, resolutions.clone());
            result.diagnostics.extend(p.refusals.iter().cloned());
            result.incomplete.extend(p.gaps.iter().map(|g| Incomplete {
                primary: g.site.clone(),
                message: g.what.into(),
            }));
            meter.finish("declarations/layout");
            for (f, fd) in p.fns.iter().enumerate() {
                if fd.body.is_none() {
                    continue;
                }
                let resolution =
                    Arc::clone(&resolutions[fd.at.module as usize][fd.at.item as usize]);
                let item = &modules[fd.at.module as usize].syntax.items[fd.at.item as usize];
                let key = keys.for_body(&fd.path, item, &resolution)?;
                let resident = self
                    .published
                    .as_ref()
                    .and_then(|p| p.definitions.get(&fd.path))
                    .filter(|d| d.key == key)
                    .cloned();
                let cached = if resident.is_some() {
                    resident
                } else {
                    load_definition(&store, &fd.path, &key)?.map(Arc::new)
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
                            primary: g.site.clone(),
                            message: g.what.into(),
                        })
                        .collect();
                    let anchors = keys.capture_anchors(&check, &incomplete);
                    result.rechecked.push(fd.path.clone());
                    Arc::new(DefinitionProduct {
                        definition: fd.path.clone(),
                        check,
                        ir: emitter.module,
                        resolution,
                        key,
                        anchors,
                        incomplete,
                        stored: std::sync::OnceLock::new(),
                    })
                };
                if product.resolution.diagnostics.is_empty() {
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
        for d in index.definitions() {
            names
                .entry(d.site.module.clone())
                .or_default()
                .push(d.clone());
        }
        for ((m, rows), (_, path, _, _, source)) in
            modules.into_iter().zip(resolutions).zip(sources)
        {
            let module_names = names.remove(&m.id).unwrap_or_default();
            result.files.push(Arc::new(FileProduct {
                module: m.id,
                path,
                source,
                syntax: m.syntax,
                names: module_names,
                resolutions: rows,
            }));
        }
        let mut root = Root {
            snapshot: result.snapshot,
            bytes: result.bytes,
            files: vec![],
            definitions: vec![],
            diagnostics: result.diagnostics.clone(),
            incomplete: result.incomplete.clone(),
        };
        for f in &result.files {
            root.files.push(store.put(&encode(f.as_ref())?)?);
        }
        for (path, d) in &result.definitions {
            let stored = if same_store && let Some(&stored) = d.stored.get() {
                stored
            } else {
                let c = d.check.counts;
                let disk = DiskCheck {
                    anchors: d.anchors.clone(),
                    key: d.key.clone(),
                    definition: path.clone(),
                    refusals: d.check.refusals.clone(),
                    incomplete: d.incomplete.clone(),
                    counts: [c.nodes, c.uses, c.places, c.calls, c.types, c.bytes],
                    function: d.check.function.map(|f| f.0),
                    ir: text::print(&d.ir),
                    resolution: d.resolution.as_ref().clone(),
                };
                let hash = store.put(&encode(&disk)?)?;
                let _ = d.stored.set(hash);
                hash
            };
            store.select(&definition_selection(path), stored)?;
            root.definitions.push((path.clone(), stored));
        }
        result.root = store.put(&encode(&root)?)?;
        store.publish_root(result.root)?;
        let result = Arc::new(result);
        self.published = Some(Arc::clone(&result));
        self.store = Some(request.store.clone());
        meter.finish("publish");
        Ok(result)
    }
}
fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    bincode::serialize(value).map_err(io::Error::other)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()
}
fn definition_selection(path: &DefPath) -> String {
    format!(
        "definition-{}",
        path.fingerprint()
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn load_definition(
    store: &Store,
    path: &DefPath,
    key: &DefinitionKey,
) -> io::Result<Option<DefinitionProduct>> {
    let Some(hash) = store.selection(&definition_selection(path))? else {
        return Ok(None);
    };
    let Some(bytes) = store.get(hash)? else {
        return Ok(None);
    };
    let Some(disk) = decode::<DiskCheck>(&bytes) else {
        return Ok(None);
    };
    if disk.definition != *path || disk.key != *key {
        return Ok(None);
    }
    Ok(from_disk(disk, hash))
}
fn from_disk(disk: DiskCheck, stored: ContentHash) -> Option<DefinitionProduct> {
    let ir = text::parse(&disk.ir).ok()?;
    let [nodes, uses, places, calls, types, bytes] = disk.counts;
    Some(DefinitionProduct {
        definition: disk.definition,
        anchors: disk.anchors,
        key: disk.key,
        incomplete: disk.incomplete,
        stored: std::sync::OnceLock::from(stored),
        resolution: Arc::new(disk.resolution),
        ir,
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
    })
}
fn load_root(
    store: &Store,
    root: ContentHash,
    snapshot: ContentHash,
) -> io::Result<Option<CheckProduct>> {
    let Some(bytes) = store.get(root)? else {
        return Ok(None);
    };
    let Some(row) = decode::<Root>(&bytes) else {
        return Ok(None);
    };
    if row.snapshot != snapshot {
        return Ok(None);
    }
    let mut result = CheckProduct {
        diagnostics: row.diagnostics,
        incomplete: row.incomplete,
        root,
        snapshot,
        bytes: row.bytes,
        ..Default::default()
    };
    for hash in row.files {
        let Some(bytes) = store.get(hash)? else {
            return Ok(None);
        };
        let Some(file) = decode::<FileProduct>(&bytes) else {
            return Ok(None);
        };
        result.files.push(Arc::new(file));
    }
    for (path, hash) in row.definitions {
        let Some(bytes) = store.get(hash)? else {
            return Ok(None);
        };
        let Some(disk) = decode::<DiskCheck>(&bytes) else {
            return Ok(None);
        };
        let Some(product) = from_disk(disk, hash) else {
            return Ok(None);
        };
        result.definitions.insert(path, Arc::new(product));
    }
    Ok(Some(result))
}

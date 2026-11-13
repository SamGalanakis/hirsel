//! One core-only request path from an immutable source snapshot to published products.
//! Sources and signatures are explicit inputs; no old compiler or std image is loaded.
mod meter;
mod store;
pub use meter::{Measurements, Meter, Stage};
pub use store::Store;

use hirsel_core_front::{CoreCheck, Emitter, ModuleInput, Program};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use hirsel_core_ir::{Module, text};
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
}
pub struct CheckProduct {
    pub index: Arc<NameIndex>,
    pub files: Vec<Arc<FileProduct>>,
    pub definitions: BTreeMap<DefPath, Arc<DefinitionProduct>>,
    pub diagnostics: Vec<Refusal>,
    pub incomplete: Vec<Incomplete>,
    pub bytes: usize,
    pub rechecked: Vec<DefPath>,
    pub root: ContentHash,
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
}

#[derive(Serialize, Deserialize)]
struct DiskCheck {
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
    identities: ContentHash,
    files: Vec<ContentHash>,
    definitions: Vec<(DefPath, ContentHash)>,
    diagnostics: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
}

impl Driver {
    pub fn check(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        // Read source bytes exactly once. The source digest is reused by file publication.
        let mut sources = vec![];
        let mut bytes_total = 0;
        let mut diagnostics = vec![];
        let mut incomplete = vec![];
        let mut symbols = Symbols::default();
        for p in &request.inputs.packages {
            p.intern(&mut symbols);
        }
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
        meter.finish("load/hash");
        let mut modules = vec![];
        for (id, _, signatures, bytes, _) in &sources {
            let output = parse_bytes(
                bytes,
                if *signatures {
                    ParseMode::Signatures
                } else {
                    ParseMode::Source
                },
                &mut symbols,
            );
            diagnostics.extend(output.diagnostics.iter().map(|d| (id.clone(), d.clone())));
            if let Some(r) = output.resource_refusal {
                incomplete.push((id.clone(), format!("parser resource refusal: {r:?}")));
            }
            modules.push((id.clone(), output.syntax));
        }
        meter.finish("parse");
        let mut index = NameIndex::new(Arc::new(symbols));
        index.add_manifests(&request.inputs.packages);
        let modules = modules
            .into_iter()
            .map(|(path, syntax)| ModuleInput {
                id: index.module(&path).unwrap(),
                syntax,
            })
            .collect::<Vec<_>>();
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
        let mut result = CheckProduct {
            index: Arc::new(index),
            files: vec![],
            definitions: BTreeMap::new(),
            diagnostics: vec![],
            incomplete: vec![],
            bytes: bytes_total,
            rechecked: vec![],
            root: ContentHash::of_bytes(b""),
        };
        let index = result.index.clone();
        for (path, d) in diagnostics {
            result
                .diagnostics
                .push(Refusal::syntax(index.module(&path).unwrap(), &d));
        }
        for (path, message) in incomplete {
            result.incomplete.push(Incomplete {
                primary: Site {
                    module: index.module(&path).unwrap(),
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
                        primary: site.clone(),
                        message: "resolver resource refusal".into(),
                    });
                }
            }
        }
        meter.finish("resolve");
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
                if !resolution.diagnostics.is_empty() || resolution.resource_refusal.is_some() {
                    continue;
                }
                let mut emitter = Emitter::new();
                let check = p.check_lower(f, &mut emitter);
                result.diagnostics.extend(check.refusals.iter().cloned());
                result
                    .incomplete
                    .extend(check.gaps.iter().map(|g| Incomplete {
                        primary: g.site.clone(),
                        message: g.what.into(),
                    }));
                result.rechecked.push(fd.path.clone());
                result.definitions.insert(
                    fd.path.clone(),
                    Arc::new(DefinitionProduct {
                        definition: fd.path.clone(),
                        check,
                        ir: emitter.module,
                        resolution,
                    }),
                );
            }
            drop(p);
        }
        meter.finish("check/lower");
        let mut names: BTreeMap<ModuleId, Vec<Definition>> = BTreeMap::new();
        for (_, d) in index.definitions() {
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
        let store = Store::new(&request.store)?;
        let mut root = Root {
            identities: store.put(&encode(index.as_ref())?)?,
            files: vec![],
            definitions: vec![],
            diagnostics: result.diagnostics.clone(),
            incomplete: result.incomplete.clone(),
        };
        for f in &result.files {
            root.files.push(store.put(&encode(f.as_ref())?)?);
        }
        for (path, d) in &result.definitions {
            let c = d.check.counts;
            let disk = DiskCheck {
                definition: path.clone(),
                refusals: d.check.refusals.clone(),
                incomplete: d
                    .check
                    .gaps
                    .iter()
                    .map(|g| Incomplete {
                        primary: g.site.clone(),
                        message: g.what.into(),
                    })
                    .collect(),
                counts: [c.nodes, c.uses, c.places, c.calls, c.types, c.bytes],
                function: d.check.function.map(|f| f.0),
                ir: text::print(&d.ir),
                resolution: d.resolution.as_ref().clone(),
            };
            root.definitions
                .push((path.clone(), store.put(&encode(&disk)?)?));
        }
        result.root = store.put(&encode(&root)?)?;
        store.publish_root(result.root)?;
        let result = Arc::new(result);
        self.published = Some(Arc::clone(&result));
        meter.finish("publish");
        Ok(result)
    }
}
fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    bincode::serialize(value).map_err(io::Error::other)
}

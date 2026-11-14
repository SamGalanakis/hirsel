//! One core-only request path from an immutable source snapshot to published products.
//! Sources and signatures are explicit inputs; no old compiler or std image is loaded.
pub mod cli;
pub mod cli_tools;
mod diagnostics;
mod image;
mod image_index;
pub mod installation;
mod product;
pub use product::PackageProduct;
mod callable_variants;
mod check_index;
mod instances;
mod native;
mod native_debug;
mod native_entry;
mod native_facts;
mod native_library;
mod native_object;
mod native_sources;
mod reachability;
mod test_run;
mod tool_inputs;
pub use native::{Backend, BuildProduct, NativeOptions, NativeStop, ObjectProduct, grant_kind};
pub use reachability::{GapStop, Reachable};
pub use test_run::{Runner, TestOutcome};
mod meter;
pub use diagnostics::{Diagnostic, IncompleteDiagnostic, ResourceDiagnostic, SourceSite};
pub use image::{CoreImage, LibraryInput};
pub use instances::{InstanceStop, collect_instances};
mod reuse;
mod store;
use bincode::Options;
pub use meter::{Measurements, Meter, Stage};
pub use reuse::DefinitionKey;
use reuse::{Keys, compiler_identity};
pub use store::Store;
pub use tool_inputs::link_driver;

use hirsel_core_front::{CoreCheck, Emitter, ModuleInput, Program};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use hirsel_core_ir::Module;
use hirsel_core_resolve::{
    CorpusManifest, Definition, DefinitionId, ModuleId, ModulePath, NameIndex, PackageManifest,
    Refusal, Resolution, Site, resolve,
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
#[derive(Clone)]
pub struct CheckRequest {
    pub inputs: CorpusManifest,
    pub store: PathBuf,
    pub library: LibraryInput,
    pub entry: Option<DefPath>,
}
impl CheckRequest {
    fn source_std(root: &Path) -> io::Result<PackageManifest> {
        let mut manifest = PackageManifest::directory(root, package("std"), true)?;
        manifest.dependencies.push(package("core"));
        Ok(manifest)
    }

    pub fn libraries(libraries: &Path, store: &Path) -> io::Result<Self> {
        let core = if libraries == Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core") {
            PackageProduct::bundled_manifest(&libraries.join("core"))
        } else {
            PackageManifest::directory(&libraries.join("core"), package("core"), true)?
        };
        let std = Self::source_std(&libraries.join("std"))?;
        Ok(Self {
            inputs: CorpusManifest {
                core: package("core"),
                packages: vec![core, std],
            },
            store: store.to_owned(),
            library: LibraryInput::Sources,
            entry: None,
        })
    }
    pub fn with_image(dir: &Path, store: &Path, image: Arc<CoreImage>) -> io::Result<Self> {
        let mut app = PackageManifest::directory(dir, package("app"), false)?;
        app.dependencies.extend(
            image
                .directory()?
                .inputs
                .packages
                .iter()
                .map(|p| p.package.clone()),
        );
        Ok(Self {
            inputs: CorpusManifest {
                core: image.directory()?.inputs.core.clone(),
                packages: vec![app],
            },
            store: store.to_owned(),
            library: LibraryInput::Image(image),
            entry: Some(DefPath::from_full("app@1/main/main").unwrap()),
        })
    }
    /// Installed program libraries are the published root modules, not their contract-test corpus.
    pub fn program(dir: &Path, libraries: &Path, store: &Path) -> io::Result<Self> {
        if product::normalized(libraries)
            != product::normalized(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core"))
            && product::normalized(libraries) != product::normalized(Path::new("core"))
        {
            return Self::directory(dir, libraries, store);
        }
        let mut app = PackageManifest::directory(dir, package("app"), false)?;
        app.dependencies.extend([package("core"), package("std")]);
        Ok(Self {
            inputs: CorpusManifest {
                core: package("core"),
                packages: vec![
                    PackageProduct::bundled_manifest(&libraries.join("core")),
                    PackageProduct::bundled_std_manifest(&libraries.join("std")),
                    app,
                ],
            },
            store: store.to_owned(),
            library: LibraryInput::Sources,
            entry: Some(DefPath::from_full("app@1/main/main").unwrap()),
        })
    }
    pub fn directory(dir: &Path, libraries: &Path, store: &Path) -> io::Result<Self> {
        let core = if libraries == Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core") {
            PackageProduct::bundled_manifest(&libraries.join("core"))
        } else {
            PackageManifest::directory(&libraries.join("core"), package("core"), true)?
        };
        let std = Self::source_std(&libraries.join("std"))?;
        let mut app = PackageManifest::directory(dir, package("app"), false)?;
        app.dependencies.extend([package("core"), package("std")]);
        Ok(Self {
            inputs: CorpusManifest {
                core: package("core"),
                packages: vec![core, std, app],
            },
            store: store.to_owned(),
            library: LibraryInput::Sources,
            entry: Some(DefPath::from_full("app@1/main/main").unwrap()),
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
/// The implementation's resource refusal (PROG-6): a parser, resolver, layout or
/// instance-collection limit is exceeded. It names no rule and is neither a rule
/// refusal nor an unbuilt construct; the message names the amount and the limit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceRefusal {
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
    /// The body record's address: the content hash of this product as published.
    pub record: ContentHash,
}
#[derive(Clone)]
pub struct CheckProduct {
    pub index: Arc<NameIndex>,
    pub files: Vec<Arc<FileProduct>>,
    pub definitions: BTreeMap<DefPath, Arc<DefinitionProduct>>,
    pub diagnostics: Vec<Refusal>,
    pub incomplete: Vec<Incomplete>,
    pub resources: Vec<ResourceRefusal>,
    pub bytes: usize,
    pub rechecked: Vec<DefPath>,
    pub root: ContentHash,
    pub snapshot: ContentHash,
    pub image: Option<Arc<CoreImage>>,
    pub(crate) library: Option<Arc<hirsel_core_front::Library>>,
    native_entry: Option<native::EntryBinding>,
    test_entries: BTreeMap<DefPath, Result<native::EntryAbi, String>>,
    headers: Arc<reuse::Headers>,
}
impl CheckProduct {
    fn empty(index: Arc<NameIndex>) -> Self {
        Self {
            index,
            files: vec![],
            definitions: BTreeMap::new(),
            diagnostics: vec![],
            incomplete: vec![],
            resources: vec![],
            bytes: 0,
            rechecked: vec![],
            root: ContentHash::default(),
            snapshot: ContentHash::default(),
            image: None,
            library: None,
            native_entry: None,
            test_entries: BTreeMap::new(),
            headers: Arc::default(),
        }
    }
    pub fn accepted(&self) -> bool {
        self.diagnostics.is_empty() && self.incomplete.is_empty() && self.resources.is_empty()
    }
    pub fn demand_body(&self, definition: &DefPath) -> io::Result<Arc<DefinitionProduct>> {
        if let Some(body) = self.definitions.get(definition) {
            return Ok(Arc::clone(body));
        }
        if let Some(image) = &self.image {
            return image.demand(definition);
        }
        Err(io::Error::other(format!(
            "core body demand outside reached definitions: {definition}"
        )))
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

/// The module of a canonical record site: its item is an index into the
/// record's anchor list, so a record does not depend on module numbering or
/// item order at the time it was checked.
const ANCHORED: ModuleId = ModuleId(u32::MAX);

// Borrow the native result tables for output. No cloned IR, resolver scratch or text.
// Sites are canonical: each site with an anchor names it by its position in `anchors`.
#[derive(Serialize)]
struct DiskCheckRef<'a> {
    anchors: Vec<&'a DefPath>,
    key: &'a DefinitionKey,
    definition: &'a DefPath,
    refusals: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
    counts: [u64; 6],
    template: &'a Option<hirsel_core_front::GenericTemplate>,
    function: Option<u32>,
    ir: &'a Module,
    resolved: bool,
}
impl<'a> DiskCheckRef<'a> {
    /// A site is a byte offset from its item's start and the key covers only
    /// the body's own item, so a resolved body, whose refusals and gaps are
    /// reported from its record, must cite no other item. An unresolved body's
    /// refusals are the resolver's, reported afresh by every request.
    fn of(d: &'a DefinitionProduct) -> io::Result<Self> {
        let c = d.check.counts;
        let mut anchors: Vec<&DefPath> = d.anchors.values().collect();
        anchors.sort_unstable();
        anchors.dedup();
        if d.resolved
            && let Some(other) = anchors.iter().find(|p| **p != &d.definition)
        {
            return Err(io::Error::other(format!(
                "internal error: the checked body `{}` cites a site in `{other}`",
                d.definition
            )));
        }
        let canonical = |site: &mut Site| {
            if let Some(path) = d.anchors.get(&(site.module, site.item)) {
                site.module = ANCHORED;
                site.item = anchors.binary_search(&path).unwrap() as u32;
            }
        };
        let mut refusals = d.check.refusals.clone();
        for r in &mut refusals {
            canonical(&mut r.primary);
            r.causal.iter_mut().for_each(canonical);
        }
        let mut incomplete = d.incomplete.clone();
        for g in &mut incomplete {
            canonical(&mut g.primary);
        }
        Ok(Self {
            anchors,
            key: &d.key,
            definition: &d.definition,
            refusals,
            incomplete,
            counts: [c.nodes, c.uses, c.places, c.calls, c.types, c.bytes],
            template: &d.check.template,
            function: d.check.function.map(|f| f.0),
            ir: &d.ir,
            resolved: d.resolved,
        })
    }
}
#[derive(Deserialize)]
struct DiskCheck {
    anchors: Vec<DefPath>,
    key: DefinitionKey,
    definition: DefPath,
    refusals: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
    counts: [u64; 6],
    template: Option<hirsel_core_front::GenericTemplate>,
    function: Option<u32>,
    ir: Module,
    resolved: bool,
}
impl Driver {
    pub fn check(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        hirsel_core_ids::published::catch_io(|| self.check_inner(request, meter))
    }
    fn check_inner(
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
                let bytes = {
                    hirsel_core_front::cost_scope!("non-check", "source file reads");
                    std::fs::read(&s.file)?
                };
                let hash = {
                    hirsel_core_front::cost_scope!("non-check", "source byte hashing");
                    ContentHash::of_bytes(&bytes)
                };
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
        meter.finish("load/source files and hashes");
        let compiler = compiler_identity()?;
        meter.finish("load/compiler identity");
        let image = match &request.library {
            LibraryInput::Image(i) | LibraryInput::ReachableImage(i) => Some(Arc::clone(i)),
            _ => None,
        };
        let produce = matches!(request.library, LibraryInput::Produce);
        let reachable_only = matches!(request.library, LibraryInput::ReachableImage(_));
        let source_stamps: Vec<_> = sources.iter().map(|s| s.4).collect();
        let snapshot = ContentHash::of_bytes(&encode(&(
            compiler,
            &request.inputs,
            source_stamps,
            image.as_ref().map(|i| i.identity),
            produce,
            reachable_only,
            &request.entry,
        ))?);
        let store = Store::new(&request.store)?;
        meter.finish("load/request identity and store");
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
        let prior = check_index::ProgramIndex::open(&store)?;
        let mut publication = check_index::Publication::new();
        meter.finish("load/program index");
        let image_directory = image.as_ref().map(|i| i.directory()).transpose()?;
        meter.finish("package index demand");

        let mut resolution_inputs = request.inputs.clone();
        if let Some(directory) = &image_directory {
            for package in &directory.inputs.packages {
                if !resolution_inputs
                    .packages
                    .iter()
                    .any(|p| p.package == package.package)
                {
                    let mut package = package.clone();
                    package.sources.clear();
                    resolution_inputs.packages.push(package);
                }
            }
        }
        let mut symbols = image_directory
            .as_ref()
            .map_or_else(Symbols::default, |i| i.index.symbols.as_ref().clone());
        for p in &request.inputs.packages {
            p.intern(&mut symbols);
        }
        let mut modules = vec![];
        let mut syntax_diagnostics = vec![];
        let mut syntax_resources = vec![];
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
                syntax_resources.push((path, r.site(), format!("parser resource refusal: {r}")));
            }
            modules.push((path, output.syntax));
        }
        meter.finish("parse");
        let symbols = Arc::new(symbols);
        let mut index = image_directory.as_ref().map_or_else(
            || NameIndex::new(Arc::clone(&symbols)),
            |i| i.index.as_ref().clone(),
        );
        index.symbols = symbols;
        index.add_manifests(&resolution_inputs.packages);
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
        result.image = image.clone();
        if let Some(directory) = &image_directory
            && !reachable_only
        {
            result
                .diagnostics
                .extend(directory.diagnostics.iter().cloned());
            result
                .incomplete
                .extend(directory.incomplete.iter().cloned());
            result.resources.extend(directory.resources.iter().cloned());
        }
        result.snapshot = snapshot;
        for (path, d) in syntax_diagnostics {
            result
                .diagnostics
                .push(Refusal::syntax(index.module(path).unwrap(), &d));
        }
        for (path, span, message) in syntax_resources {
            result.resources.push(ResourceRefusal {
                primary: Site {
                    module: index.module(path).unwrap(),
                    item: u32::MAX,
                    span,
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
                if let Some((site, depth)) = &row.resource_refusal {
                    result.resources.push(ResourceRefusal {
                        primary: *site,
                        message: format!(
                            "resolver resource refusal: expression nesting reaches depth {} past the limit of {depth}",
                            depth + 1
                        ),
                    });
                }
            }
        }
        let reads_library = image_directory.as_ref().is_some_and(|image| {
            modules.iter().zip(&resolutions).any(|(m, rows)| {
                // Region/limit scopes and string carriers name core types
                // implicitly; they need the same published declarations.
                m.syntax.tree.nodes.iter().any(|n| {
                    matches!(
                        n.kind,
                        hirsel_core_syntax::NodeKind::Region | hirsel_core_syntax::NodeKind::Bound
                    ) || n.token.is_some_and(|t| {
                        matches!(
                            m.syntax.tree.tokens[t.0 as usize].kind,
                            hirsel_core_syntax::TokenKind::String
                                | hirsel_core_syntax::TokenKind::Bytes
                        )
                    })
                }) || rows.iter().any(|r| {
                    r.names.iter().any(|n| match n.target {
                        hirsel_core_resolve::Target::Definition(d) => {
                            (d.0 as usize) < image.index.definitions.len()
                        }
                        hirsel_core_resolve::Target::ContextualConstructor { name } => index
                            .constructor_candidates(m.id, name)
                            .any(|d| (d.0 as usize) < image.index.definitions.len()),
                        _ => false,
                    })
                })
            })
        });
        meter.finish("resolve");
        let library = if reads_library {
            image.as_ref().map(|i| i.declarations()).transpose()?
        } else {
            None
        };
        meter.finish("package declarations demand");
        let mut keys = Keys::new(
            &index,
            &modules,
            &resolutions,
            &resolution_inputs,
            compiler_identity()?,
            image_directory.as_ref().map(|i| &*i.headers),
            image_directory.as_ref().map(|i| &i.anchors),
        )?;
        {
            let mut p = Program::declare_with_library(
                &index,
                &modules,
                resolutions.clone(),
                library.as_deref(),
            );
            p.entry = request.entry.clone();
            result.diagnostics.extend(p.refusals.iter().cloned());
            result.incomplete.extend(p.gaps.iter().map(|g| Incomplete {
                primary: g.site,
                message: g.what.into(),
            }));
            result
                .resources
                .extend(p.resource_refusals.iter().map(|r| ResourceRefusal {
                    primary: r.site,
                    message: r.message.clone(),
                }));
            meter.finish("declarations/layout");
            let declarations_accepted = result.accepted();
            let mut fresh: Vec<(DefPath, ContentHash)> = Vec::new();
            for f in p.fns.base_len()..p.fns.len() {
                let fd = &p.fns[f];
                if fd.body.is_none() {
                    continue;
                }
                let resolution =
                    Arc::clone(&resolutions[fd.at.module as usize][fd.at.item as usize]);
                let item = modules[fd.at.module as usize]
                    .syntax
                    .item(fd.at.item as usize);
                let mut key = keys.for_body(
                    &index,
                    &fd.path,
                    modules[fd.at.module as usize].id,
                    fd.at.item,
                    item,
                    &resolution,
                )?;
                // Entry policy is a read of the selected body only. Keep it
                // out of the shared environment key so another program does
                // not recheck the library closure, while separating the
                // entry body when its waits rule changes.
                if request.entry.as_ref() == Some(&fd.path) {
                    key.environment = ContentHash::of_parts(
                        "hirsel.entry-policy.v1",
                        [&encode(&request.entry)?[..], &key.environment.0],
                    );
                }
                let key_hash = check_index::key_hash(&key)?;
                // A resident product's record lives in the store it was published to.
                let resident = self
                    .published
                    .as_ref()
                    .filter(|_| same_store)
                    .and_then(|p| p.definitions.get(&fd.path))
                    .filter(|d| d.key == key)
                    .cloned();
                let cached = if resident.is_some() {
                    resident
                } else {
                    prior_definition(&store, &prior, &fd.path, &key, key_hash)?.map(Arc::new)
                };
                let product = if let Some(product) = cached {
                    publication.rows.insert(
                        fd.path.clone(),
                        check_index::Row::Live {
                            key: key_hash,
                            body: product.record,
                        },
                    );
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
                    emitter.seal();
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
                    let product = DefinitionProduct {
                        definition: fd.path.clone(),
                        check,
                        ir: emitter.module,
                        resolved: resolution.diagnostics.is_empty(),
                        key,
                        anchors,
                        incomplete,
                        record: ContentHash::default(),
                    };
                    // Framed below, as publication work, once checking is done.
                    fresh.push((fd.path.clone(), key_hash));
                    Arc::new(product)
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
            meter.finish("check/lower");
            for (path, key_hash) in fresh {
                let product = result
                    .definitions
                    .get_mut(&path)
                    .and_then(Arc::get_mut)
                    .ok_or_else(|| {
                        io::Error::other(format!(
                            "internal error: the checked body `{path}` is shared before its record is framed"
                        ))
                    })?;
                let (record, bytes) = Store::frame(&encode(&DiskCheckRef::of(product)?)?);
                product.record = record;
                publication.bodies.push((record, bytes));
                publication.rows.insert(
                    path,
                    check_index::Row::Live {
                        key: key_hash,
                        body: record,
                    },
                );
            }
            meter.finish("publish/body records");
            result.native_entry = request
                .entry
                .as_ref()
                .map(|e| native::bind_entry(&p, e, result.definitions.get(e).map(Arc::as_ref)));
            for f in p.fns.base_len()..p.fns.len() {
                let fd = &p.fns[f];
                if fd.is_test {
                    let (path, abi) = native::bind_entry(
                        &p,
                        &fd.path,
                        result.definitions.get(&fd.path).map(Arc::as_ref),
                    );
                    result.test_entries.insert(path, abi);
                }
            }
            if produce && declarations_accepted {
                result.library = Some(Arc::new(p.into_library()));
            } else {
                drop(p);
            }
        }
        meter.finish("check/lower");
        let mut names: BTreeMap<ModuleId, Vec<(DefinitionId, Definition)>> = BTreeMap::new();
        let base = index.definitions.base_len();
        for (i, d) in index.definitions.own_iter().enumerate() {
            names
                .entry(d.site.module)
                .or_default()
                .push((DefinitionId((base + i) as u32), *d));
        }
        for (m, (path, file, _, bytes, source)) in modules.into_iter().zip(sources) {
            let rows = names.remove(&m.id).unwrap_or_default();
            publication.files.insert(
                (
                    path.package.name.clone(),
                    path.package.version.clone(),
                    path.modules.clone(),
                ),
                file_record(&index, &m, &bytes, source, &rows),
            );
            let module_names = rows.into_iter().map(|(_, d)| d).collect();
            result.files.push(Arc::new(FileProduct {
                module: m.id,
                path: file,
                source,
                syntax: m.syntax,
                names: module_names,
            }));
        }
        meter.finish("publish/assemble products");
        result.headers = Arc::new(std::mem::take(&mut keys.headers));
        result.root = publication.publish(&store, &prior)?;
        meter.finish("publish/program index");
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
    hirsel_core_front::cost_scope!("encoding", "canonical key encoding");
    let mut bytes = Vec::with_capacity(64);
    bincode::serialize_into(&mut bytes, value).map_err(io::Error::other)?;
    Ok(bytes)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()
}
/// A body record of the prior publication, when its live row and its
/// content both carry the current key. Anything else is a miss.
fn prior_definition(
    store: &Store,
    prior: &check_index::ProgramIndex,
    path: &DefPath,
    key: &DefinitionKey,
    key_hash: ContentHash,
) -> io::Result<Option<DefinitionProduct>> {
    hirsel_core_front::cost_scope!("non-check", "prior body record read");
    let Some((row_key, record)) = prior.live(path) else {
        return Ok(None);
    };
    if row_key != key_hash {
        return Ok(None);
    }
    let Some(disk) = store.get(record)?.and_then(|b| decode::<DiskCheck>(&b)) else {
        return Ok(None);
    };
    Ok((disk.definition == *path && disk.key == *key).then(|| from_disk(disk, record)))
}
/// The file record: namespace map and anchor map (item ordinal, line base).
fn file_record(
    index: &NameIndex,
    module: &ModuleInput,
    bytes: &[u8],
    source: ContentHash,
    definitions: &[(DefinitionId, Definition)],
) -> check_index::FileRecord {
    let path = index.module_path(module.id);
    let mut record = check_index::FileRecord {
        module: (path.package.name, path.package.version, path.modules),
        source,
        names: BTreeMap::new(),
        anchors: BTreeMap::new(),
    };
    let mut line = 0u32;
    let mut at = 0usize;
    let mut starts = BTreeMap::new();
    for (i, item) in module.syntax.items().enumerate() {
        let offset = (item.file_offset as usize).clamp(at, bytes.len());
        line += bytes[at..offset].iter().filter(|&&b| b == b'\n').count() as u32;
        at = offset;
        starts.insert(i as u32, line);
    }
    for &(id, d) in definitions {
        let def = index.definition_path(id);
        if let Some(&line) = starts.get(&d.site.item) {
            record.anchors.insert(d.site.item, (def.clone(), line));
        }
        record.names.insert(def.0.name.clone(), def);
    }
    record
}
fn from_disk(disk: DiskCheck, record: ContentHash) -> DefinitionProduct {
    let [nodes, uses, places, calls, types, bytes] = disk.counts;
    DefinitionProduct {
        record,
        definition: disk.definition,
        anchors: (0u32..)
            .zip(disk.anchors)
            .map(|(i, path)| ((ANCHORED, i), path))
            .collect(),
        key: disk.key,
        incomplete: disk.incomplete,
        resolved: disk.resolved,
        ir: disk.ir,
        check: CoreCheck {
            refusals: disk.refusals,
            gaps: vec![],
            function: disk.function.map(hirsel_core_ir::FuncId),
            template: disk.template,
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

#[cfg(test)]
#[path = "../../hirsel-core-rt/src/openat2_required.rs"]
mod openat2_required;

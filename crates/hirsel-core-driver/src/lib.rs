//! One core-only request path from an immutable source snapshot to published products.
//! Sources and signatures are explicit inputs; no old compiler or std image is loaded.
pub mod call;
pub mod cli;
pub mod cli_tools;
pub mod code_admission;
pub mod code_store;
pub mod describe;
mod diagnostics;
mod entry_show;
mod exec_launch;
mod foreign_archive;
mod foreign_header;
mod host;
mod image;
mod image_index;
mod image_unit;
mod inspect;
pub mod installation;
pub(crate) mod launcher;
mod product;
mod property_corpus;
pub mod sdk;
pub mod sdk_command;
mod tape_cli;
pub use product::PackageProduct;
mod callable_variants;
#[cfg(test)]
mod carried_law;
mod check_index;
mod executable;
mod flow;
mod instances;
mod native;
pub mod native_debug;
mod native_entry;
mod native_facts;
mod native_object;
mod native_sources;
mod package_content;
mod package_dir;
mod package_files;
mod package_lock;
mod package_manifest;
mod package_resolve;
mod package_store;
mod package_workspace;
mod prebuild;
mod prof_command;
mod release_manifest;
pub use prebuild::Prebuilt;
mod core_inventory;
mod layers;
mod link_closure;
mod reach;
mod reachability;
pub mod request;
mod request_record;
mod test_run;
mod tool_inputs;
pub use core_inventory::{allowlist, core_declarations, core_inventory};
pub use native::recipe_build::{AssemblyRecord, Publication};
pub use native::{
    Backend, BuildProduct, Link, NativeOptions, NativeStop, ObjectProduct, ReplayPlan, grant_kind,
};
pub use reach::ReachCounters;
pub use reachability::{GapStop, Reachable};
pub use test_run::{Runner, Source, TestCache, TestOutcome};
mod meter;
pub use diagnostics::{
    Applicability, AvailableMember, Diagnostic, DiagnosticCause, Edit, Fix, IncompleteDiagnostic,
    ResourceDiagnostic, SourceFiles, SourceSite, line_column,
};
pub use image::{CoreImage, LibraryInput};
pub use instances::{InstanceStop, Units, collect_instances};
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
    Refusal, Resolution, ResolutionScratch, Site, SourceId,
};
use hirsel_core_syntax::{Symbols, SyntaxFile};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod closure_laws;
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
    /// The directory of the explicit manifest `inputs` was read from. A
    /// diagnostic and a native site name a file of the request by its path
    /// relative to it, so two files of one module have two names. Without a
    /// manifest a file is named by its module, as the directory convention
    /// gives each file a module of its own.
    pub manifest: Option<PathBuf>,
}
impl CheckRequest {
    pub(crate) fn capture_sources(&mut self) -> io::Result<()> {
        for package in &mut self.inputs.packages {
            for source in &mut package.sources {
                if source.captured.is_none() {
                    source.captured = Some(hirsel_core_resolve::CapturedSource {
                        bytes: source.read()?,
                        name: source.name(self.manifest.as_deref())?,
                        test: false,
                    });
                }
            }
        }
        Ok(())
    }

    fn source_std(root: &Path) -> io::Result<PackageManifest> {
        let mut manifest = PackageManifest::directory(root, package("std"), true)?;
        manifest.dependencies.push(package("core"));
        Ok(manifest)
    }

    fn library_packages(
        libraries: Option<&Path>,
        needs_std: bool,
    ) -> io::Result<Vec<PackageManifest>> {
        let core = match libraries {
            Some(root) => PackageManifest::directory(&root.join("core"), package("core"), true)?,
            None => PackageProduct::bundled_manifest(Path::new("core/core")),
        };
        let mut packages = vec![core];
        if needs_std {
            packages.push(match libraries {
                Some(root) => Self::source_std(&root.join("std"))?,
                None => PackageProduct::bundled_std_manifest(Path::new("core/std")),
            });
        }
        Ok(packages)
    }

    pub fn libraries(libraries: &Path, store: &Path) -> io::Result<Self> {
        Self::library_request(Some(libraries), store)
    }

    pub(crate) fn library_request(libraries: Option<&Path>, store: &Path) -> io::Result<Self> {
        Ok(Self {
            manifest: None,
            inputs: CorpusManifest {
                core: package("core"),
                packages: Self::library_packages(libraries, true)?,
            },
            store: store.to_owned(),
            library: LibraryInput::Sources,
            entry: None,
        })
    }
    pub fn with_image(dir: &Path, store: &Path, image: Arc<CoreImage>) -> io::Result<Self> {
        let mut request = Self::from_app(package_dir::PackageDir::open(dir)?, None, store, true)?;
        if !request.use_image(image)? {
            return Err(io::Error::other(
                "the image holds packages outside the request",
            ));
        }
        Ok(request)
    }

    pub fn program(dir: &Path, libraries: &Path, store: &Path) -> io::Result<Self> {
        Self::from_app(
            package_dir::PackageDir::open(dir)?,
            Some(libraries),
            store,
            true,
        )
    }

    pub fn directory(dir: &Path, libraries: &Path, store: &Path) -> io::Result<Self> {
        Self::from_app(
            package_dir::PackageDir::open(dir)?,
            Some(libraries),
            store,
            false,
        )
    }

    pub(crate) fn from_app(
        app: package_dir::PackageDir,
        libraries: Option<&Path>,
        store: &Path,
        program: bool,
    ) -> io::Result<Self> {
        let libraries = libraries.filter(|root| {
            !program || product::normalized(root) != product::normalized(Path::new("core"))
        });
        let mut packages = Self::library_packages(libraries, app.needs_std())?;
        packages.extend(app.dependencies);
        packages.push(app.package);
        Ok(Self {
            manifest: app.manifest,
            inputs: CorpusManifest {
                core: package("core"),
                packages,
            },
            store: store.to_owned(),
            library: LibraryInput::Sources,
            entry: Some(app.entry),
        })
    }

    pub(crate) fn use_image(&mut self, image: Arc<CoreImage>) -> io::Result<bool> {
        self.capture_sources()?;
        self.inputs.normalise(self.manifest.as_deref());
        let directory = image.directory()?;
        let supplied = &directory.inputs.packages;
        for in_image in supplied {
            let Some(requested) = self
                .inputs
                .packages
                .iter()
                .find(|p| p.package == in_image.package)
            else {
                return Ok(false);
            };
            if in_image.dependencies != requested.dependencies
                || directory
                    .stamps
                    .get(&in_image.package)
                    .map(|stamp| stamp.hash)
                    != Some(
                        image::stamp(requested, self.manifest.as_deref(), self.entry.as_ref())?
                            .hash,
                    )
            {
                return Ok(false);
            }
        }
        self.inputs.packages.retain_mut(|package| {
            if !supplied.iter().any(|row| row.package == package.package) {
                return true;
            }
            // Keep this request's native archive captures for assembly.
            package.sources.clear();
            package.namespace.is_some()
        });
        self.library = LibraryInput::Image(image);
        Ok(true)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileProduct {
    pub module: ModuleId,
    /// This file's occurrence in the index: sites in it name it.
    pub file: SourceId,
    /// Its path relative to the request's manifest, when it has one.
    pub name: Option<String>,
    pub source: ContentHash,
    pub syntax: SyntaxFile,
    pub names: Vec<Definition>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Incomplete {
    pub primary: Site,
    pub causal: Vec<Site>,
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

pub use body::DefinitionProduct;
mod body {
    use super::*;
    /// A body and its result are kept together: CoreIR ids refer only to this module.
    /// Callee rows are signature-only imports, so a callee-body edit does not alter it.
    ///
    /// A product read back from its record holds only its head; its IR is read
    /// through `get_body` on the first [`DefinitionProduct::ir`] demand.
    pub struct DefinitionProduct {
        pub definition: DefPath,
        pub check: CoreCheck,
        ir: std::sync::OnceLock<Module>,
        /// Where a record-backed product's IR is read from on demand.
        ir_source: Option<IrSource>,
        /// The hash of `ir`'s serialization: its body record's module part,
        /// set when the record is framed.
        ir_hash: ContentHash,
        pub key: DefinitionKey,
        pub anchors: BTreeMap<ItemAt, DefPath>,
        pub incomplete: Vec<Incomplete>,
        /// The body record's address: the content hash of this product as published.
        pub record: ContentHash,
    }
    /// The store a record-backed product's IR is read from, and for an image
    /// body the reader its published descriptors decode through.
    pub(crate) struct IrSource {
        pub store: Store,
        pub reader: Option<Arc<hirsel_core_ids::published::Reader>>,
    }
    /// The hash a body record's module part would have, for a module no
    /// record carries: an instance's materialized body, serialized into
    /// `scratch`, which a caller reuses across modules. A stream into the
    /// hasher costs a dispatched write per encoded field.
    pub(crate) fn ir_hash(module: &Module, scratch: &mut Vec<u8>) -> io::Result<ContentHash> {
        scratch.clear();
        bincode::serialize_into(&mut *scratch, module).map_err(io::Error::other)?;
        Ok(Store::ir_hash(scratch))
    }
    impl DefinitionProduct {
        /// A freshly checked body. Its record is framed at publication
        /// (`frame`), which sets `ir_hash` and `record`.
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn fresh(
            definition: DefPath,
            check: CoreCheck,
            ir: Module,
            key: DefinitionKey,
            anchors: BTreeMap<ItemAt, DefPath>,
            incomplete: Vec<Incomplete>,
        ) -> Self {
            Self {
                definition,
                check,
                ir: ir.into(),
                ir_source: None,
                ir_hash: ContentHash::default(),
                key,
                anchors,
                incomplete,
                record: ContentHash::default(),
            }
        }
        /// The body record (`DiskCheck`): its head, then the serialization of
        /// the module the emitter has, encoded once into the envelope. The
        /// module part's hash is `ir_hash`.
        pub(crate) fn frame(&mut self) -> io::Result<Vec<u8>> {
            let head = encode(&DiskCheckRef::of(self)?)?;
            let ir = self.ir.get().expect("a fresh product holds its IR");
            thread_local! {
                static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
            }
            let (record, ir_hash, bytes) = SCRATCH.with_borrow_mut(|scratch| {
                scratch.clear();
                {
                    hirsel_core_front::cost_scope!("encoding", "canonical key encoding");
                    bincode::serialize_into(&mut *scratch, ir).map_err(io::Error::other)?;
                }
                Ok::<_, io::Error>(Store::frame_body(&head, scratch))
            })?;
            self.record = record;
            self.ir_hash = ir_hash;
            Ok(bytes)
        }
        /// A published body record's head read back under its address
        /// (`Store::get_body_head`). Its IR is read from `source` on demand.
        pub(crate) fn from_head(
            head: &crate::store::Head,
            record: ContentHash,
            decode: impl FnOnce(&[u8]) -> Option<DiskCheck>,
            source: IrSource,
        ) -> Option<Self> {
            let disk = decode(head.head())?;
            let [nodes, uses, places, calls, types, bytes] = disk.counts;
            Some(Self {
                record,
                definition: disk.definition,
                anchors: (0u32..)
                    .zip(disk.anchors)
                    .map(|(i, path)| ((ANCHORED, i), path))
                    .collect(),
                key: disk.key,
                incomplete: disk.incomplete,
                ir_hash: head.ir_hash,
                ir: std::sync::OnceLock::new(),
                ir_source: Some(source),
                check: CoreCheck {
                    refusals: disk.refusals,
                    gaps: vec![],
                    function: disk.function.map(hirsel_core_ir::FuncId),
                    template: disk.template,
                    flow: disk.flow,
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
        /// The body's module. A record-backed product reads it once, through
        /// `get_body`, the only IR reader. A record whose IR fails its check
        /// behind the head this product was read from is removed, so the next
        /// request misses it and checks the body again (a library image's needs
        /// a rebuild); this request refuses with the record's path.
        pub fn ir(&self) -> io::Result<&Module> {
            if let Some(ir) = self.ir.get() {
                return Ok(ir);
            }
            let source = self
                .ir_source
                .as_ref()
                .expect("a product without its IR has a record");
            let ir = source
                .store
                .get_body(self.record)?
                .filter(|body| body.ir_hash == self.ir_hash)
                .and_then(|body| match &source.reader {
                    Some(reader) => reader.decode(|| decode::<Module>(body.ir())),
                    None => decode::<Module>(body.ir()),
                });
            let Some(ir) = ir else {
                source.store.remove_damaged(self.record)?;
                let next = match source.reader {
                    None => "the next request checks the body again",
                    Some(_) => "the library image needs a rebuild",
                };
                return Err(io::Error::other(format!(
                    "the body record {} of `{}` is damaged behind its head; it is removed, and {next}",
                    source.store.path(self.record).display(),
                    self.definition
                )));
            };
            Ok(self.ir.get_or_init(|| ir))
        }
        /// The hash of `ir`'s serialization, as its record's module part.
        pub fn ir_hash(&self) -> ContentHash {
            self.ir_hash
        }
    }
}
#[derive(Clone)]
pub struct CheckProduct {
    pub index: Arc<NameIndex>,
    pub files: Vec<Arc<FileProduct>>,
    unparsed_sources: Vec<(SourceId, Arc<[u8]>)>,
    pub definitions: BTreeMap<DefPath, Arc<DefinitionProduct>>,
    pub diagnostics: Vec<Refusal>,
    pub incomplete: Vec<Incomplete>,
    pub resources: Vec<ResourceRefusal>,
    pub bytes: usize,
    /// Definition paths whose body products were recomputed by this request.
    pub rechecked: Vec<DefPath>,
    pub root: ContentHash,
    pub snapshot: ContentHash,
    pub(crate) environment: ContentHash,
    pub(crate) foreign_inputs:
        BTreeMap<PackageId, BTreeMap<Vec<String>, hirsel_core_resolve::ForeignModuleCapture>>,
    pub(crate) foreign_headers: BTreeMap<DefPath, (DefPath, String)>,
    pub image: Option<Arc<CoreImage>>,
    pub(crate) library: Option<Arc<hirsel_core_front::Library>>,
    native_entry: Option<native::EntryBinding>,
    test_entries: BTreeMap<DefPath, Result<native::EntryDecl, String>>,
    headers: Arc<reuse::Headers>,
}
impl CheckProduct {
    fn empty(index: Arc<NameIndex>) -> Self {
        Self {
            index,
            files: vec![],
            unparsed_sources: vec![],
            definitions: BTreeMap::new(),
            diagnostics: vec![],
            incomplete: vec![],
            resources: vec![],
            bytes: 0,
            rechecked: vec![],
            root: ContentHash::default(),
            snapshot: ContentHash::default(),
            environment: ContentHash::default(),
            foreign_inputs: BTreeMap::new(),
            foreign_headers: BTreeMap::new(),
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
    /// Definitions in the request's sources, including those with reused check results.
    pub(crate) fn checked_definitions(&self) -> usize {
        self.index.definitions.len() - self.index.definitions.base_len()
    }
    /// The identities of the rechecked bodies, independent of traversal order.
    pub fn rechecked_paths(&self) -> BTreeSet<&DefPath> {
        self.rechecked.iter().collect()
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
    #[cfg(test)]
    pub(crate) layers: layers::Probe,
    /// Each body builds every row it names, as `closure_laws` compares copies with.
    #[cfg(test)]
    pub(crate) fresh_rows: bool,
}

/// An item in one source file: the file's occurrence and the item's ordinal
/// in it. A site's anchor key; two files of one module never share one.
pub(crate) type ItemAt = (SourceId, u32);
pub(crate) fn item_at(site: &Site) -> Option<ItemAt> {
    site.source.map(|source| (source, site.item))
}
/// Point `site` at the item `anchor` (module, file, ordinal) names.
pub(crate) fn bind(site: &mut Site, anchor: (ModuleId, SourceId, u32)) {
    site.module = anchor.0;
    site.source = Some(anchor.1);
    site.item = anchor.2;
}
/// The module and file of a canonical record site: its item is an index into
/// the record's anchor list, so a record does not depend on module or file
/// numbering or item order at the time it was checked.
const ANCHORED_MODULE: ModuleId = ModuleId(u32::MAX);
const ANCHORED: SourceId = SourceId(u32::MAX);

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
    flow: hirsel_core_front::FlowFacts,
    function: Option<u32>,
}
impl<'a> DiskCheckRef<'a> {
    /// A site is a byte offset from its item's start and the key covers only
    /// the body's own item, so a body, whose refusals and gaps are reported
    /// from its record, must cite no other item.
    fn of(d: &'a DefinitionProduct) -> io::Result<Self> {
        let c = d.check.counts;
        let mut anchors: Vec<&DefPath> = d.anchors.values().collect();
        anchors.sort_unstable();
        anchors.dedup();
        if let Some(other) = anchors.iter().find(|p| **p != &d.definition) {
            return Err(io::Error::other(format!(
                "internal error: the checked body `{}` cites a site in `{other}`",
                d.definition
            )));
        }
        let canonical = |site: &mut Site| {
            if let Some(path) = item_at(site).and_then(|at| d.anchors.get(&at)) {
                site.module = ANCHORED_MODULE;
                site.source = Some(ANCHORED);
                site.item = anchors.binary_search(&path).unwrap() as u32;
            }
        };
        let mut refusals = d.check.refusals.clone();
        for r in &mut refusals {
            canonical(&mut r.primary);
            r.causal.iter_mut().for_each(|c| canonical(&mut c.site));
        }
        let mut incomplete = d.incomplete.clone();
        for g in &mut incomplete {
            canonical(&mut g.primary);
            g.causal.iter_mut().for_each(canonical);
        }
        let mut flow = d.check.flow.clone();
        for (site, _) in &mut flow.stages {
            canonical(site);
        }
        Ok(Self {
            anchors,
            key: &d.key,
            definition: &d.definition,
            refusals,
            incomplete,
            counts: [c.nodes, c.uses, c.places, c.calls, c.types, c.bytes],
            template: &d.check.template,
            flow,
            function: d.check.function.map(|f| f.0),
        })
    }
}
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct DiskCheck {
    anchors: Vec<DefPath>,
    key: DefinitionKey,
    definition: DefPath,
    refusals: Vec<Refusal>,
    incomplete: Vec<Incomplete>,
    counts: [u64; 6],
    template: Option<hirsel_core_front::GenericTemplate>,
    flow: hirsel_core_front::FlowFacts,
    function: Option<u32>,
}
impl Driver {
    pub fn check(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        self.check_recorded(request, meter, None)?.product()
    }
    /// Check `request`, answered from its request record when `record` names
    /// one that holds its answer.
    pub(crate) fn check_recorded(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
        record: Option<&request_record::Request>,
    ) -> io::Result<Checked> {
        if meter.enabled() {
            hirsel_core_ids::published::catch_io(|| {
                self.check_inner::<true>(request, meter, record)
            })
        } else {
            hirsel_core_ids::published::catch_io(|| {
                self.check_inner::<false>(request, meter, record)
            })
        }
    }
    fn check_inner<const MEASURE: bool>(
        &mut self,
        mut request: CheckRequest,
        meter: &mut Meter,
        record: Option<&request_record::Request>,
    ) -> io::Result<Checked> {
        request.inputs.normalise(request.manifest.as_deref());
        #[cfg(test)]
        {
            self.layers.shape = None;
            self.layers.tables.clear();
        }
        // Read source bytes exactly once. The source digest is reused by file publication.
        let mut sources = vec![];
        let mut bytes_total = 0;
        for p in &request.inputs.packages {
            for s in &p.sources {
                let bytes = {
                    hirsel_core_front::cost_scope!("non-check", "source file reads");
                    crate::meter::count(crate::meter::Count::FilesOpened, 1);
                    s.read()?
                };
                let hash = {
                    hirsel_core_front::cost_scope!("non-check", "source byte hashing");
                    ContentHash::of_bytes(&bytes)
                };
                bytes_total += bytes.len();
                let name = s.name(request.manifest.as_deref())?;
                sources.push((
                    ModulePath {
                        package: p.package.clone(),
                        modules: s.modules.clone(),
                    },
                    s.mode(),
                    bytes,
                    hash,
                    name,
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
        let image_directory = image.as_ref().map(|i| i.directory()).transpose()?;

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
                    package.namespace = directory
                        .stamps
                        .get(&package.package)
                        .ok_or_else(|| io::Error::other("image package is missing its stamp"))?
                        .namespace
                        .clone();
                    resolution_inputs.packages.push(package);
                }
            }
        }
        let environment = reuse::environment(&resolution_inputs, compiler)?;
        let produce = matches!(request.library, LibraryInput::Produce);
        let reachable_only = matches!(request.library, LibraryInput::ReachableImage(_));
        // A file is read for its bytes alone: the snapshot names each source
        // by its module, digest and manifest-relative name, never by where
        // the checkout lives.
        let source_stamps: Vec<_> = sources.iter().map(|s| (&s.0, s.1, s.3, &s.4)).collect();
        let snapshot = ContentHash::of_bytes(&encode(&(
            environment,
            source_stamps,
            image.as_ref().map(|i| i.identity),
            produce,
            reachable_only,
            &request.entry,
        ))?);
        let store = Store::new(&request.store)?;
        meter.finish("load/request identity and store");
        let record = record.map(|record| (record, record.key(snapshot)));
        if let Some((record, key)) = record {
            let names = || {
                sources
                    .iter()
                    .map(|s| request_record::file_name(s.4.as_deref(), &s.0))
                    .collect()
            };
            match request_record::lookup(&store, key, record, names)? {
                Ok(hit) => {
                    meter.finish("request record/hit");
                    return Ok(Checked::Hit(hit));
                }
                Err(miss) => meter.finish(miss.stage()),
            }
        }
        let prior = check_index::ProgramIndex::open(&store)?;
        let mut publication = check_index::Publication::new();
        meter.finish("load/program index");
        meter.finish("package index demand");
        let mut symbols = image_directory
            .as_ref()
            .map_or_else(Symbols::default, |i| i.index.symbols.as_ref().clone());
        for p in &request.inputs.packages {
            p.intern(&mut symbols);
        }
        let mut modules = Vec::with_capacity(sources.len());
        let mut syntax_diagnostics = vec![];
        let mut syntax_resources = vec![];
        for (file, (path, mode, bytes, _, _)) in sources.iter().enumerate() {
            let derive = request
                .inputs
                .packages
                .iter()
                .find(|p| p.package == path.package)
                .and_then(|p| {
                    hirsel_core_resolve::codec_derive(&p.package, &p.dependencies, &path.modules)
                });
            let output =
                hirsel_core_resolve::parse_module(bytes, *mode, &mut symbols, derive.as_ref());
            syntax_diagnostics.extend(output.diagnostics.into_iter().map(|d| (file, d)));
            if let Some(r) = output.resource_refusal {
                syntax_resources.push((file, r.site(), format!("parser resource refusal: {r}")));
            }
            modules.push(output.syntax);
        }
        meter.finish("parse");
        if MEASURE {
            meter.result.source_bytes += bytes_total as u64;
            meter.result.tokens += modules
                .iter()
                .map(|syntax| {
                    syntax
                        .tree
                        .tokens
                        .iter()
                        .filter(|token| {
                            !token.kind.is_trivia()
                                && token.kind != hirsel_core_syntax::TokenKind::Eof
                        })
                        .count() as u64
                })
                .sum::<u64>();
            meter.finish("front/input counts");
        }
        let symbols = Arc::new(symbols);
        let mut index = image_directory.as_ref().map_or_else(
            || NameIndex::new(Arc::clone(&symbols)),
            |i| i.index.as_ref().clone(),
        );
        index.symbols = symbols;
        // Both traversals follow the request's package and source order.
        // Manifests retained from the image have no sources in this request.
        let module_ids = index.add_manifests(&resolution_inputs.packages);
        let modules: Vec<_> = modules
            .into_iter()
            .zip(module_ids)
            .map(|(syntax, id)| ModuleInput {
                id,
                source: index.add_source(),
                syntax,
            })
            .collect();
        for m in &modules {
            for (i, item) in m.syntax.items().enumerate() {
                index.index_item(m.id, m.source, i as u32, item);
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
        result.environment = environment;
        result.foreign_inputs = request
            .inputs
            .packages
            .iter()
            .filter_map(|package| {
                let namespace = package.namespace.as_ref()?;
                (!namespace.foreign_inputs.is_empty())
                    .then(|| (package.package.clone(), namespace.foreign_inputs.clone()))
            })
            .collect();
        // Modules are the sources in order, one file each.
        for (file, d) in syntax_diagnostics {
            let m = &modules[file];
            result.diagnostics.push(Refusal::syntax(m.id, m.source, &d));
        }
        for (file, span, message) in syntax_resources {
            result.resources.push(ResourceRefusal {
                primary: Site {
                    module: modules[file].id,
                    source: Some(modules[file].source),
                    item: u32::MAX,
                    span,
                },
                message,
            });
        }
        meter.finish("index");
        // The gate (OPTION-L L2): the shape reads the environment and every
        // item header, never a resolution, so it is decided before resolve.
        // The store selects this shape's record of declared tables, or not.
        let shape = reuse::Shape::new(&index, &modules, environment)?;
        publication.shape = shape.key(&encode(&(
            &request.entry,
            image.as_ref().map(|i| i.identity),
            produce,
            reachable_only,
            sources.iter().map(|s| s.1).collect::<Vec<_>>(),
        ))?);
        let layer = layers::Layer::read(&store, publication.shape)?;
        meter.result.layer_class = Some(if layer.selected {
            meter::LayerClass::Fast
        } else {
            meter::LayerClass::Fallback
        });
        meter.finish("gate");
        let mut resolution_scratch = ResolutionScratch::default();
        let resolutions: Vec<Vec<Arc<Resolution>>> = modules
            .iter()
            .map(|m| {
                m.syntax
                    .items()
                    .enumerate()
                    .map(|(i, item)| {
                        Arc::new(resolution_scratch.resolve(&index, m.id, m.source, i as u32, item))
                    })
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
        meter.finish("resolve");
        // Declaration stands on the image's declarations whenever an image is
        // given, read or not, so the gate's shape holds every declaration
        // input (OPTION-L L2): a body edit cannot move the library base.
        let library = image.as_ref().map(|i| i.declarations()).transpose()?;
        meter.finish("package declarations demand");
        let mut keys = if MEASURE {
            meter.measure("body keys/new", || {
                Keys::new(
                    &index,
                    &modules,
                    &resolutions,
                    shape,
                    image.as_ref().map(|i| i.identity),
                    image_directory.as_ref().map(|i| &*i.headers),
                    image_directory.as_ref().map(|i| &i.anchors),
                )
            })?
        } else {
            Keys::new(
                &index,
                &modules,
                &resolutions,
                shape,
                image.as_ref().map(|i| i.identity),
                image_directory.as_ref().map(|i| &*i.headers),
                image_directory.as_ref().map(|i| &i.anchors),
            )?
        };
        // A body whose own source is unchanged under the prior shape keeps the
        // key its record holds: no closure walk and no lookup rows.
        let same_shape = prior.shape == Some(publication.shape);
        // The fast path: the store's record of this shape's declared tables
        // stands in for declaration. A missing, damaged or unbinding record
        // is damage, and the request declares as if no record were selected.
        let mut layer_record = None;
        {
            let installed = if layer.selected {
                store
                    .get(layer.key)?
                    .and_then(|bytes| decode::<hirsel_core_front::Declared>(&bytes))
                    .and_then(|record| {
                        Program::install(
                            &index,
                            &modules,
                            resolutions.clone(),
                            library.as_deref(),
                            record,
                        )
                    })
            } else {
                None
            };
            if installed.is_some() {
                meter::count(meter::Count::DeclaredInstalls, 1);
            }
            #[cfg(test)]
            {
                self.layers.shape = Some(publication.shape);
            }
            let mut p = installed.unwrap_or_else(|| {
                Program::declare_with_library(
                    &index,
                    &modules,
                    resolutions.clone(),
                    library.as_deref(),
                )
            });
            p.entry = request.entry.clone();
            #[cfg(test)]
            if self.fresh_rows {
                p.build_rows_fresh();
            }
            if !result.foreign_inputs.is_empty() {
                result.foreign_headers = p
                    .fns
                    .iter()
                    .filter_map(|function| {
                        p.foreign_function_header(function)
                            .map(|header| (function.path.clone(), header))
                    })
                    .collect();
            }
            #[cfg(test)]
            if self.layers.law {
                self.layers.tables = p.tables();
            }
            result.diagnostics.extend(p.refusals.iter().cloned());
            result.incomplete.extend(p.gaps.iter().map(|g| Incomplete {
                primary: g.site,
                causal: vec![],
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
            if layer.wanted() && declarations_accepted && !reachable_only {
                layer_record = p.declared().map(|d| encode(&d)).transpose()?;
            }
            let mut fresh: Vec<(DefPath, ContentHash, Option<ContentHash>)> = Vec::new();
            // A duplicate definition is a refusal of the declarations.
            let duplicated: BTreeSet<&DefPath> = if declarations_accepted {
                BTreeSet::new()
            } else {
                let mut seen = BTreeSet::new();
                (p.fns.base_len()..p.fns.len())
                    .map(|f| &p.fns[f].path)
                    .filter(|path| !seen.insert(*path))
                    .collect()
            };
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
                let module = &modules[fd.at.module as usize];
                let recorded = if same_shape && !duplicated.contains(&fd.path) {
                    let source = keys.body_source(&index, &fd.path, module, fd.at.item, item)?;
                    recorded_definition(&store, &prior, &fd.path)?
                        .filter(|product| product.key.body == source)
                } else {
                    None
                };
                let key = match &recorded {
                    Some(product) => product.key.clone(),
                    None => {
                        let mut key = if MEASURE {
                            meter.measure("body keys/for_body", || {
                                keys.for_body(
                                    &index,
                                    &fd.path,
                                    module,
                                    fd.at.item,
                                    item,
                                    &resolution,
                                )
                            })?
                        } else {
                            keys.for_body(&index, &fd.path, module, fd.at.item, item, &resolution)?
                        };
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
                        key
                    }
                };
                let key_hash = check_index::key_hash(&key)?;
                // The copies of a duplicate definition (LANG-7) share one
                // name, whose key hashes one copy's text: each is checked on
                // every request.
                // The key pointer, once a lookup has hashed it; publication
                // reuses it.
                let mut pointer = None;
                let cached = if duplicated.contains(&fd.path) {
                    None
                } else if let Some(product) = recorded {
                    Some(Arc::new(product))
                } else {
                    prior_definition(&store, &prior, &fd.path, &key, key_hash, &mut pointer)?
                        .map(Arc::new)
                };
                let mut checked = None;
                let product = if let Some(product) = cached {
                    publication.rows.insert(
                        fd.path.clone(),
                        check_index::Row {
                            key: key_hash,
                            body: product.record,
                        },
                    );
                    product
                } else {
                    let mut emitter = Emitter::new();
                    let check = if resolution.resource_refusal.is_some() {
                        CoreCheck::default()
                    } else {
                        p.check_lower(f, &mut emitter)
                    };
                    emitter.seal();
                    let incomplete: Vec<Incomplete> = check
                        .gaps
                        .iter()
                        .map(|g| Incomplete {
                            primary: g.site,
                            causal: vec![],
                            message: g.what.into(),
                        })
                        .collect();
                    let anchors = keys.capture_anchors(&check, &incomplete);
                    result.rechecked.push(fd.path.clone());
                    let product = DefinitionProduct::fresh(
                        fd.path.clone(),
                        check,
                        emitter.module,
                        key,
                        anchors,
                        incomplete,
                    );
                    checked = Some(key_hash);
                    Arc::new(product)
                };
                for r in &product.check.refusals {
                    let mut r = r.clone();
                    keys.bind_site(&product.anchors, &mut r.primary);
                    for cause in &mut r.causal {
                        keys.bind_site(&product.anchors, &mut cause.site);
                    }
                    result.diagnostics.push(r);
                }
                for g in &product.incomplete {
                    let mut g = g.clone();
                    keys.bind_site(&product.anchors, &mut g.primary);
                    for site in &mut g.causal {
                        keys.bind_site(&product.anchors, site);
                    }
                    result.incomplete.push(g);
                }
                // A duplicate definition's copies (LANG-7) share the path,
                // whose product is the last copy's.
                if result
                    .definitions
                    .insert(fd.path.clone(), product)
                    .is_some()
                {
                    fresh.retain(|(path, _, _)| *path != fd.path);
                }
                // Framed below, as publication work, once checking is done.
                if let Some(key_hash) = checked {
                    fresh.push((fd.path.clone(), key_hash, pointer));
                }
            }
            meter.finish("check/lower");
            if MEASURE {
                meter.finish_front();
            }
            for (path, key_hash, pointer) in fresh {
                let product = result
                    .definitions
                    .get_mut(&path)
                    .and_then(Arc::get_mut)
                    .ok_or_else(|| {
                        io::Error::other(format!(
                            "internal error: the checked body `{path}` is shared before its record is framed"
                        ))
                    })?;
                let bytes = product.frame()?;
                let record = product.record;
                publication.bodies.push((record, bytes));
                // The copies of a duplicate definition share a key; none is
                // found by it.
                if !duplicated.contains(&path) {
                    let pointer = match pointer {
                        Some(pointer) => pointer,
                        None => check_index::key_pointer(&path, key_hash)?,
                    };
                    publication.keyed.push((pointer, record));
                }
                publication.rows.insert(
                    path,
                    check_index::Row {
                        key: key_hash,
                        body: record,
                    },
                );
            }
            meter.finish("publish/body records");
            let diagnostics = result.flow_refusals(|definition, site| {
                keys.bind_site(&definition.anchors, site);
            });
            result.diagnostics.extend(diagnostics);
            // Check binds an entry's declaration part; its ABI is read from
            // the body's IR in the build.
            result.native_entry = request
                .entry
                .as_ref()
                .map(|e| native::bind_entry(&mut p, e));
            for f in p.fns.base_len()..p.fns.len() {
                if p.fns[f].is_test {
                    let path = p.fns[f].path.clone();
                    let (path, decl) = native::bind_entry(&mut p, &path);
                    result.test_entries.insert(path, decl);
                }
            }
            diagnostics::resolve_method_edits(&p, &mut result.diagnostics);
            if produce && declarations_accepted {
                result.library = Some(Arc::new(p.into_library()));
            } else {
                drop(p);
            }
        }
        meter.finish("check/lower");
        derived_sites(&mut result, &modules);
        let mut names: BTreeMap<Option<SourceId>, Vec<(DefinitionId, Definition)>> =
            BTreeMap::new();
        let base = index.definitions.base_len();
        for (i, d) in index.definitions.own_iter().enumerate() {
            names
                .entry(d.site.source)
                .or_default()
                .push((DefinitionId((base + i) as u32), *d));
        }
        for (m, (_, _, bytes, source, name)) in modules.into_iter().zip(sources) {
            let rows = names.remove(&Some(m.source)).unwrap_or_default();
            publication
                .files
                .push(file_record(&index, &m, &bytes, source, &rows));
            let file_names = rows.into_iter().map(|(_, d)| d).collect();
            if m.syntax.source.is_empty() && !bytes.is_empty() {
                result.unparsed_sources.push((m.source, bytes));
            }
            result.files.push(Arc::new(FileProduct {
                module: m.id,
                file: m.source,
                name,
                source,
                syntax: m.syntax,
                names: file_names,
            }));
        }
        meter.finish("publish/assemble products");
        result.headers = Arc::new(std::mem::take(&mut keys.headers));
        result.root = publication.publish(&store, &prior)?;
        meter.finish("publish/program index");
        if result.accepted() && !reachable_only {
            layer.finish(&store, layer_record.as_deref())?;
        }
        meter.finish("publish/declared layer");
        let result = Arc::new(result);
        meter.finish("publish/commit and Arc move");
        drop(index);
        drop(resolutions);
        drop(keys);
        drop(prior);
        drop(request);
        meter.finish("publish/scratch cleanup");
        Ok(Checked::Product(result, record.map(|(_, key)| key)))
    }
}
/// A request's check: its product with the key of its request record, or
/// the record's answer.
pub(crate) enum Checked {
    Product(Arc<CheckProduct>, Option<ContentHash>),
    Hit(request_record::Hit),
}
impl Checked {
    fn product(self) -> io::Result<Arc<CheckProduct>> {
        match self {
            Self::Product(product, _) => Ok(product),
            Self::Hit(_) => Err(io::Error::other(
                "internal error: a request record answered a request that names none",
            )),
        }
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
/// A body record checked under the current key: the one the prior
/// publication's live row names when the row carries the key, else the one
/// the key's pointer names, which an earlier request of another shape wrote.
/// The record's own path and key must match; anything else is a miss. Only
/// its head is read; its IR is read on demand. A key pointer hashed for the
/// lookup is left in `pointer`.
fn prior_definition(
    store: &Store,
    prior: &check_index::ProgramIndex,
    path: &DefPath,
    key: &DefinitionKey,
    key_hash: ContentHash,
    pointer: &mut Option<ContentHash>,
) -> io::Result<Option<DefinitionProduct>> {
    let record = match prior.live(path) {
        Some((row_key, record)) if row_key == key_hash => Some(record),
        _ => {
            let at = check_index::key_pointer(path, key_hash)?;
            *pointer = Some(at);
            check_index::keyed_record(store, at)?
        }
    };
    let Some(record) = record else {
        return Ok(None);
    };
    Ok(body_record(store, record, path)?.filter(|product| product.key == *key))
}
/// The body record a live row of the prior publication names, when it is
/// the record of `path`. Its key is the key under the prior shape.
fn recorded_definition(
    store: &Store,
    prior: &check_index::ProgramIndex,
    path: &DefPath,
) -> io::Result<Option<DefinitionProduct>> {
    let Some((_, record)) = prior.live(path) else {
        return Ok(None);
    };
    body_record(store, record, path)
}
/// The body record `record`, when it is the record of `path`.
fn body_record(
    store: &Store,
    record: ContentHash,
    path: &DefPath,
) -> io::Result<Option<DefinitionProduct>> {
    hirsel_core_front::cost_scope!("non-check", "prior body record read");
    let Some(head) = store.get_body_head(record)? else {
        return Ok(None);
    };
    let source = body::IrSource {
        store: store.clone(),
        reader: None,
    };
    Ok(DefinitionProduct::from_head(&head, record, decode, source)
        .filter(|product| product.definition == *path))
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
/// Publish codec failures through the same declaration/arguments/operation/member path,
/// retaining the underlying type declaration that made a field lack `codec`.
fn derived_sites(result: &mut CheckProduct, modules: &[ModuleInput]) {
    let context = |site: &Site| {
        let module = modules.iter().find(|m| Some(m.source) == site.source)?;
        hirsel_core_syntax::codec_context(&module.syntax, site.item as usize, site.span)
    };
    let generated = |site: &Site| {
        modules
            .iter()
            .find(|m| Some(m.source) == site.source)
            .and_then(|m| hirsel_core_syntax::declaration_of(&m.syntax, site.item as usize))
            .is_some()
    };
    let own: std::collections::HashSet<_> = result
        .diagnostics
        .iter()
        .filter(|r| !generated(&r.primary))
        .map(|r| (r.primary.source, r.primary.item))
        .collect();
    let project = |site: Site| {
        if generated(&site) {
            context(&site).map_or(site, |c| Site {
                item: c.item as u32,
                span: c.primary,
                ..site
            })
        } else {
            site
        }
    };
    let apply = |primary: &mut Site, ctx: &hirsel_core_syntax::DeriveContext| {
        primary.item = ctx.item as u32;
        primary.span = ctx.primary;
        let at = |span| Site { span, ..*primary };
        let owner = |span| Site {
            item: ctx.owner as u32,
            span,
            ..*primary
        };
        let mut path = vec![owner(ctx.declaration)];
        path.extend(ctx.arguments.iter().map(|&(item, span)| Site {
            item: item as u32,
            span,
            ..*primary
        }));
        path.push(owner(ctx.operation));
        path.extend(ctx.path.iter().copied().map(at));
        path
    };
    let mut seen = std::collections::HashSet::new();
    result.diagnostics.retain_mut(|r| {
        let is_generated = generated(&r.primary);
        if !is_generated
            && !(r.rule == "OWN-4"
                && !r.causal.is_empty()
                && r.required.as_deref().is_some_and(|b| b.contains("codec")))
        {
            return true;
        }
        let Some(ctx) = context(&r.primary) else {
            return true;
        };
        if is_generated && own.contains(&(r.primary.source, ctx.owner as u32)) {
            return false;
        }
        let path = apply(&mut r.primary, &ctx);
        let mut causal: Vec<_> = path
            .into_iter()
            .map(|site| hirsel_core_resolve::Cause {
                site,
                role: hirsel_core_resolve::CausalRole::DerivedHere,
            })
            .collect();
        for mut cause in std::mem::take(&mut r.causal) {
            cause.site = project(cause.site);
            if !causal.iter().any(|c| c.site == cause.site) {
                causal.push(cause);
            }
        }
        r.causal = causal;
        r.detail = None;
        r.unresolved = None;
        let keep = seen.insert((
            r.rule.clone(),
            r.primary.source,
            r.primary.item,
            r.primary.span,
            r.message.clone(),
        ));
        if is_generated {
            r.message = format!("{}: {}", ctx.description, r.message);
        }
        keep
    });
    let mut seen = std::collections::HashSet::new();
    result.incomplete.retain_mut(|g| {
        if !generated(&g.primary) {
            return true;
        }
        let Some(ctx) = context(&g.primary) else {
            return true;
        };
        if own.contains(&(g.primary.source, ctx.owner as u32)) {
            return false;
        }
        let mut path = apply(&mut g.primary, &ctx);
        for cause in g.causal.iter().copied().map(project) {
            if !path.contains(&cause) {
                path.push(cause);
            }
        }
        g.causal = path;
        let keep = seen.insert((
            g.primary.source,
            g.primary.item,
            g.primary.span,
            g.message.clone(),
        ));
        g.message = format!("{}: {}", ctx.description, g.message);
        keep
    });
}

/// A record's sites are canonical. An image body is reported as it stands, so
/// bind its sites and anchors to the image's items once, when it is decoded;
/// a request's own bodies bind the copies they report instead.
fn bind_record_sites(product: &mut DefinitionProduct, headers: &reuse::Headers) {
    let anchors = std::mem::take(&mut product.anchors);
    let bind = |site: &mut Site| {
        if let Some(h) = item_at(site)
            .and_then(|at| anchors.get(&at))
            .and_then(|p| headers.get(p))
        {
            crate::bind(site, h.anchor);
        }
    };
    for r in &mut product.check.refusals {
        bind(&mut r.primary);
        for cause in &mut r.causal {
            bind(&mut cause.site);
        }
    }
    for g in &mut product.incomplete {
        bind(&mut g.primary);
        for site in &mut g.causal {
            bind(site);
        }
    }
    product.anchors = anchors
        .into_values()
        .filter_map(|path| {
            let (_, source, item) = headers.get(&path)?.anchor;
            Some(((source, item), path))
        })
        .collect();
}

#[cfg(test)]
#[path = "../../hirsel-core-rt/src/openat2_required.rs"]
mod openat2_required;

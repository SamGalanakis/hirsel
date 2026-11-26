//! Immutable packages with a fixed root and content-addressed, indexed rows.
use super::*;
use hirsel_core_front::Library;
use hirsel_core_ids::published::{Map, Publisher, Reader, Sparse, Table};
use std::sync::OnceLock;

#[derive(Clone, Default)]
pub enum LibraryInput {
    #[default]
    Sources,
    Image(Arc<CoreImage>),
    ReachableImage(Arc<CoreImage>),
    Produce,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageFile {
    pub module: ModuleId,
    pub names: Vec<Definition>,
    pub row: ContentHash,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ImageDirectory {
    pub compiler: ContentHash,
    pub inputs: CorpusManifest,
    pub stamps: BTreeMap<PackageId, PackageStamp>,
    pub index: Arc<NameIndex>,
    pub files: Table<ImageFile>,
    /// Each file's manifest-relative name, if it had one, and its text's row.
    pub sources: Map<SourceId, (Option<String>, ContentHash)>,
    pub headers: Arc<reuse::Headers>,
    pub anchors: Map<ItemAt, DefPath>,
    pub offsets: Map<ItemAt, u32>,
    pub declarations: ContentHash,
    pub definitions: Map<DefPath, ContentHash>,
    /// Each computed constant's value, by its initialiser's code address
    /// (PROG-10): an importer reads it there and runs nothing.
    pub values: Map<ContentHash, crate::comptime::Value>,
    pub diagnostics: Table<Refusal>,
    pub incomplete: Table<Incomplete>,
    pub resources: Table<ResourceRefusal>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct PackageStamp {
    pub hash: ContentHash,
    pub namespace: Option<hirsel_core_resolve::PackageNamespace>,
    /// Bundled files use their module names in every installation.
    canonical_names: bool,
}

impl PackageStamp {
    pub(crate) fn matches(
        &self,
        requested: &PackageManifest,
        request: &CheckRequest,
    ) -> io::Result<bool> {
        let current = stamp(
            requested,
            request.manifest.as_deref(),
            request.entry.as_ref(),
            self.canonical_names,
        )?;
        Ok(self.hash == current.hash)
    }
}

fn stamp(
    package: &PackageManifest,
    manifest: Option<&Path>,
    entry: Option<&DefPath>,
    canonical_names: bool,
) -> io::Result<PackageStamp> {
    let mut files = Vec::with_capacity(package.sources.len());
    for source in &package.sources {
        let name = if canonical_names {
            None
        } else {
            source.name(manifest)?
        };
        let test = source.captured.as_ref().is_some_and(|source| source.test);
        files.push((
            name,
            &source.modules,
            source.mode() as u8,
            test,
            ContentHash::of_bytes(&source.read()?),
        ));
    }
    let owner = entry.filter(|entry| entry.0.package == package.package);
    let hash = ContentHash::of_parts(
        "hirsel.package-image-stamp.v2",
        [&encode(&(
            &package.package,
            &package.dependencies,
            &package.namespace,
            package.toolchain,
            files,
            owner,
        ))?[..]],
    );
    Ok(PackageStamp {
        hash,
        namespace: package.namespace.clone(),
        canonical_names,
    })
}

pub(crate) struct ImageInput {
    request: CheckRequest,
    stamps: Vec<(PackageId, PackageStamp)>,
}

impl ImageInput {
    pub(crate) fn prepare(mut request: CheckRequest, canonical_names: bool) -> io::Result<Self> {
        if request.entry.is_some() {
            return Err(io::Error::other(
                "a core library image has no program entry",
            ));
        }
        request.library = LibraryInput::Produce;
        request.capture_sources()?;
        request.inputs.normalise(request.manifest.as_deref());
        let stamps = request
            .inputs
            .packages
            .iter()
            .map(|package| {
                stamp(package, request.manifest.as_deref(), None, canonical_names)
                    .map(|stamp| (package.package.clone(), stamp))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { request, stamps })
    }

    pub(crate) fn digest(&self) -> io::Result<ContentHash> {
        let packages: Vec<_> = self.stamps.iter().map(|(_, stamp)| stamp.hash).collect();
        Ok(ContentHash::of_parts(
            "hirsel.package-image-inputs.v1",
            [&encode(&(&self.request.inputs.core, packages))?[..]],
        ))
    }

    pub(crate) fn set_store(&mut self, store: PathBuf) {
        self.request.store = store;
    }
}

/// The workspace image supplies this member's checked dependency packages.
pub(crate) fn closure_image(
    request: &CheckRequest,
    member: &PackageId,
    produce: bool,
    meter: &mut Meter,
) -> io::Result<Option<Arc<CoreImage>>> {
    let mut input = request.clone();
    input
        .inputs
        .packages
        .retain(|package| &package.package != member);
    if input
        .inputs
        .packages
        .iter()
        .all(|package| package.namespace.is_none())
    {
        return Ok(None);
    }
    input.entry = None;
    let mut input = ImageInput::prepare(input, false)?;
    let digest = ContentHash::of_parts(
        "hirsel.workspace-dependency-image.v2",
        [&compiler_identity().0[..], &input.digest()?.0[..]],
    );
    let store = request
        .store
        .join("package-images")
        .join(crate::request::hex(digest));
    if let Some(image) = CoreImage::open(&store)? {
        return Ok(Some(image));
    }
    if produce {
        input.set_store(store);
        return Driver::default()
            .produce_package(input, meter, true, ContentHash::default())
            .map(|(_, image)| image);
    }
    Ok(None)
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ImageRoot {
    pub compiler: ContentHash,
    pub directory: ContentHash,
    pub namespace: ContentHash,
    pub source_stamp: ContentHash,
    /// The published tables' [`crate::image_index::Sections`] record, which
    /// names every page by hash: the root's identity covers each page.
    pub sections: ContentHash,
}
pub struct CoreImage {
    pub(crate) store: Store,
    pub identity: ContentHash,
    pub(crate) source_stamp: ContentHash,
    directory_hash: ContentHash,
    directory: OnceLock<Arc<ImageDirectory>>,
    declarations: OnceLock<Arc<Library>>,
    files: std::sync::Mutex<BTreeMap<usize, Arc<FileProduct>>>,
    bodies: Sparse<Arc<DefinitionProduct>>,
    indexed: Arc<crate::image_index::DiskIndex>,
    /// This image's tables, over its own store and no other reader's.
    reader: Arc<Reader>,
}
impl CoreImage {
    pub(crate) fn store_root(&self) -> PathBuf {
        self.store
            .path(ContentHash::default())
            .parent()
            .unwrap()
            .to_owned()
    }
    /// O(1) in the package size: one selection and a fixed-size root.
    pub fn open(path: &Path) -> io::Result<Option<Arc<Self>>> {
        let store = Store::image(path)?;
        let Some(identity) = store.selection("image-root")? else {
            return Ok(None);
        };
        let Some(bytes) = store.get(identity)? else {
            return Ok(None);
        };
        let Some(root) = decode::<ImageRoot>(&bytes) else {
            return Ok(None);
        };
        if root.compiler != compiler_identity() {
            return Ok(None);
        }
        let indexed = Arc::new(crate::image_index::DiskIndex::new(path)?);
        indexed.open(identity, root.sections);
        let reader = Reader::new(root.namespace, indexed.clone());
        Ok(Some(Arc::new(Self {
            store,
            identity,
            source_stamp: root.source_stamp,
            directory_hash: root.directory,
            directory: OnceLock::new(),
            declarations: OnceLock::new(),
            files: std::sync::Mutex::default(),
            bodies: Sparse::default(),
            indexed,
            reader,
        })))
    }
    pub(crate) fn directory(&self) -> io::Result<Arc<ImageDirectory>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        if let Some(directory) = self.directory.get() {
            return Ok(Arc::clone(directory));
        }
        let directory = Arc::new(self.read::<ImageDirectory>(
            self.directory_hash,
            crate::meter::Count::ImageDirectoryBytes,
        )?);
        let _ = self.directory.set(Arc::clone(&directory));
        Ok(directory)
    }
    /// Resolve one fully qualified name through the package's immutable indexes.
    pub fn resolve_name(&self, path: &DefPath) -> io::Result<Option<Definition>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        hirsel_core_ids::published::catch_io(|| {
            let directory = self.directory()?;
            let index = &directory.index;
            let module = index.module(&ModulePath {
                package: path.0.package.clone(),
                modules: path.0.modules.clone(),
            });
            Ok(module
                .zip(index.symbols.find(&path.0.name))
                .and_then(|(m, n)| index.definition(m, n))
                .map(|id| *index.get(id)))
        })
    }
    /// Demand the named signature/summary row, without loading other declarations.
    pub fn demand_declaration(&self, path: &DefPath) -> io::Result<bool> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        hirsel_core_ids::published::catch_io(|| {
            let library = self.declarations()?;
            Ok(match library.paths.get(path) {
                Some(hirsel_core_front::Def::Fn(f)) => {
                    let _ = &library.fns[*f as usize];
                    true
                }
                Some(hirsel_core_front::Def::Type(d)) => {
                    let _ = &library.decls[d.index()];
                    true
                }
                None => false,
            })
        })
    }
    pub fn decoded_files(&self) -> usize {
        self.files.lock().unwrap().len()
    }
    pub fn decoded_bodies(&self) -> usize {
        self.bodies.len()
    }
    pub fn file(&self, index: usize) -> io::Result<Arc<FileProduct>> {
        hirsel_core_ids::published::catch_io(|| self.file_inner(index))
    }
    /// One library file's manifest-relative name, if it had one.
    pub(crate) fn source_name(&self, source: SourceId) -> io::Result<Option<String>> {
        hirsel_core_ids::published::catch_io(|| {
            Ok(self
                .directory()?
                .sources
                .get(&source)
                .and_then(|(name, _)| name.clone()))
        })
    }
    /// One library file's manifest-relative name, if it had one, and its text
    /// for an executable's source table; never its installed path.
    pub(crate) fn source_file(&self, source: SourceId) -> io::Result<(Option<String>, String)> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        hirsel_core_ids::published::catch_io(|| {
            let directory = self.directory()?;
            let (name, row) = directory.sources.get(&source).ok_or_else(|| {
                io::Error::other("native source file is not in its package image")
            })?;
            Ok((
                name.clone(),
                self.read(*row, crate::meter::Count::ImageSourceBytes)?,
            ))
        })
    }
    pub(crate) fn declaration_file(&self, source: SourceId) -> io::Result<Arc<FileProduct>> {
        let index = self
            .directory()?
            .files
            .iter()
            .position(|file| {
                file.names
                    .iter()
                    .any(|name| name.site.source == Some(source))
            })
            .ok_or_else(|| io::Error::other("candidate source file is not in its package image"))?;
        self.file(index)
    }
    fn file_inner(&self, index: usize) -> io::Result<Arc<FileProduct>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        if let Some(file) = self.files.lock().unwrap().get(&index) {
            return Ok(Arc::clone(file));
        }
        let directory = self.directory()?;
        let row = directory
            .files
            .get(index)
            .ok_or_else(|| io::Error::other("core image file demand outside reached directory"))?
            .row;
        crate::meter::count(crate::meter::Count::ImageFiles, 1);
        let file = Arc::new(self.read::<FileProduct>(row, crate::meter::Count::ImageFileBytes)?);
        self.files.lock().unwrap().insert(index, Arc::clone(&file));
        Ok(file)
    }
    pub(crate) fn has_body(&self, path: &DefPath) -> io::Result<bool> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        hirsel_core_ids::published::catch_io(|| {
            Ok(self.directory()?.definitions.contains_key(path))
        })
    }
    /// The value of the computed constant whose initialiser has `address`.
    pub(crate) fn constant_value(
        &self,
        address: &ContentHash,
    ) -> io::Result<Option<crate::comptime::Value>> {
        hirsel_core_ids::published::catch_io(|| Ok(self.directory()?.values.get(address).cloned()))
    }
    pub fn decoded_index_rows(&self) -> usize {
        self.indexed.decoded_rows()
    }
    pub fn checked_index_pages(&self) -> usize {
        self.indexed.checked_pages()
    }
    pub(crate) fn file_offset(&self, at: ItemAt) -> io::Result<u32> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        hirsel_core_ids::published::catch_io(|| {
            Ok(self.directory()?.offsets.get(&at).copied().unwrap_or(0))
        })
    }
    pub fn demand(&self, path: &DefPath) -> io::Result<Arc<DefinitionProduct>> {
        Ok(Arc::clone(self.demand_arc(path)?))
    }
    pub(crate) fn demand_ref(&self, path: &DefPath) -> io::Result<&DefinitionProduct> {
        Ok(self.demand_arc(path)?.as_ref())
    }
    fn demand_arc(&self, path: &DefPath) -> io::Result<&Arc<DefinitionProduct>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        hirsel_core_ids::published::catch_io(|| {
            let directory = self.directory()?;
            let id = directory.definitions.index_of(path).ok_or_else(|| {
                io::Error::other(format!(
                    "core image body demand outside reached definitions: {path}"
                ))
            })?;
            self.bodies.get_or_try_init(id as u64, || {
                let record = directory.definitions[path];
                let head = self
                    .store
                    .get_body_head(record)?
                    .ok_or_else(|| self.body_miss(record))?;
                crate::meter::count(crate::meter::Count::ImageBodies, 1);
                crate::meter::count(
                    crate::meter::Count::ImageBodyBytes,
                    head.head().len() as u64,
                );
                let source = crate::body::IrSource {
                    store: self.store.clone(),
                    reader: Some(Arc::clone(&self.reader)),
                };
                let mut body = DefinitionProduct::from_head(
                    &head,
                    record,
                    |head| self.reader.decode(|| crate::body::decode_head(head)),
                    source,
                )
                .ok_or_else(|| self.body_miss(record))?;
                bind_record_sites(&mut body, &directory.headers);
                Ok(Arc::new(body))
            })
        })
    }
    pub(crate) fn declarations(&self) -> io::Result<Arc<Library>> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        if let Some(rows) = self.declarations.get() {
            return Ok(Arc::clone(rows));
        }
        let rows = Arc::new(self.read::<Library>(
            self.directory()?.declarations,
            crate::meter::Count::ImageDeclarationBytes,
        )?);
        let _ = self.declarations.set(Arc::clone(&rows));
        Ok(rows)
    }
    fn read<T: serde::de::DeserializeOwned>(
        &self,
        record: ContentHash,
        kind: crate::meter::Count,
    ) -> io::Result<T> {
        let _image = crate::meter::ImageIndex::start(crate::meter::ImageIndexKind::Access);
        let bytes = self.store.get(record)?.ok_or_else(miss)?;
        crate::meter::count(kind, bytes.len() as u64);
        self.reader.decode(|| decode(&bytes)).ok_or_else(miss)
    }
}
impl CoreImage {
    /// A body record the image names that is missing or damaged.
    fn body_miss(&self, record: ContentHash) -> io::Error {
        io::Error::other(format!(
            "core image cache miss: the body record {} is missing or damaged; rebuild the image",
            self.store.path(record).display()
        ))
    }
}
fn miss() -> io::Error {
    io::Error::other("core image cache miss: damaged or missing external bytes; rebuild the image")
}

#[test]
fn source_package_image_uses_the_captured_text_and_name() {
    use hirsel_core_resolve::{CapturedSource, ModuleSource};
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("core.hirsel");
    std::fs::write(&file, "pub fn answer() -> u64 { 41 }").unwrap();
    let lookup = |value, name: &str, namespace| {
        let core = package("core");
        let inputs = CorpusManifest {
            core: core.clone(),
            packages: vec![PackageManifest {
                package: core,
                dependencies: vec![],
                namespace,
                sources: vec![ModuleSource {
                    file: file.clone(),
                    modules: vec!["core".into()],
                    signatures: true,
                    foreign: false,
                    captured: Some(CapturedSource {
                        bytes: format!("pub fn answer() -> u64 {{ {value} }}")
                            .into_bytes()
                            .into(),
                        name: Some(name.into()),
                        test: false,
                    }),
                }],
                toolchain: false,
            }],
        };
        let product =
            PackageProduct::sources(inputs, directory.path(), &mut Meter::new(false)).unwrap();
        let image = CoreImage::open(&product.image.store_root())
            .unwrap()
            .unwrap();
        assert_eq!(image.file(0).unwrap().name.as_deref(), Some(name));
        let answer = image
            .demand(&DefPath::from_full("core@1/core/answer").unwrap())
            .unwrap();
        let result = hirsel_core_eval::run(
            answer.ir().unwrap(),
            answer.check.function.unwrap(),
            &[],
            1_000_000,
        );
        assert_eq!(
            result.outcome,
            hirsel_core_eval::Outcome::Returned(vec![value])
        );
        product.rebuilt
    };
    assert!(lookup(7, "first.hirsel", None));
    assert!(lookup(9, "first.hirsel", None));
    assert!(lookup(9, "renamed.hirsel", None));
    std::fs::remove_file(&file).unwrap();
    assert!(!lookup(9, "renamed.hirsel", None));
    assert!(lookup(
        9,
        "renamed.hirsel",
        Some(hirsel_core_resolve::PackageNamespace {
            root: None,
            foreign: ContentHash::of_bytes(b"changed foreign stamp"),
            published_bindings: BTreeMap::new(),
            foreign_inputs: BTreeMap::new(),
        })
    ));
}

impl Driver {
    /// Use the ordinary request path with an explicit image output product.
    pub fn produce_image(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<(Arc<CheckProduct>, Option<Arc<CoreImage>>)> {
        self.produce_package(
            ImageInput::prepare(request, false)?,
            meter,
            true,
            ContentHash::default(),
        )
    }
    pub(crate) fn produce_package(
        &mut self,
        input: ImageInput,
        meter: &mut Meter,
        require_complete: bool,
        source_stamp: ContentHash,
    ) -> io::Result<(Arc<CheckProduct>, Option<Arc<CoreImage>>)> {
        let ImageInput { request, stamps } = input;
        let stamps = stamps.into_iter().collect();
        let mut inputs = request.inputs.clone();
        let store_path = request.store.clone();
        let product = self.check(request, meter)?;
        let Some(library) = product.library.clone() else {
            return Ok((product, None));
        };
        if require_complete && !product.accepted() {
            return Ok((product, None));
        }
        let store = Store::new(&store_path)?;
        let mut files = Vec::new();
        let mut sources = Map::default();
        for file in &product.files {
            sources.insert(
                file.file,
                (file.name.clone(), store.put(&encode(&file.syntax.source)?)?),
            );
            files.push(ImageFile {
                module: file.module,
                names: file.names.clone(),
                row: store.put(&encode(file.as_ref())?)?,
            });
        }
        // Image bodies are the program's own body records, already published.
        let mut definitions = Map::default();
        for (path, body) in &product.definitions {
            definitions.insert(path.clone(), body.record);
        }
        let values = product.constant_values.published();
        let mut anchors = Map::default();
        for (id, header) in product.headers.rows.iter().enumerate() {
            if let Some(header) = header {
                anchors.insert(
                    (header.anchor.1, header.anchor.2),
                    product
                        .index
                        .definition_path(hirsel_core_resolve::DefinitionId(id as u32)),
                );
            }
        }
        let mut offsets = Map::default();
        for file in &product.files {
            for (i, item) in file.syntax.items.iter().enumerate() {
                offsets.insert((file.file, i as u32), item.file_offset);
            }
        }
        for package in &mut inputs.packages {
            package.sources.clear();
        }
        let namespace = product.snapshot;
        let indexed = Arc::new(crate::image_index::DiskIndex::new(&store_path)?);
        let reader = Reader::new(namespace, indexed.clone());
        let mut writer = crate::image_index::Writer::new(reader.clone());
        let outward = product.index.outward();
        let directory = ImageDirectory {
            compiler: compiler_identity(),
            inputs,
            stamps,
            index: Arc::new(product.index.publish(&mut writer)?),
            files: writer.table(&files.into())?,
            sources: writer.map(&sources)?,
            headers: Arc::new(
                product
                    .headers
                    .seal(&product.index)?
                    .publish(&mut writer, &outward)?,
            ),
            anchors: writer.map(&anchors)?,
            offsets: writer.map(&offsets)?,
            declarations: store.put(&encode(&library.publish(&mut writer, &outward)?)?)?,
            definitions: writer.map(&definitions)?,
            values: writer.map(&values)?,
            diagnostics: writer.table(&product.diagnostics.clone().into())?,
            incomplete: writer.table(&product.incomplete.clone().into())?,
            resources: writer.table(&product.resources.clone().into())?,
        };
        let directory_hash = store.put(&encode(&directory)?)?;
        let (sections, pages) = writer.finish(&store)?;
        let sections_hash = store.put(&sections.encode()?)?;
        let identity = store.put(&encode(&ImageRoot {
            compiler: compiler_identity(),
            directory: directory_hash,
            namespace,
            source_stamp,
            sections: sections_hash,
        })?)?;
        indexed.publish(identity, sections, pages);
        store.select("image-root", identity)?;
        let image = Arc::new(CoreImage {
            store,
            identity,
            source_stamp,
            directory_hash,
            directory: OnceLock::new(),
            declarations: OnceLock::new(),
            files: std::sync::Mutex::default(),
            bodies: Sparse::default(),
            indexed,
            reader,
        });
        let _ = image.directory.set(Arc::new(directory));
        for (i, file) in product.files.iter().enumerate() {
            image.files.lock().unwrap().insert(i, Arc::clone(file));
        }
        let _ = image.declarations.set(library);
        meter.finish("image publication");
        Ok((product, Some(image)))
    }
}

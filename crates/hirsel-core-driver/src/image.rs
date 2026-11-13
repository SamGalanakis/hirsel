//! Immutable packages with a fixed root and content-addressed, indexed rows.
use super::*;
use hirsel_core_front::Library;
use hirsel_core_ids::published::{Map, Publisher, Sparse, Table};
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
    pub path: PathBuf,
    pub names: Vec<Definition>,
    pub row: ContentHash,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ImageDirectory {
    pub compiler: ContentHash,
    pub inputs: CorpusManifest,
    pub index: Arc<NameIndex>,
    pub files: Table<ImageFile>,
    pub headers: Arc<Map<DefPath, reuse::Header>>,
    pub anchors: Map<(ModuleId, u32), DefPath>,
    pub offsets: Map<(ModuleId, u32), u32>,
    pub declarations: ContentHash,
    pub definitions: Map<DefPath, ContentHash>,
    pub diagnostics: Table<Refusal>,
    pub incomplete: Table<Incomplete>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ImageRoot {
    pub compiler: ContentHash,
    pub directory: ContentHash,
    pub namespace: ContentHash,
    pub source_stamp: ContentHash,
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
}
impl CoreImage {
    pub(crate) fn store_root(&self) -> PathBuf {
        self.store
            .path(ContentHash::default())
            .parent()
            .unwrap()
            .to_owned()
    }
    /// O(1) in the package size: one selection and a fixed-size compiler/directory root.
    pub fn open(path: &Path) -> io::Result<Option<Arc<Self>>> {
        let store = Store::new(path)?;
        let Some(identity) = store.selection("image-root")? else {
            return Ok(None);
        };
        let Some(bytes) = store.get(identity)? else {
            return Ok(None);
        };
        let Some(root) = decode::<ImageRoot>(&bytes) else {
            return Ok(None);
        };
        if root.compiler != compiler_identity()? {
            return Ok(None);
        }
        let indexed = Arc::new(crate::image_index::DiskIndex::new(path)?);
        hirsel_core_ids::published::bind_source(root.namespace, indexed.clone());
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
        })))
    }
    pub(crate) fn directory(&self) -> io::Result<Arc<ImageDirectory>> {
        if let Some(directory) = self.directory.get() {
            return Ok(Arc::clone(directory));
        }
        let directory = Arc::new(self.read::<ImageDirectory>(self.directory_hash)?);
        let _ = self.directory.set(Arc::clone(&directory));
        Ok(directory)
    }
    /// Resolve one fully qualified name through the package's immutable indexes.
    pub fn resolve_name(&self, path: &DefPath) -> io::Result<Option<Definition>> {
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
    fn file_inner(&self, index: usize) -> io::Result<Arc<FileProduct>> {
        if let Some(file) = self.files.lock().unwrap().get(&index) {
            return Ok(Arc::clone(file));
        }
        let directory = self.directory()?;
        let row = directory
            .files
            .get(index)
            .ok_or_else(|| io::Error::other("core image file demand outside reached directory"))?
            .row;
        let file = Arc::new(self.read::<FileProduct>(row)?);
        self.files.lock().unwrap().insert(index, Arc::clone(&file));
        Ok(file)
    }
    pub(crate) fn has_body(&self, path: &DefPath) -> io::Result<bool> {
        hirsel_core_ids::published::catch_io(|| {
            Ok(self.directory()?.definitions.contains_key(path))
        })
    }
    pub fn decoded_index_rows(&self) -> usize {
        self.indexed.decoded_rows()
    }
    pub fn decoded_index_nodes(&self) -> usize {
        self.indexed.decoded_nodes()
    }
    pub(crate) fn file_offset(&self, module: ModuleId, item: u32) -> io::Result<u32> {
        hirsel_core_ids::published::catch_io(|| {
            Ok(self
                .directory()?
                .offsets
                .get(&(module, item))
                .copied()
                .unwrap_or(0))
        })
    }
    pub fn demand(&self, path: &DefPath) -> io::Result<Arc<DefinitionProduct>> {
        Ok(Arc::clone(self.demand_arc(path)?))
    }
    pub(crate) fn demand_ref(&self, path: &DefPath) -> io::Result<&DefinitionProduct> {
        Ok(self.demand_arc(path)?.as_ref())
    }
    fn demand_arc(&self, path: &DefPath) -> io::Result<&Arc<DefinitionProduct>> {
        hirsel_core_ids::published::catch_io(|| {
            let directory = self.directory()?;
            let id = directory.definitions.index_of(path).ok_or_else(|| {
                io::Error::other(format!(
                    "core image body demand outside reached definitions: {path}"
                ))
            })?;
            self.bodies.get_or_try_init(id as u64, || {
                let disk = self.read::<DiskCheck>(directory.definitions[path])?;
                Ok(Arc::new(from_disk(disk)))
            })
        })
    }
    pub(crate) fn declarations(&self) -> io::Result<Arc<Library>> {
        if let Some(rows) = self.declarations.get() {
            return Ok(Arc::clone(rows));
        }
        let rows = Arc::new(self.read::<Library>(self.directory()?.declarations)?);
        let _ = self.declarations.set(Arc::clone(&rows));
        Ok(rows)
    }
    fn read<T: serde::de::DeserializeOwned>(&self, record: ContentHash) -> io::Result<T> {
        let bytes = self.store.get(record)?.ok_or_else(miss)?;
        decode(&bytes).ok_or_else(miss)
    }
}
fn miss() -> io::Error {
    io::Error::other("core image cache miss: damaged or missing external bytes; rebuild the image")
}
impl Driver {
    /// Use the ordinary request path with an explicit image output product.
    pub fn produce_image(
        &mut self,
        request: CheckRequest,
        meter: &mut Meter,
    ) -> io::Result<(Arc<CheckProduct>, Option<Arc<CoreImage>>)> {
        self.produce_package(request, meter, true, ContentHash::default())
    }
    pub(crate) fn produce_package(
        &mut self,
        mut request: CheckRequest,
        meter: &mut Meter,
        require_complete: bool,
        source_stamp: ContentHash,
    ) -> io::Result<(Arc<CheckProduct>, Option<Arc<CoreImage>>)> {
        if request.entry.is_some() {
            return Err(io::Error::other(
                "a core library image has no program entry",
            ));
        }
        request.library = LibraryInput::Produce;
        let mut inputs = request.inputs.clone();
        inputs.packages.sort_by(|a, b| a.package.cmp(&b.package));
        for p in &mut inputs.packages {
            p.dependencies.sort();
            p.sources.sort_by(|a, b| {
                (&a.modules, &a.file, a.signatures).cmp(&(&b.modules, &b.file, b.signatures))
            });
        }
        let store_path = request.store.clone();
        let product = self.check(request, meter)?;
        if require_complete && !product.accepted() || product.records.is_none() {
            return Ok((product, None));
        }
        let store = Store::new(&store_path)?;
        let library = if let Some(library) = &product.library {
            Arc::clone(library)
        } else {
            // A foreign/missing image directory can be rebuilt from the ordinary
            // cached check product without checking its source again.
            let cache = Cache::open(&store, product.root)?.ok_or_else(miss)?;
            let record = cache.root.declarations.ok_or_else(miss)?;
            Arc::new(
                decode::<Library>(record.bytes(&cache.bytes).ok_or_else(miss)?).ok_or_else(miss)?,
            )
        };
        let mut files = Vec::new();
        for file in &product.files {
            files.push(ImageFile {
                module: file.module,
                path: file.path.clone(),
                names: file.names.clone(),
                row: store.put(&encode(file.as_ref())?)?,
            });
        }
        let mut definitions = Map::default();
        for (path, body) in &product.definitions {
            let c = body.check.counts;
            let disk = DiskCheckRef {
                anchors: &body.anchors,
                key: &body.key,
                definition: path,
                refusals: &body.check.refusals,
                incomplete: &body.incomplete,
                counts: [c.nodes, c.uses, c.places, c.calls, c.types, c.bytes],
                template: &body.check.template,
                function: body.check.function.map(|f| f.0),
                ir: &body.ir,
                resolved: body.resolved,
            };
            let hash = store.archive(|writer| {
                writer.record(&disk)?;
                Ok(())
            })?;
            definitions.insert(path.clone(), hash);
        }
        let mut anchors = Map::default();
        for (path, header) in product.headers.iter() {
            anchors.insert(header.anchor, path.clone());
        }
        let mut offsets = Map::default();
        for file in &product.files {
            for (i, item) in file.syntax.items.iter().enumerate() {
                offsets.insert((file.module, i as u32), item.file_offset);
            }
        }
        for package in &mut inputs.packages {
            package.sources.clear();
        }
        let namespace = product.snapshot;
        let indexed = Arc::new(crate::image_index::DiskIndex::new(&store_path)?);
        hirsel_core_ids::published::bind_source(namespace, indexed.clone());
        let mut writer = crate::image_index::Writer::new(&store, namespace);
        let directory = ImageDirectory {
            compiler: compiler_identity()?,
            inputs,
            index: Arc::new(product.index.publish(&mut writer)?),
            files: writer.table(&files.into())?,
            headers: Arc::new(writer.map(&product.headers)?),
            anchors: writer.map(&anchors)?,
            offsets: writer.map(&offsets)?,
            declarations: store.put(&encode(&library.publish(&mut writer)?)?)?,
            definitions: writer.map(&definitions)?,
            diagnostics: writer.table(&product.diagnostics.clone().into())?,
            incomplete: writer.table(&product.incomplete.clone().into())?,
        };
        let directory_hash = store.put(&encode(&directory)?)?;
        let identity = store.put(&encode(&ImageRoot {
            compiler: compiler_identity()?,
            directory: directory_hash,
            namespace,
            source_stamp,
        })?)?;
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

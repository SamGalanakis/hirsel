//! New-driver immutable images. Opening reads only the directory; declarations,
//! source/name rows and checked bodies decode on their first respective demand.
use super::*;
use hirsel_core_front::Library;
use std::sync::OnceLock;

#[derive(Clone, Default)]
pub enum LibraryInput {
    #[default]
    Sources,
    Image(Arc<CoreImage>),
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
    pub files: Vec<ImageFile>,
    pub headers: Arc<BTreeMap<DefPath, reuse::Header>>,
    pub declarations: ContentHash,
    pub definitions: BTreeMap<DefPath, ContentHash>,
    pub diagnostics: Vec<Refusal>,
    pub incomplete: Vec<Incomplete>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ImageRoot {
    pub compiler: ContentHash,
    pub directory: ContentHash,
}
pub struct CoreImage {
    pub(crate) store: Store,
    pub identity: ContentHash,
    directory_hash: ContentHash,
    directory: OnceLock<Arc<ImageDirectory>>,
    declarations: OnceLock<Arc<Library>>,
    files: std::sync::Mutex<BTreeMap<usize, Arc<FileProduct>>>,
    bodies: std::sync::Mutex<BTreeMap<DefPath, Arc<DefinitionProduct>>>,
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
        Ok(Some(Arc::new(Self {
            store,
            identity,
            directory_hash: root.directory,
            directory: OnceLock::new(),
            declarations: OnceLock::new(),
            files: std::sync::Mutex::default(),
            bodies: std::sync::Mutex::default(),
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
    pub fn decoded_files(&self) -> usize {
        self.files.lock().unwrap().len()
    }
    pub fn decoded_bodies(&self) -> usize {
        self.bodies.lock().unwrap().len()
    }
    pub fn file(&self, index: usize) -> io::Result<Arc<FileProduct>> {
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
        Ok(self.directory()?.definitions.contains_key(path))
    }
    pub fn demand(&self, path: &DefPath) -> io::Result<Arc<DefinitionProduct>> {
        if let Some(body) = self.bodies.lock().unwrap().get(path) {
            return Ok(Arc::clone(body));
        }
        let directory = self.directory()?;
        let hash = directory.definitions.get(path).ok_or_else(|| {
            io::Error::other(format!(
                "core image body demand outside reached definitions: {path}"
            ))
        })?;
        let disk = self.read::<DiskCheck>(*hash)?;
        let row = Arc::new(from_disk(disk));
        self.bodies
            .lock()
            .unwrap()
            .insert(path.clone(), Arc::clone(&row));
        Ok(row)
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
        self.produce_package(request, meter, true)
    }
    pub(crate) fn produce_package(
        &mut self,
        mut request: CheckRequest,
        meter: &mut Meter,
        require_complete: bool,
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
        let declarations = store.put(&encode(library.as_ref())?)?;
        let mut files = Vec::new();
        for file in &product.files {
            files.push(ImageFile {
                module: file.module,
                path: file.path.clone(),
                names: file.names.clone(),
                row: store.put(&encode(file.as_ref())?)?,
            });
        }
        let mut definitions = BTreeMap::new();
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
        let directory = ImageDirectory {
            compiler: compiler_identity()?,
            inputs,
            index: Arc::clone(&product.index),
            files,
            headers: Arc::clone(&product.headers),
            declarations,
            definitions,
            diagnostics: product.diagnostics.clone(),
            incomplete: product.incomplete.clone(),
        };
        let directory_hash = store.put(&encode(&directory)?)?;
        let identity = store.put(&encode(&ImageRoot {
            compiler: compiler_identity()?,
            directory: directory_hash,
        })?)?;
        store.select("image-root", identity)?;
        let image = Arc::new(CoreImage {
            store,
            identity,
            directory_hash,
            directory: OnceLock::new(),
            declarations: OnceLock::new(),
            files: std::sync::Mutex::default(),
            bodies: std::sync::Mutex::default(),
        });
        let _ = image.directory.set(Arc::new(directory));
        for (i, file) in product.files.iter().enumerate() {
            image.files.lock().unwrap().insert(i, Arc::clone(file));
        }
        for (path, body) in &product.definitions {
            image
                .bodies
                .lock()
                .unwrap()
                .insert(path.clone(), Arc::clone(body));
        }
        let _ = image.declarations.set(library);
        meter.finish("image publication");
        Ok((product, Some(image)))
    }
}

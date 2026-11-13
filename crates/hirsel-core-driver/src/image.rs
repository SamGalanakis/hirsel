//! New-driver immutable images. Opening reads only the directory; declarations,
//! source/name rows and checked bodies decode on their first respective demand.
use super::*;
use hirsel_core_front::Library;
use std::sync::OnceLock;

#[derive(Default)]
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
    pub row: Record,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ImageDirectory {
    pub compiler: ContentHash,
    pub inputs: CorpusManifest,
    pub index: Arc<NameIndex>,
    pub files: Vec<ImageFile>,
    pub headers: BTreeMap<DefPath, reuse::Header>,
    pub archive: ContentHash,
    pub declarations: Record,
    pub definitions: BTreeMap<DefPath, Record>,
}
pub struct CoreImage {
    store: Store,
    pub identity: ContentHash,
    pub(crate) directory: ImageDirectory,
    backing: OnceLock<Arc<Cache>>,
    declarations: OnceLock<Arc<Library>>,
    files: Vec<OnceLock<Arc<FileProduct>>>,
    bodies: BTreeMap<DefPath, OnceLock<Arc<DefinitionProduct>>>,
}
impl CoreImage {
    pub fn open(path: &Path) -> io::Result<Option<Arc<Self>>> {
        let store = Store::new(path)?;
        let Some(identity) = store.selection("image-root")? else {
            return Ok(None);
        };
        let Some(bytes) = store.get(identity)? else {
            return Ok(None);
        };
        let Some(directory) = decode::<ImageDirectory>(&bytes) else {
            return Ok(None);
        };
        if directory.compiler != compiler_identity()? {
            return Ok(None);
        }
        Ok(Some(Self::from_directory(store, identity, directory)))
    }
    fn from_directory(store: Store, identity: ContentHash, directory: ImageDirectory) -> Arc<Self> {
        let files = directory.files.iter().map(|_| OnceLock::new()).collect();
        let bodies = directory
            .definitions
            .keys()
            .map(|p| (p.clone(), OnceLock::new()))
            .collect();
        Arc::new(Self {
            store,
            identity,
            directory,
            backing: OnceLock::new(),
            declarations: OnceLock::new(),
            files,
            bodies,
        })
    }
    pub fn decoded_files(&self) -> usize {
        self.files.iter().filter(|r| r.get().is_some()).count()
    }
    pub fn decoded_bodies(&self) -> usize {
        self.bodies.values().filter(|r| r.get().is_some()).count()
    }
    pub fn file(&self, index: usize) -> io::Result<Arc<FileProduct>> {
        let Some(slot) = self.files.get(index) else {
            return Err(io::Error::other(
                "core image file demand outside reached directory",
            ));
        };
        if let Some(row) = slot.get() {
            return Ok(Arc::clone(row));
        }
        let row = self.read::<FileProduct>(self.directory.files[index].row)?;
        let row = Arc::new(row);
        let _ = slot.set(Arc::clone(&row));
        Ok(row)
    }
    pub fn demand(&self, path: &DefPath) -> io::Result<Arc<DefinitionProduct>> {
        let Some(slot) = self.bodies.get(path) else {
            return Err(io::Error::other(format!(
                "core image body demand outside reached definitions: {path}"
            )));
        };
        if let Some(row) = slot.get() {
            return Ok(Arc::clone(row));
        }
        let hash = self.directory.definitions[path];
        let disk = self.read::<DiskCheck>(hash)?;
        let row = Arc::new(from_disk(disk));
        let _ = slot.set(Arc::clone(&row));
        Ok(row)
    }
    pub(crate) fn declarations(&self) -> io::Result<Arc<Library>> {
        if let Some(rows) = self.declarations.get() {
            return Ok(Arc::clone(rows));
        }
        let rows = Arc::new(self.read::<Library>(self.directory.declarations)?);
        let _ = self.declarations.set(Arc::clone(&rows));
        Ok(rows)
    }
    fn read<T: serde::de::DeserializeOwned>(&self, record: Record) -> io::Result<T> {
        let backing = if let Some(backing) = self.backing.get() {
            Arc::clone(backing)
        } else {
            let backing =
                Arc::new(Cache::open(&self.store, self.directory.archive)?.ok_or_else(miss)?);
            let _ = self.backing.set(Arc::clone(&backing));
            backing
        };
        decode(record.bytes(&backing.bytes).ok_or_else(miss)?).ok_or_else(miss)
    }
}
fn miss() -> io::Error {
    io::Error::other("core image cache miss: damaged or missing external bytes; rebuild the image")
}
impl Driver {
    /// Use the ordinary request path with an explicit image output product.
    pub fn produce_image(
        &mut self,
        mut request: CheckRequest,
        meter: &mut Meter,
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
        if !product.accepted() {
            return Ok((product, None));
        }
        let store = Store::new(&store_path)?;
        let root = product
            .records
            .as_ref()
            .expect("image request publishes its checked declarations");
        let directory = ImageDirectory {
            compiler: compiler_identity()?,
            inputs,
            index: Arc::clone(&product.index),
            files: product
                .files
                .iter()
                .zip(root.files.iter().copied())
                .map(|(file, row)| ImageFile {
                    module: file.module.clone(),
                    path: file.path.clone(),
                    names: file.names.clone(),
                    row,
                })
                .collect(),
            headers: root.headers.clone(),
            archive: product.root,
            declarations: root
                .declarations
                .expect("image request publishes its checked declarations"),
            definitions: root.definitions.clone(),
        };
        let identity = store.archive(|writer| {
            writer.record(&directory)?;
            Ok(())
        })?;
        store.select("image-root", identity)?;
        let image = CoreImage::from_directory(store, identity, directory);
        for (slot, file) in image.files.iter().zip(&product.files) {
            let _ = slot.set(Arc::clone(file));
        }
        for (path, body) in &product.definitions {
            let _ = image.bodies[path].set(Arc::clone(body));
        }
        if let Some(rows) = &product.library {
            let _ = image.declarations.set(Arc::clone(rows));
        }
        meter.finish("image publication");
        Ok((product, Some(image)))
    }
}

//! Immutable code objects, named by kind and SHA-256.
//!
//! A store holds loose objects, `<kind directory>/<name>`, and packs,
//! `packs/*.pack` (`pack`). Every store reads both. A durable store (`new`)
//! writes loose objects, each synced and named atomically ("Durable
//! publication" in docs/spec/launch.md): host, publish and supplied stores.
//! A cache store (`cache`) is a build's own, inside its driver store: it
//! holds what `put` gives it until `commit` writes one unsynced pack, and a
//! lost or damaged object is a miss that the next build writes again.

use crate::store::Store;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::ObjectRef;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
mod pack;

/// A launcher publish writes its recipe here until the compiling process exits.
pub(crate) const PUBLISH_RECIPE_STAGE: &str = "HIRSEL_PUBLISH_RECIPE_STAGE";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ObjectKind {
    Body,
    Annex,
    Group,
    Recipe,
    ForeignArchive,
}

impl ObjectKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Body => "bodies",
            Self::Annex => "annexes",
            Self::Group => "groups",
            Self::Recipe => "recipes",
            Self::ForeignArchive => "archives",
        }
    }

    /// The kind's code in a pack index.
    fn code(self) -> u8 {
        match self {
            Self::Body => 0,
            Self::Annex => 1,
            Self::Group => 2,
            Self::Recipe => 3,
            Self::ForeignArchive => 4,
        }
    }

    fn domain(self) -> Option<&'static str> {
        Some(match self {
            Self::Body => hirsel_core_ir::normal::BODY_HASH_DOMAIN,
            Self::Annex => "hirsel.code-annex.v1",
            Self::Group => "hirsel.code-group.v1",
            Self::Recipe => hirsel_core_ir::release_recipe::HASH_DOMAIN,
            Self::ForeignArchive => return None,
        })
    }

    pub fn hash(self, bytes: &[u8]) -> ContentHash {
        match self.domain() {
            Some(domain) => ContentHash::of_parts(domain, [bytes]),
            None => ContentHash(Sha256::digest(bytes).into()),
        }
    }

    pub(crate) fn hash_stream(
        self,
        length: u64,
        emit: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<ContentHash> {
        match self.domain() {
            Some(domain) => ContentHash::of_stream(domain, length, emit),
            None => {
                let mut hash = Sha256::new();
                emit(&mut hash)?;
                Ok(ContentHash(hash.finalize().into()))
            }
        }
    }
}

/// One object's bytes as a store holds them: a loose file, or a pack row
/// already checked.
pub struct Object {
    len: u64,
    source: Source,
}

enum Source {
    File(File),
    Packed(io::Cursor<Vec<u8>>),
}

impl Object {
    /// A regular file's object.
    pub(crate) fn file(file: File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "code object is not a regular file",
            ));
        }
        Ok(Self {
            len: metadata.len(),
            source: Source::File(file),
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }
}

impl Read for Object {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.source {
            Source::File(file) => file.read(buf),
            Source::Packed(bytes) => bytes.read(buf),
        }
    }
}

#[derive(Clone)]
pub struct CodeStore {
    root: PathBuf,
    packs: Arc<Mutex<pack::Packs>>,
    /// The driver store whose temporaries a cache store's packs are written
    /// through; `None` for a durable store.
    cache: Option<Store>,
    recipe_stage: Option<PathBuf>,
}

impl CodeStore {
    /// A durable store at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            packs: Arc::new(Mutex::new(pack::Packs::new(&root))),
            root,
            cache: None,
            recipe_stage: None,
        }
    }

    /// The cache store `name` inside driver store `store`.
    pub(crate) fn cache(store: &Path, name: &str) -> io::Result<Self> {
        Ok(Self {
            cache: Some(Store::new(store)?),
            ..Self::new(store.join(name))
        })
    }

    pub(crate) fn for_request(
        command: &str,
        path: Option<PathBuf>,
        compiler: &Path,
    ) -> io::Result<Self> {
        let mut code = match path {
            Some(path) => Self::new(path),
            None if command == "build" => Self::cache(compiler, "code-store")?,
            None => Self::new(compiler.join("code-store")),
        };
        if command == "publish" {
            code.recipe_stage = std::env::var_os(PUBLISH_RECIPE_STAGE).map(PathBuf::from);
        }
        Ok(code)
    }

    /// Whether this is a build's own cache store.
    pub(crate) fn is_cache(&self) -> bool {
        self.cache.is_some()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// A loose object's path.
    pub fn path(&self, kind: ObjectKind, hash: ContentHash) -> PathBuf {
        self.root
            .join(kind.directory())
            .join(crate::native::selection(hash))
    }

    /// The `kind` object named `hash`: a pack's whole row, else, in a durable
    /// store, the loose file. A damaged row is not found.
    pub fn open(&self, kind: ObjectKind, hash: ContentHash) -> io::Result<Object> {
        if let Some(bytes) = self.packs.lock().unwrap().read(kind, hash)? {
            return Ok(Object {
                len: bytes.len() as u64,
                source: Source::Packed(io::Cursor::new(bytes)),
            });
        }
        if self.cache.is_some() {
            return Err(io::ErrorKind::NotFound.into());
        }
        Object::file(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(self.path(kind, hash))?,
        )
    }

    /// Whether the store holds `object`: a whole pack row of its length, or,
    /// in a durable store, a loose file of its name.
    pub(crate) fn contains(&self, kind: ObjectKind, object: ObjectRef) -> io::Result<bool> {
        if self
            .packs
            .lock()
            .unwrap()
            .contains(kind, object.hash, object.len)?
        {
            return Ok(true);
        }
        if self.cache.is_some() {
            return Ok(false);
        }
        self.path(kind, object.hash).try_exists()
    }

    /// Write a cache store's pending objects as one pack. A durable store's
    /// objects are written by `put`.
    pub(crate) fn commit(&self) -> io::Result<()> {
        match &self.cache {
            Some(store) => self.packs.lock().unwrap().commit(store),
            None => Ok(()),
        }
    }

    /// Make a private, already verified admission file durable and visible.
    pub(crate) fn publish_staged(
        &self,
        kind: ObjectKind,
        hash: ContentHash,
        staged: &Path,
    ) -> io::Result<()> {
        let path = self.path(kind, hash);
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        let file = File::open(staged)?;
        file.set_permissions(Permissions::from_mode(0o400))?;
        file.sync_all()?;
        match fs::hard_link(staged, &path) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        sync_dir(parent)?;
        sync_dir(&self.root)
    }

    /// Remove every object `keep` refuses that was not published within `fresh`; answer
    /// how many. `put` refreshes an object it finds present, so a publication's whole
    /// closure stays at least `fresh` after it was published, until an admission roots it.
    pub(crate) fn collect(
        &self,
        keep: impl Fn(ObjectKind, ContentHash) -> bool,
        fresh: std::time::Duration,
    ) -> io::Result<u64> {
        let floor = std::time::SystemTime::now()
            .checked_sub(fresh)
            .unwrap_or(std::time::UNIX_EPOCH);
        let mut removed = 0;
        for kind in [
            ObjectKind::Recipe,
            ObjectKind::Group,
            ObjectKind::Body,
            ObjectKind::Annex,
            ObjectKind::ForeignArchive,
        ] {
            let directory = self.root.join(kind.directory());
            let rows = match fs::read_dir(&directory) {
                Ok(rows) => rows,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for row in rows {
                let row = row?;
                if let Some(name) = row.file_name().to_str()
                    && let Ok(hash) = crate::native::recipe_build::parse_root(name)
                    && !keep(kind, hash)
                    && row.metadata()?.modified()? < floor
                {
                    fs::remove_file(row.path())?;
                    removed += 1;
                }
            }
            sync_dir(&directory)?;
        }
        Ok(removed)
    }

    /// Publish one checked object: a durable store's with an atomic,
    /// non-replacing name, a cache store's at its next commit.
    pub fn put(&self, kind: ObjectKind, bytes: &[u8]) -> io::Result<ObjectRef> {
        let object = ObjectRef {
            hash: kind.hash(bytes),
            len: bytes.len() as u64,
        };
        if kind == ObjectKind::Recipe
            && let Some(stage) = &self.recipe_stage
        {
            return Self::new(stage).put_addressed(kind, object, bytes);
        }
        self.put_addressed(kind, object, bytes)
    }

    /// `put` of an object whose name the caller computed.
    pub(crate) fn put_addressed(
        &self,
        kind: ObjectKind,
        object: ObjectRef,
        bytes: &[u8],
    ) -> io::Result<ObjectRef> {
        if self.cache.is_some() {
            if !self.contains(kind, object)? {
                self.packs.lock().unwrap().put(kind, object.hash, bytes);
            }
            return Ok(object);
        }
        let path = self.path(kind, object.hash);
        if path.try_exists()? {
            // Published again: refresh it, so collection keeps it as long as a new object.
            // A store this user does not own is not this user's to collect either.
            match File::open(&path)?.set_modified(std::time::SystemTime::now()) {
                Err(error) if error.kind() != io::ErrorKind::PermissionDenied => return Err(error),
                _ => {}
            }
            return Ok(object);
        }
        let parent = path.parent().expect("objects have a kind directory");
        fs::create_dir_all(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(bytes)?;
        staged
            .as_file()
            .set_permissions(Permissions::from_mode(0o444))?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&path) {
            Ok(_) => {}
            Err(staged) if staged.error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(staged) => return Err(staged.error),
        }
        sync_dir(parent)?;
        sync_dir(&self.root)?;
        Ok(object)
    }
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How a power loss may leave a renamed pack.
    #[derive(Clone, Copy, Debug)]
    enum Damage {
        /// Cut short at a record boundary.
        Cut(usize),
        /// Zero-filled from a record boundary to its end.
        Zeroed(usize),
        /// One payload's bytes zero-filled.
        Payload(usize),
    }

    /// A cache store's committed pack, damaged as a power loss may leave it
    /// at each record boundary and in each payload: every object reads back
    /// as the bytes put or is absent (`open` and `contains` agree), never
    /// other bytes; putting every object again and committing makes each
    /// one read back whole in a new process. The undamaged pack serves every
    /// object. Oracle: the bytes put.
    #[test]
    fn a_damaged_pack_is_a_miss_and_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let objects: Vec<(ObjectKind, Vec<u8>)> = vec![
            (ObjectKind::Body, b"body one".to_vec()),
            (ObjectKind::Body, vec![7; 5000]),
            (ObjectKind::Annex, b"annex".to_vec()),
            (ObjectKind::Group, (0..=255).collect()),
            (ObjectKind::Recipe, b"recipe".to_vec()),
            (ObjectKind::ForeignArchive, b"!<arch>\n".to_vec()),
        ];
        let put_all = |store: &CodeStore| {
            let refs = objects
                .iter()
                .map(|(kind, bytes)| store.put(*kind, bytes).unwrap())
                .collect::<Vec<_>>();
            store.commit().unwrap();
            refs
        };
        let written = CodeStore::cache(&dir.path().join("written"), "code").unwrap();
        let refs = put_all(&written);
        let packs = dir.path().join("written/code/packs");
        let [pack] = fs::read_dir(&packs)
            .unwrap()
            .map(|row| row.unwrap().path())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let bytes = fs::read(&pack).unwrap();
        let cuts = pack::tests::boundaries(&bytes);
        let payloads = refs
            .iter()
            .zip(&objects)
            .map(|(object, (_, content))| {
                let at = bytes
                    .windows(content.len())
                    .position(|w| w == content.as_slice())
                    .unwrap();
                assert_eq!(object.len, content.len() as u64);
                at..at + content.len()
            })
            .collect::<Vec<_>>();
        let damages = cuts
            .iter()
            .flat_map(|&cut| [Damage::Cut(cut), Damage::Zeroed(cut)])
            .chain((0..payloads.len()).map(Damage::Payload));
        for (case, damage) in damages.enumerate() {
            let mut damaged = bytes.clone();
            match damage {
                Damage::Cut(cut) => damaged.truncate(cut),
                Damage::Zeroed(cut) => damaged[cut..].fill(0),
                Damage::Payload(at) => damaged[payloads[at].clone()].fill(0),
            }
            let driver = dir.path().join(format!("case-{case}"));
            fs::create_dir_all(driver.join("code/packs")).unwrap();
            fs::write(
                driver.join("code/packs").join(pack.file_name().unwrap()),
                &damaged,
            )
            .unwrap();
            let store = CodeStore::cache(&driver, "code").unwrap();
            let mut served = 0;
            for ((kind, content), object) in objects.iter().zip(&refs) {
                let held = store.contains(*kind, *object).unwrap();
                match store.open(*kind, object.hash) {
                    Ok(mut read) => {
                        let mut got = Vec::new();
                        read.read_to_end(&mut got).unwrap();
                        assert_eq!(&got, content, "{damage:?}: {kind:?} served other bytes");
                        assert!(held, "{damage:?}: {kind:?} read but not held");
                        served += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        assert!(!held, "{damage:?}: {kind:?} held but not read");
                    }
                    Err(error) => panic!("{damage:?}: {kind:?}: {error}"),
                }
            }
            if damaged == bytes {
                assert_eq!(served, objects.len(), "{damage:?}: the whole pack");
            }
            assert_eq!(put_all(&store), refs);
            let rebuilt = CodeStore::cache(&driver, "code").unwrap();
            for ((kind, content), object) in objects.iter().zip(&refs) {
                let mut got = Vec::new();
                rebuilt
                    .open(*kind, object.hash)
                    .unwrap_or_else(|error| panic!("{damage:?}: {kind:?} after rebuild: {error}"))
                    .read_to_end(&mut got)
                    .unwrap();
                assert_eq!(&got, content, "{damage:?}: {kind:?} after rebuild");
            }
        }
    }
}

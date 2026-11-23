//! Immutable code objects. Native action results use a separate local cache.

use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::ObjectRef;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
mod watch;
pub(crate) use watch::{Presence, Watch};

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

#[derive(Clone)]
pub struct CodeStore {
    root: PathBuf,
    local_build: bool,
    recipe_stage: Option<PathBuf>,
}

impl CodeStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            local_build: false,
            recipe_stage: None,
        }
    }

    /// Default code cache of a CLI build. Collection leases and publication
    /// freshness apply to a supplied code store, whose objects are shared
    /// with admission.
    pub(crate) fn for_build(root: PathBuf) -> Self {
        Self {
            root,
            local_build: true,
            recipe_stage: None,
        }
    }

    pub(crate) fn for_request(command: &str, path: Option<PathBuf>, compiler: &Path) -> Self {
        let mut code = match path {
            Some(path) => Self::new(path),
            None if command == "build" => Self::for_build(compiler.join("code-store")),
            None => Self::new(compiler.join("code-store")),
        };
        if command == "publish" {
            code.recipe_stage = std::env::var_os(PUBLISH_RECIPE_STAGE).map(PathBuf::from);
        }
        code
    }

    pub(crate) fn local_build(&self) -> bool {
        self.local_build
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, kind: ObjectKind, hash: ContentHash) -> PathBuf {
        self.root
            .join(kind.directory())
            .join(crate::native::selection(hash))
    }

    pub fn open(&self, kind: ObjectKind, hash: ContentHash) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.path(kind, hash))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "code object is not a regular file",
            ));
        }
        Ok(file)
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

    /// Publish one checked object, with an atomic, non-replacing name.
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

    fn put_addressed(
        &self,
        kind: ObjectKind,
        object: ObjectRef,
        bytes: &[u8],
    ) -> io::Result<ObjectRef> {
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

//! Immutable code objects. Native action results use a separate local cache.

use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::ObjectRef;
use std::{
    fs::{self, File, Permissions},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ObjectKind {
    Body,
    Annex,
    Group,
    Recipe,
}

impl ObjectKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Body => "bodies",
            Self::Annex => "annexes",
            Self::Group => "groups",
            Self::Recipe => "recipes",
        }
    }

    pub fn hash(self, bytes: &[u8]) -> ContentHash {
        let domain = match self {
            Self::Body => "hirsel.code-body.v1",
            Self::Annex => "hirsel.code-annex.v1",
            Self::Group => "hirsel.code-group.v1",
            Self::Recipe => "hirsel.release-recipe.v1",
        };
        ContentHash::of_parts(domain, [bytes])
    }
}

pub struct CodeStore {
    root: PathBuf,
}

impl CodeStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self, kind: ObjectKind, hash: ContentHash) -> PathBuf {
        self.root
            .join(kind.directory())
            .join(crate::native::selection(hash))
    }

    pub fn open(&self, kind: ObjectKind, hash: ContentHash) -> io::Result<File> {
        File::open(self.path(kind, hash))
    }

    /// Publish one checked object, with an atomic, non-replacing name.
    pub fn put(&self, kind: ObjectKind, bytes: &[u8]) -> io::Result<ObjectRef> {
        let object = ObjectRef {
            hash: kind.hash(bytes),
            len: bytes.len() as u64,
        };
        let path = self.path(kind, object.hash);
        if path.try_exists()? {
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

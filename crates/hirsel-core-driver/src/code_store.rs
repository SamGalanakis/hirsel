//! Immutable code objects. Native action results use a separate local cache.

use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::ObjectRef;
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
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

    pub(crate) fn domain(self) -> &'static str {
        match self {
            Self::Body => hirsel_core_ir::normal::BODY_HASH_DOMAIN,
            Self::Annex => "hirsel.code-annex.v1",
            Self::Group => "hirsel.code-group.v1",
            Self::Recipe => hirsel_core_ir::release_recipe::HASH_DOMAIN,
        }
    }

    pub fn hash(self, bytes: &[u8]) -> ContentHash {
        ContentHash::of_parts(self.domain(), [bytes])
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

    pub(crate) fn collect(&self, keep: impl Fn(ObjectKind, ContentHash) -> bool) -> io::Result<()> {
        for kind in [
            ObjectKind::Recipe,
            ObjectKind::Group,
            ObjectKind::Body,
            ObjectKind::Annex,
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
                {
                    fs::remove_file(row.path())?;
                }
            }
            sync_dir(&directory)?;
        }
        Ok(())
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

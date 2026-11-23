//! The launcher's retained objects (launch.md "Recording and replay"): each assembled image
//! under `images/` by its SHA-256, and each assembly record under `assemblies/` by its
//! identity. A writer holds `LOCK`; a reader opened with `read_only` relies on its caller to
//! exclude writers.
use crate::AssemblyRecord;
use hirsel_core_ids::ContentHash;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(crate) struct Records {
    root: PathBuf,
    _owner: Option<File>,
}

impl Records {
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let owner = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("LOCK"))?;
        owner.try_lock()?;
        for name in ["images", "assemblies"] {
            fs::create_dir_all(root.join(name))?;
        }
        sync_dir(&root)?;
        Ok(Self {
            root,
            _owner: Some(owner),
        })
    }

    pub(crate) fn read_only(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            _owner: None,
        })
    }

    pub(crate) fn put_assembly(&self, record: &AssemblyRecord, bytes: &[u8]) -> io::Result<()> {
        write_record(&self.image_path(record.digest), bytes, 0o500, true)?;
        let payload = [record.recipe.0, record.toolchain.0, record.digest.0].concat();
        write_record(
            &self.key("assemblies", record.identity),
            &payload,
            0o400,
            true,
        )
    }

    pub(crate) fn assembly(&self, identity: ContentHash) -> io::Result<AssemblyRecord> {
        let [recipe, toolchain, digest] = read_hashes(&self.key("assemblies", identity))?;
        let payload = [recipe.0, toolchain.0, digest.0].concat();
        if ContentHash::of_parts("hirsel.assembly-record.v1", [payload.as_slice()]) != identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "assembly record key",
            ));
        }
        Ok(AssemblyRecord {
            identity,
            recipe,
            toolchain,
            digest,
        })
    }

    pub(crate) fn image_path(&self, digest: ContentHash) -> PathBuf {
        self.key("images", digest)
    }

    pub(crate) fn open_image(&self, digest: ContentHash) -> io::Result<File> {
        let mut file = File::open(self.image_path(digest))?;
        let mut hash = Sha256::new();
        io::copy(&mut file, &mut hash)?;
        if <[u8; 32]>::from(hash.finalize()) != digest.0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "image digest"));
        }
        Ok(file)
    }

    pub(crate) fn collect(
        &self,
        assemblies: &BTreeSet<ContentHash>,
        images: &BTreeSet<ContentHash>,
    ) -> io::Result<u64> {
        let mut removed = 0;
        for (name, keep) in [("assemblies", assemblies), ("images", images)] {
            let dir = self.root.join(name);
            for row in fs::read_dir(&dir)? {
                let row = row?;
                if row.file_name().as_encoded_bytes().starts_with(b".tmp") {
                    fs::remove_file(row.path())?;
                    continue;
                }
                if let Some(name) = row.file_name().to_str()
                    && let Ok(hash) = crate::native::recipe_build::parse_root(name)
                    && !keep.contains(&hash)
                {
                    fs::remove_file(row.path())?;
                    removed += 1;
                }
            }
            sync_dir(&dir)?;
        }
        Ok(removed)
    }

    fn key(&self, directory: &str, hash: ContentHash) -> PathBuf {
        self.root.join(directory).join(crate::call::hex(&hash.0))
    }
}

fn read_hashes<const N: usize>(path: &Path) -> io::Result<[ContentHash; N]> {
    let mut file = File::open(path)?;
    let mut hashes = [ContentHash([0; 32]); N];
    for hash in &mut hashes {
        file.read_exact(&mut hash.0)?;
    }
    if file.read(&mut [0])? != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "record length"));
    }
    Ok(hashes)
}

fn write_record(path: &Path, bytes: &[u8], mode: u32, immutable: bool) -> io::Result<()> {
    let parent = path.parent().unwrap();
    if !immutable || !path.try_exists()? {
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(bytes)?;
        file.as_file()
            .set_permissions(Permissions::from_mode(mode))?;
        file.as_file().sync_all()?;
        if immutable {
            match file.persist_noclobber(path) {
                Ok(_) => (),
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.error),
            }
        } else {
            file.persist(path)?;
        }
    }
    sync_dir(parent)
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

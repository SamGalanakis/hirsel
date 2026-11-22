use crate::AssemblyRecord;
use hirsel_core_ids::ContentHash;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use twox_hash::XxHash3_64;

pub(super) type Attempt = [u8; 16];

pub(crate) struct Records {
    root: PathBuf,
    _owner: Option<File>,
}

#[derive(Clone, Copy)]
pub(super) struct Active {
    pub(super) store: Attempt,
    pub(super) version: u64,
    pub(super) assembly: ContentHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum State {
    Preparing = 1,
    Installing = 2,
    Refused = 3,
    Done = 4,
    Failed = 5,
}

pub(super) struct Deploy {
    pub(super) attempt: Attempt,
    pub(super) prior_activation: u64,
    pub(super) exported: u64,
    pub(super) old: ContentHash,
    pub(super) new: ContentHash,
    pub(super) assembly: ContentHash,
    pub(super) state: State,
}

const HEADER: &[u8; 10] = b"HIRDEPLY\x02\x00";
const RECORD_BYTES: usize = 10 + 16 + 16 + 3 * 32 + 1 + 8;
const PIN_HEADER: &[u8; 8] = b"HIRPIN\x01\x00";

impl Deploy {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECORD_BYTES);
        out.extend_from_slice(HEADER);
        out.extend_from_slice(&self.attempt);
        out.extend_from_slice(&self.prior_activation.to_le_bytes());
        out.extend_from_slice(&self.exported.to_le_bytes());
        out.extend_from_slice(&self.old.0);
        out.extend_from_slice(&self.new.0);
        out.extend_from_slice(&self.assembly.0);
        out.push(self.state as u8);
        out.extend_from_slice(&XxHash3_64::oneshot(&out).to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let data = bytes.get(..RECORD_BYTES - 8)?;
        let check = bytes.get(RECORD_BYTES - 8..)?;
        if !data.starts_with(HEADER) || XxHash3_64::oneshot(data).to_le_bytes() != check {
            return None;
        }
        let state = match data[138] {
            1 => State::Preparing,
            2 => State::Installing,
            3 => State::Refused,
            4 => State::Done,
            5 => State::Failed,
            _ => return None,
        };
        Some(Self {
            attempt: data[10..26].try_into().ok()?,
            prior_activation: u64::from_le_bytes(data[26..34].try_into().ok()?),
            exported: u64::from_le_bytes(data[34..42].try_into().ok()?),
            old: ContentHash(data[42..74].try_into().ok()?),
            new: ContentHash(data[74..106].try_into().ok()?),
            assembly: ContentHash(data[106..138].try_into().ok()?),
            state,
        })
    }
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
        for name in ["images", "assemblies", "pending", "activations", "pins"] {
            fs::create_dir_all(root.join(name))?;
        }
        sync_dir(&root)?;
        Ok(Self {
            root,
            _owner: Some(owner),
        })
    }

    pub(super) fn read_only(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            _owner: None,
        })
    }

    pub(super) fn write_deploy(&self, record: &Deploy) -> io::Result<()> {
        write_record(&self.root.join("deploy"), &record.encode(), 0o600, false)
    }

    pub(super) fn read_deploy(&self) -> io::Result<Option<Deploy>> {
        let mut bytes = Vec::with_capacity(RECORD_BYTES + 1);
        match File::open(self.root.join("deploy")) {
            Ok(file) => {
                file.take((RECORD_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        Ok(Deploy::decode(&bytes))
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

    pub(crate) fn verify_image(&self, digest: ContentHash) -> io::Result<PathBuf> {
        let path = self.image_path(digest);
        let _ = self.open_image(digest)?;
        Ok(path)
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

    pub(super) fn keep_pin(
        &self,
        id: Attempt,
        assembly: ContentHash,
        expires: u64,
    ) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(48);
        bytes.extend_from_slice(PIN_HEADER);
        bytes.extend_from_slice(&expires.to_le_bytes());
        bytes.extend_from_slice(&assembly.0);
        let path = self.root.join("pins").join(crate::call::hex(&id));
        write_record(&path, &bytes, 0o400, true)
    }

    pub(super) fn pin(&self, id: Attempt, now: u64) -> io::Result<ContentHash> {
        let path = self.root.join("pins").join(crate::call::hex(&id));
        let (key, expires) = read_pin(&path)?;
        if now >= expires {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(key)
    }

    pub(super) fn pending(&self, attempt: Attempt, assembly: ContentHash) -> io::Result<()> {
        write_record(&self.attempt_path(attempt), &assembly.0, 0o400, true)
    }

    pub(super) fn clear_pending(&self, attempt: Attempt) -> io::Result<()> {
        let path = self.attempt_path(attempt);
        match fs::remove_file(&path) {
            Ok(()) => sync_dir(path.parent().unwrap()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn reconcile_pending(&self) -> io::Result<()> {
        let dir = self.root.join("pending");
        for row in fs::read_dir(&dir)? {
            fs::remove_file(row?.path())?;
        }
        sync_dir(&dir)
    }

    pub(super) fn pending_assemblies(&self) -> io::Result<BTreeSet<ContentHash>> {
        let mut assemblies = BTreeSet::new();
        for item in fs::read_dir(self.root.join("pending"))? {
            let item = item?;
            if !item.file_name().as_encoded_bytes().starts_with(b".tmp") {
                let [assembly] = read_hashes(&item.path())?;
                assemblies.insert(assembly);
            }
        }
        Ok(assemblies)
    }

    pub(super) fn retire_activations(&self, store: Attempt, retain_from: u64) -> io::Result<()> {
        let prefix = format!("{}-", crate::call::hex(&store));
        let directory = self.root.join("activations");
        for item in fs::read_dir(&directory)? {
            let item = item?;
            if let Some(name) = item
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix(&prefix))
                && let Ok(version) = u64::from_str_radix(name, 16)
                && version < retain_from
            {
                fs::remove_file(item.path())?;
            }
        }
        sync_dir(&directory)
    }

    pub(super) fn roots(&self, now: u64) -> io::Result<BTreeSet<ContentHash>> {
        let mut roots = BTreeSet::new();
        for directory in ["activations", "pending", "pins"] {
            let path = self.root.join(directory);
            for row in fs::read_dir(&path)? {
                let row = row?;
                if row.file_name().as_encoded_bytes().starts_with(b".tmp") {
                    fs::remove_file(row.path())?;
                    continue;
                }
                let hash = if directory == "pins" {
                    let (key, expires) = read_pin(&row.path())?;
                    if now >= expires {
                        fs::remove_file(row.path())?;
                        continue;
                    }
                    key
                } else {
                    let [hash] = read_hashes(&row.path())?;
                    hash
                };
                roots.insert(hash);
            }
            sync_dir(&path)?;
        }
        Ok(roots)
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

    pub(super) fn index(
        &self,
        store: Attempt,
        version: u64,
        record: ContentHash,
    ) -> io::Result<()> {
        let path = self.activation_path(store, version);
        match read_hashes::<1>(&path) {
            Ok([current]) if current == record => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "activation already names another assembly",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_record(&path, &record.0, 0o400, true)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn indexed(&self, store: Attempt, version: u64) -> io::Result<Option<ContentHash>> {
        match read_hashes::<1>(&self.activation_path(store, version)) {
            Ok([key]) => Ok(Some(key)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn activation_path(&self, store: Attempt, version: u64) -> PathBuf {
        self.root
            .join("activations")
            .join(format!("{}-{version:016x}", crate::call::hex(&store)))
    }

    pub(super) fn set_active(&self, active: Active) -> io::Result<()> {
        self.index(active.store, active.version, active.assembly)?;
        let mut bytes = Vec::with_capacity(56);
        bytes.extend_from_slice(&active.store);
        bytes.extend_from_slice(&active.version.to_le_bytes());
        bytes.extend_from_slice(&active.assembly.0);
        write_record(&self.root.join("active"), &bytes, 0o600, false)
    }

    pub(super) fn active(&self) -> io::Result<Option<Active>> {
        let path = self.root.join("active");
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = [0u8; 56];
        file.read_exact(&mut bytes)?;
        if file.read(&mut [0])? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "active record length",
            ));
        }
        Ok(Some(Active {
            store: bytes[..16].try_into().unwrap(),
            version: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            assembly: ContentHash(bytes[24..].try_into().unwrap()),
        }))
    }

    fn attempt_path(&self, attempt: Attempt) -> PathBuf {
        self.root.join("pending").join(crate::call::hex(&attempt))
    }

    fn key(&self, directory: &str, hash: ContentHash) -> PathBuf {
        self.root.join(directory).join(crate::call::hex(&hash.0))
    }
}

fn read_pin(path: &Path) -> io::Result<(ContentHash, u64)> {
    let mut record = [0; 48];
    let mut file = File::open(path)?;
    file.read_exact(&mut record)?;
    if &record[..8] != PIN_HEADER || file.read(&mut [0])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recording pin format",
        ));
    }
    let expires = u64::from_le_bytes(record[8..16].try_into().unwrap());
    Ok((ContentHash(record[16..].try_into().unwrap()), expires))
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

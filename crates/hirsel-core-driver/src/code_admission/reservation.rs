use super::{AdmissionError, Limit, ObjectKey, ObjectKind};
use crate::code_store::CodeStore;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::decode::DecodeBudget;
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

pub(super) struct ListedObject {
    pub(super) len: u64,
    present: bool,
    pub(super) received: bool,
    pub(super) requested: bool,
}

impl ListedObject {
    pub(super) fn new(len: u64) -> Self {
        Self {
            len,
            present: false,
            received: false,
            requested: false,
        }
    }

    pub(super) fn needed(&self) -> bool {
        !self.present && !self.received
    }
}

pub(super) type Listing = BTreeMap<ObjectKey, ListedObject>;

/// The live reservations for a host-owned code store. The host must use this
/// owner for collection, and keep the contents of published objects immutable.
pub struct AdmissionStore {
    store: CodeStore,
    staging: PathBuf,
    max_staging: u64,
    collection: Mutex<Collection>,
    _owner: File,
}

#[derive(Default)]
struct Collection {
    pins: BTreeMap<ObjectKey, usize>,
    staged: u64,
    staging_error: Option<io::Error>,
}

impl AdmissionStore {
    /// Open the host's private code store and discard staging left by a dead
    /// host. The owner lock excludes a second live host across processes.
    pub fn open(root: &Path, max_staging: u64) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join(".admission.lock"))?;
        owner.try_lock()?;

        let staging = root.join("staging");
        match fs::remove_dir_all(&staging) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        DirBuilder::new().mode(0o700).create(&staging)?;
        Ok(Self {
            store: CodeStore::new(root),
            staging,
            max_staging,
            collection: Mutex::default(),
            _owner: owner,
        })
    }

    /// Remove an otherwise unrooted object if no admission leases it. The
    /// caller is responsible for computing the host's durable roots.
    pub fn remove_unreserved(&self, key: ObjectKey) -> io::Result<bool> {
        let collection = self.collection();
        if collection.pins.contains_key(&key) {
            return Ok(false);
        }
        match fs::remove_file(self.store.path(key.kind, key.hash)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn collection(&self) -> MutexGuard<'_, Collection> {
        self.collection.lock().expect("code collection lock")
    }

    pub(crate) fn under_collection<R>(
        &self,
        action: impl FnOnce(&dyn Fn(ObjectKey) -> bool) -> R,
    ) -> R {
        let collection = self.collection();
        action(&|key| collection.pins.contains_key(&key))
    }

    pub(super) fn reserve(
        &self,
        root: ContentHash,
        mut objects: Listing,
        max_outstanding: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Reservation<'_>, AdmissionError> {
        let mut collection = self.collection();
        if let Some(error) = &collection.staging_error {
            return Err(AdmissionError::Io(io::Error::new(
                error.kind(),
                "admission staging cleanup failed",
            )));
        }

        // Pin under the same lock as collection. The presence snapshot and the
        // pins become visible together, before the caller can send Need.
        for (&key, row) in &mut objects {
            budget.check()?;
            match self.store.open(key.kind, key.hash) {
                Ok(_) => row.present = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(AdmissionError::Io(error)),
            }
        }
        budget.charge_bytes(
            objects
                .len()
                .checked_add(1)
                .and_then(|n| n.checked_mul(size_of::<(ObjectKey, usize)>() * 4))
                .ok_or(AdmissionError::Limit(Limit::DecodeBytes))?,
        )?;
        // Collection sees the same lease after the recipe and its staged
        // objects are published, until the host has published durable roots.
        let recipe = ObjectKey {
            kind: ObjectKind::Recipe,
            hash: root,
        };
        for &key in std::iter::once(&recipe).chain(objects.keys()) {
            *collection.pins.entry(key).or_default() += 1;
        }
        drop(collection);

        let mut reservation = Reservation {
            owner: self,
            recipe,
            objects,
            staging: None,
            bytes: 0,
        };
        let (count, bytes) = reservation
            .objects
            .values()
            .filter(|row| row.needed())
            .try_fold((0, 0_u64), |(count, bytes), row| {
                Ok::<_, AdmissionError>((
                    count + 1,
                    bytes
                        .checked_add(row.len)
                        .ok_or(AdmissionError::Limit(Limit::StagingBytes))?,
                ))
            })?;
        if count > max_outstanding {
            return Err(AdmissionError::Limit(Limit::Outstanding));
        }
        if count != 0 {
            {
                let mut collection = self.collection();
                if let Some(error) = &collection.staging_error {
                    return Err(AdmissionError::Io(io::Error::new(
                        error.kind(),
                        "admission staging cleanup failed",
                    )));
                }
                collection.staged = collection
                    .staged
                    .checked_add(bytes)
                    .filter(|&total| total <= self.max_staging)
                    .ok_or(AdmissionError::Limit(Limit::StagingBytes))?;
                reservation.bytes = bytes;
            }
            reservation.staging = Some(
                tempfile::Builder::new()
                    .permissions(Permissions::from_mode(0o700))
                    .tempdir_in(&self.staging)?,
            );
        }
        budget.check()?;
        Ok(reservation)
    }
}

pub(super) struct Reservation<'a> {
    owner: &'a AdmissionStore,
    recipe: ObjectKey,
    pub(super) objects: Listing,
    staging: Option<TempDir>,
    bytes: u64,
}

impl Reservation<'_> {
    pub(super) fn staged_path(&self, key: ObjectKey) -> Option<PathBuf> {
        self.staging.as_ref().map(|staging| {
            let kind = match key.kind {
                ObjectKind::Group => "group",
                ObjectKind::Body => "body",
                ObjectKind::Annex => "annex",
                ObjectKind::ForeignArchive => "archive",
                ObjectKind::Recipe => unreachable!("recipes are in the request header"),
            };
            staging
                .path()
                .join(format!("{kind}-{}", crate::call::hex(&key.hash.0)))
        })
    }

    pub(super) fn open(&self, key: ObjectKey) -> io::Result<File> {
        if self.objects[&key].received {
            File::open(self.staged_path(key).expect("received in staging"))
        } else {
            self.owner.store.open(key.kind, key.hash)
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let cleanup = self.staging.take().map(TempDir::close).transpose();
        let mut collection = self.owner.collection();
        match cleanup {
            Ok(_) => collection.staged -= self.bytes,
            Err(error) => collection.staging_error = Some(error),
        }
        for key in std::iter::once(&self.recipe).chain(self.objects.keys()) {
            let count = collection.pins.get_mut(key).expect("object pin");
            *count -= 1;
            if *count == 0 {
                collection.pins.remove(key);
            }
        }
    }
}

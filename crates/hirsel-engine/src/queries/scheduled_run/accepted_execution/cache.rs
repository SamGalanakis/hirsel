//! Optional durable cache backing and same-user directory lifetimes.
use super::*;
use hirsel_artifact_store::{Bytes, ContentReadError, Contents};
use std::{
    fs, io,
    path::Path,
    time::{Duration, SystemTime},
};

pub(super) enum PublicationError {
    Store(io::Error),
    Query(QueryError),
}
impl From<QueryError> for PublicationError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}
impl std::fmt::Display for PublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "storage {:?}: {error}", error.kind()),
            Self::Query(error) => write!(f, "{error:?}"),
        }
    }
}
impl PublicationError {
    pub(super) fn is_store_failure(&self) -> bool {
        matches!(self, Self::Store(_))
    }
    pub(super) fn into_query(self) -> QueryError {
        match self {
            Self::Query(error) => error,
            error => failure(error.to_string()),
        }
    }
}
/// TypeRecords' Option codec API must not erase a failed physical write. One
/// publication owns this adapter and retains its first I/O cause through encoding.
pub(super) struct EncodingStore {
    store: Store,
    error: Mutex<Option<io::Error>>,
}
impl EncodingStore {
    pub(super) fn new(store: Store) -> Self {
        Self {
            store,
            error: Mutex::new(None),
        }
    }
    pub(super) fn required<T>(
        &self,
        value: Option<T>,
        codec: impl FnOnce() -> String,
    ) -> Result<T, PublicationError> {
        value.ok_or_else(
            || match self.error.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(error) => PublicationError::Store(error),
                None => PublicationError::Query(failure(codec())),
            },
        )
    }
}
impl Contents for EncodingStore {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        self.store.get_content(reference)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        match self.store.put(domain, bytes) {
            Ok(reference) => Some(reference),
            Err(error) => {
                self.error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_insert(error);
                None
            }
        }
    }
}
const MARKER: &str = "last-used";
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[cfg(unix)]
fn owned(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no arguments or memory preconditions.
    metadata.uid() == unsafe { libc::geteuid() }
}
#[cfg(not(unix))]
fn owned(_: &fs::Metadata) -> bool {
    true
}
fn marker(directory: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(directory.join(MARKER))?;
    if !owned(&file.metadata()?) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}
fn lock(file: &fs::File, exclusive: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mode = if exclusive {
            libc::LOCK_EX | libc::LOCK_NB
        } else {
            libc::LOCK_SH
        };
        // SAFETY: the borrowed file owns this live descriptor for the call.
        if unsafe { libc::flock(file.as_raw_fd(), mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(unix))]
    let _ = (file, exclusive);
    Ok(())
}
fn same_user_tree(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !owned(&metadata) {
        return Ok(false);
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            if !same_user_tree(&entry?.path())? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn sweep(base: &Path, current: &str, now: SystemTime) -> io::Result<()> {
    // On platforms without the Unix owner/lock checks, leave cleanup to the host.
    if !cfg!(unix) {
        return Ok(());
    }
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_name() == current {
            continue;
        }
        let directory = entry.path();
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || !owned(&metadata) {
            continue;
        }
        // A missing marker starts its seven-day grace period now, including v1.
        let Ok(file) = marker(&directory) else {
            continue;
        };
        if lock(&file, true).is_err() {
            continue;
        }
        let old = file.metadata()?.modified()?;
        if now.duration_since(old).unwrap_or_default() < MAX_AGE {
            continue;
        }
        if same_user_tree(&directory)? {
            let _ = fs::remove_dir_all(&directory);
        }
    }
    Ok(())
}
pub(super) fn open(base: &Path, version: &str) -> io::Result<Store> {
    let directory = base.join(version);
    fs::create_dir_all(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || !owned(&metadata) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let lease = marker(&directory)?;
    lock(&lease, false)?;
    lease.set_times(fs::FileTimes::new().set_modified(SystemTime::now()))?;
    let _ = sweep(base, version, SystemTime::now());
    Ok(Store::new(directory).retain_owner(Arc::new(lease)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fatal_type_publication_preserves_the_store_error_kind() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("not-a-directory");
        fs::write(&root, b"occupied").unwrap();
        let backing = Arc::new(EncodingStore::new(Store::new(root)));
        let types = TypeInterner::new(None);
        let unit = types.intern(hirsel_types::Type::unit());
        let records = TypeRecords::new(types, backing.clone());
        let error = backing.required(records.encode(&[unit]), || "codec error".into());
        match error {
            Err(PublicationError::Store(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::AlreadyExists)
            }
            _ => panic!("the fatal path must retain the physical write error"),
        }
    }
    #[test]
    #[cfg(unix)]
    fn sweep_removes_old_siblings_and_keeps_recent_and_live_stores() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let old = open(base, "old").unwrap();
        let fresh = open(base, "fresh").unwrap();
        let live = open(base, "live").unwrap();
        let expired = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        for name in ["old", "live"] {
            marker(&base.join(name))
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(expired))
                .unwrap();
        }
        fs::create_dir(base.join("v1")).unwrap();
        drop(old);
        let _current = open(base, "current").unwrap();
        assert!(!base.join("old").exists());
        assert!(base.join("fresh").exists());
        assert!(
            base.join("live").exists(),
            "an active reader retains its directory"
        );
        assert!(
            base.join("v1").join(MARKER).exists(),
            "legacy data starts a grace period"
        );
        drop((fresh, live));
    }
}

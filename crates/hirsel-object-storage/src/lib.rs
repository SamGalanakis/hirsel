//! Object storage: objects by key, written create-if-absent or replaced
//! conditionally on their entity tag. This is the storage seam the code store
//! and `authority.objects@1` share: [`ObjectStorage`] is a conditional PUT
//! (`If-None-Match: *`), a conditional replace (`If-Match`), a GET with the
//! object's tag, a listing and a delete, which a directory, an S3-compatible
//! bucket and a simulation all implement.

#[cfg(test)]
hirsel_guard::install!();
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(feature = "testing")]
pub mod fake_s3;
mod s3;
pub use s3::S3Objects;

/// The outcome of [`ObjectStorage::put_new`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Put {
    /// The object did not exist and now holds the bytes.
    Created,
    /// An object already existed under the key; it is unchanged, whatever
    /// its bytes.
    Exists,
}

/// An object's entity tag. Equal bytes have equal tags under one storage, as an
/// S3 single-part ETag does, so a tag names a content, not a write: a caller
/// that replaces conditionally must never write bytes a key held before.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Tag(pub String);

/// The condition of [`ObjectStorage::replace`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Condition {
    /// Only when no object exists under the key (`If-None-Match: *`).
    Absent,
    /// Only when the object's tag is this one (`If-Match`).
    Matches(Tag),
}

/// The outcome of [`ObjectStorage::replace`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Replaced {
    /// The object now holds the bytes and has this tag.
    Replaced(Tag),
    /// The condition did not hold; nothing changed.
    Mismatch,
}

/// One page of a listing: keys in ascending order, and whether more follow
/// the last one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Listing {
    pub keys: Vec<String>,
    pub truncated: bool,
}

/// The largest page a listing returns, as an S3 `ListObjectsV2` does.
pub const LIST_PAGE: usize = 1000;

/// The tag of `bytes` for storages that tag by content: the hexadecimal
/// SHA-256 digest.
pub fn content_tag(bytes: &[u8]) -> Tag {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    Tag(out)
}

/// Why an object operation did not complete. A failed `put_new` may or may
/// not have created the object; a later `get` or `put_new` says which.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageError {
    /// The key is not a valid object key.
    Key(String),
    /// The storage did not answer (a transient failure).
    Unavailable(String),
    /// Any other failure of the medium.
    Io(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(key) => write!(f, "invalid object key {key:?}"),
            Self::Unavailable(message) => write!(f, "object storage unavailable: {message}"),
            Self::Io(message) => write!(f, "object storage failed: {message}"),
        }
    }
}

impl std::error::Error for StorageError {}

/// Objects by key. Keys are `/`-separated segments of lowercase ASCII
/// letters, digits, `.`, `_` and `-`, none empty or starting with `.`
/// ([`check_key`]). Every write is whole or not at all and durable before it
/// returns; a failed write may or may not have taken effect.
pub trait ObjectStorage: Send + Sync {
    /// The object under `key` and its tag, if one exists.
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError>;
    /// The object under `key`, if one exists.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.get_tagged(key)?.map(|(bytes, _)| bytes))
    }
    /// Each of `keys`, as [`get`](Self::get) would return it. A storage
    /// whose requests have latency issues them in parallel.
    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        keys.iter().map(|k| self.get(k)).collect()
    }
    /// Create the object under `key` with `bytes` when none exists; an
    /// existing object is left unchanged.
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError>;
    /// Write `bytes` under `key` only when `condition` holds, atomically with
    /// the check.
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError>;
    /// The keys starting with `prefix` and greater than `after`, ascending,
    /// at most [`LIST_PAGE`] of them. A listing may be incomplete: callers
    /// treat it as a hint and never conclude from a key's absence.
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError>;
    /// Remove the object under `key`; removing an absent one is success.
    fn delete(&self, key: &str) -> Result<(), StorageError>;
}

/// Refuse a key outside the object key grammar.
pub fn check_key(key: &str) -> Result<(), StorageError> {
    let segment = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
    };
    if key.split('/').all(segment) {
        Ok(())
    } else {
        Err(StorageError::Key(key.into()))
    }
}

/// Objects as files under a directory (`objects.dir@1`). A put writes a
/// temporary file, syncs it, hard-links it to its key (which fails when the
/// key exists, so creation is atomic and never replaces) and syncs its
/// directory and every one above it up to the root, so directories the put
/// created are durable too. A put that finds the key taken runs the same
/// directory barrier before returning `Exists`, so the existing entry is
/// durable whoever linked it. A replace holds the root's `.lock` file lock
/// (an open-file-description lock, so every handle in every process
/// excludes the others), compares the current content's tag, then writes a
/// synced temporary file and renames it over the key. Tags are content
/// digests ([`content_tag`]).
#[derive(Clone, Debug)]
pub struct DirObjects {
    root: PathBuf,
}

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

impl DirObjects {
    /// Objects under `root`, which is created when absent.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| io(&root, e))?;
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Sync the directory holding `path` and each one above it through the
    /// root.
    fn sync_directories(&self, path: &Path) -> Result<(), StorageError> {
        let mut directory = path.parent();
        while let Some(dir) = directory {
            File::open(dir)
                .and_then(|handle| handle.sync_all())
                .map_err(|e| io(dir, e))?;
            if dir == self.root {
                break;
            }
            directory = dir.parent();
        }
        Ok(())
    }
}

fn io(path: &Path, error: std::io::Error) -> StorageError {
    StorageError::Io(format!("{}: {error}", path.display()))
}

impl DirObjects {
    /// Hold the root's replace lock until the returned file drops.
    fn lock(&self) -> Result<File, StorageError> {
        let path = self.root.join(".lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| io(&path, e))?;
        // SAFETY: flock on a descriptor this function owns; dropping the
        // file closes it and releases the lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io(&path, std::io::Error::last_os_error()));
        }
        Ok(file)
    }

    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        check_key(key)?;
        let path = self.root.join(key);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                Ok(None)
            }
            Err(e) => Err(io(&path, e)),
        }
    }

    fn replace_checked(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
        after_check: impl FnOnce(),
    ) -> Result<Replaced, StorageError> {
        check_key(key)?;
        let _lock = self.lock()?;
        let current = self.read(key)?.map(|b| content_tag(&b));
        let holds = match &condition {
            Condition::Absent => current.is_none(),
            Condition::Matches(tag) => current.as_ref() == Some(tag),
        };
        if !holds {
            return Ok(Replaced::Mismatch);
        }
        after_check();
        let path = self.root.join(key);
        let parent = path
            .parent()
            .expect("an object key names a file under the root");
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        let temporary = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = File::create_new(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&temporary);
            return Err(io(&path, e));
        }
        self.sync_directories(&path)?;
        Ok(Replaced::Replaced(content_tag(bytes)))
    }

    /// Every key under the directory `dir`, whose key prefix is `base`.
    fn walk(&self, dir: &Path, base: &str, out: &mut Vec<String>) -> Result<(), StorageError> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                return Ok(());
            }
            Err(e) => return Err(io(dir, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| io(dir, e))?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let key = if base.is_empty() {
                name
            } else {
                format!("{base}/{name}")
            };
            let kind = entry.file_type().map_err(|e| io(dir, e))?;
            if kind.is_dir() {
                self.walk(&entry.path(), &key, out)?;
            } else if kind.is_file() {
                out.push(key);
            }
        }
        Ok(())
    }
}

impl ObjectStorage for DirObjects {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        Ok(self.read(key)?.map(|bytes| {
            let tag = content_tag(&bytes);
            (bytes, tag)
        }))
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        self.replace_checked(key, bytes, condition, || {})
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        // Walk only the directory the prefix's complete segments name.
        let (dir, base) = match prefix.rfind('/') {
            Some(i) => (self.root.join(&prefix[..i]), &prefix[..i]),
            None => (self.root.clone(), ""),
        };
        let mut keys = Vec::new();
        self.walk(&dir, base, &mut keys)?;
        keys.retain(|k| k.starts_with(prefix) && k.as_str() > after);
        keys.sort();
        let truncated = keys.len() > LIST_PAGE;
        keys.truncate(LIST_PAGE);
        Ok(Listing { keys, truncated })
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        check_key(key)?;
        let path = self.root.join(key);
        match fs::remove_file(&path) {
            Ok(()) => self.sync_directories(&path),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(&path, e)),
        }
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        check_key(key)?;
        let path = self.root.join(key);
        let parent = path
            .parent()
            .expect("an object key names a file under the root");
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        if path.exists() {
            self.sync_directories(&path)?;
            return Ok(Put::Exists);
        }
        // A leading '.' keeps temporary names out of the key space.
        let temporary = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = File::create_new(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&temporary);
            return Err(io(&temporary, e));
        }
        let linked = fs::hard_link(&temporary, &path);
        let _ = fs::remove_file(&temporary);
        match linked {
            Ok(()) => {
                self.sync_directories(&path)?;
                Ok(Put::Created)
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                self.sync_directories(&path)?;
                Ok(Put::Exists)
            }
            Err(e) => Err(io(&path, e)),
        }
    }
}

/// In-memory objects with an S3 bucket's conditional-write semantics
/// (`objects.sim@1`), optionally failing operations at seeded points. A
/// faulted write fails before or after its effect, as a lost reply does, so
/// callers see both outcomes of an uncertain write; a faulted listing fails
/// or returns every other key.
///
/// The objects also cut their client at a chosen request, for tests that must
/// reach every request in turn ([`Cuts`]). Requests (each get, put, replace,
/// listing and delete, one per key) are numbered from 1.
#[derive(Debug, Default)]
pub struct MemoryObjects {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    faults: Mutex<Option<Faults>>,
    cuts: Mutex<Cutting>,
    foreign: Mutex<Option<Foreign>>,
}

/// What [`Cuts::claim_at`] runs: another client's claim of the history.
#[derive(Clone)]
struct Foreign(Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for Foreign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Foreign")
    }
}

/// The requests (numbered from 1) at which [`MemoryObjects`] cuts its client.
/// An object write is whole or absent, so no cut tears one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cuts {
    /// Answer a definite refusal ([`StorageError::Io`]), with no effect.
    pub fail_at: Option<u64>,
    /// Lose the request before it reaches the objects: no effect, and the
    /// client hears [`StorageError::Unavailable`].
    pub lose_unapplied_at: Option<u64>,
    /// Take the request's effect and lose its reply: the client hears
    /// [`StorageError::Unavailable`].
    pub lose_response_at: Option<u64>,
    /// The client dies right after the request: every later request fails
    /// with no effect, uncounted, until [`MemoryObjects::restart`].
    pub crash_at: Option<u64>,
    /// Hold the request's write back: the client hears
    /// [`StorageError::Unavailable`] at once, and the write (its condition
    /// checked then) takes effect just before request `defer_apply_at +
    /// defer_for` is served, as an unanswered request that lands late does. A
    /// read held back has no effect.
    pub defer_apply_at: Option<u64>,
    /// How many requests later a held-back write takes effect; 1 when absent.
    pub defer_for: Option<u64>,
    /// Another client claims the history just before the request is served:
    /// the claim [`MemoryObjects::on_claim`] installs runs first, its own
    /// requests counted, so a claim can land inside one advance.
    pub claim_at: Option<u64>,
}

/// The names of the [`Cuts`] members in an `objects.sim@1` entry.
pub const CUTS: [&str; 7] = [
    "claim_at",
    "crash_at",
    "defer_apply_at",
    "defer_for",
    "fail_at",
    "lose_response_at",
    "lose_unapplied_at",
];

#[derive(Debug, Default)]
struct Cutting {
    at: Cuts,
    requests: u64,
    down: bool,
    /// A held-back write and the request it takes effect before.
    held: Option<(u64, Held)>,
}

/// A write a cut holds back.
#[derive(Debug)]
enum Held {
    Put {
        key: String,
        bytes: Vec<u8>,
    },
    Replace {
        key: String,
        bytes: Vec<u8>,
        condition: Condition,
    },
    Delete {
        key: String,
    },
}

impl Held {
    fn apply(self, objects: &mut BTreeMap<String, Vec<u8>>) {
        match self {
            Held::Put { key, bytes } => {
                objects.entry(key).or_insert(bytes);
            }
            Held::Replace {
                key,
                bytes,
                condition,
            } => {
                let current = objects.get(&key).map(|b| content_tag(b));
                let holds = match &condition {
                    Condition::Absent => current.is_none(),
                    Condition::Matches(tag) => current.as_ref() == Some(tag),
                };
                if holds {
                    objects.insert(key, bytes);
                }
            }
            Held::Delete { key } => {
                objects.remove(&key);
            }
        }
    }
}

/// What a cut does to one request.
enum Fate {
    /// Serve the request; answer it unless `lost`.
    Serve { lost: bool },
    /// No effect; answer this error.
    Fail(fn(String) -> StorageError),
    /// Hold the write back until request `until`; answer Unavailable.
    Defer { until: u64 },
}

#[derive(Debug)]
struct Faults {
    state: u64,
    one_in: u64,
}

impl Faults {
    /// SplitMix64.
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl MemoryObjects {
    pub fn new() -> Self {
        Self::default()
    }
    /// Objects of which each operation fails with probability `1/one_in`,
    /// decided by a SplitMix64 stream from `seed`; `one_in` 0 never fails.
    pub fn with_faults(seed: u64, one_in: u64) -> Self {
        let objects = Self::new();
        if one_in > 0 {
            *objects.faults.lock().expect("faults") = Some(Faults {
                state: seed,
                one_in,
            });
        }
        objects
    }
    /// Objects that cut their client at the requests `at` names.
    pub fn with_cuts(mut self, at: Cuts) -> Self {
        self.cuts.get_mut().expect("cuts").at = at;
        self
    }
    /// The claim [`Cuts::claim_at`] runs.
    pub fn on_claim(&self, claim: Arc<dyn Fn() + Send + Sync>) {
        *self.foreign.lock().expect("foreign") = Some(Foreign(claim));
    }
    /// Start a new client after a crash: true when one had happened, and
    /// requests are served again.
    pub fn restart(&self) -> bool {
        std::mem::take(&mut self.cuts.lock().expect("cuts").down)
    }
    /// Count a request and say what the cuts make of it. A held-back write
    /// due before this request takes effect first.
    fn fate(&self) -> Fate {
        let mut c = self.cuts.lock().expect("cuts");
        if c.down {
            return Fate::Fail(StorageError::Unavailable);
        }
        c.requests += 1;
        let (n, at) = (Some(c.requests), c.at);
        if at.claim_at == n {
            drop(c);
            let foreign = self.foreign.lock().expect("foreign").clone();
            if let Some(Foreign(claim)) = foreign {
                claim();
            }
            c = self.cuts.lock().expect("cuts");
        }
        if c.held.as_ref().is_some_and(|(until, _)| Some(*until) == n) {
            let (_, write) = c.held.take().expect("held");
            write.apply(&mut self.objects.lock().expect("objects"));
        }
        c.down = at.crash_at == n;
        if at.defer_apply_at == n {
            Fate::Defer {
                until: c.requests + at.defer_for.unwrap_or(1),
            }
        } else if at.fail_at == n {
            Fate::Fail(StorageError::Io)
        } else if at.lose_unapplied_at == n {
            Fate::Fail(StorageError::Unavailable)
        } else {
            Fate::Serve {
                lost: at.lose_response_at == n,
            }
        }
    }
    /// The next fault decision: None, or whether the faulted operation's
    /// effect still happens. A cut decides first.
    fn fault(
        &self,
        operation: &str,
        key: &str,
        write: impl FnOnce() -> Option<Held>,
    ) -> Result<Option<bool>, StorageError> {
        match self.fate() {
            Fate::Fail(error) => return Err(error(format!("cut at {operation} {key}"))),
            Fate::Defer { until } => {
                if let Some(w) = write() {
                    self.cuts.lock().expect("cuts").held = Some((until, w));
                }
                return Err(StorageError::Unavailable(format!(
                    "cut at {operation} {key}: held back"
                )));
            }
            Fate::Serve { lost: true } => return Ok(Some(true)),
            Fate::Serve { lost: false } => {}
        }
        let mut faults = self.faults.lock().expect("faults");
        let Some(faults) = faults.as_mut() else {
            return Ok(None);
        };
        let draw = faults.next();
        Ok((draw % faults.one_in == 0).then_some(draw & (1 << 63) != 0))
    }
    /// Replace an object's bytes, as a hostile or broken medium would.
    pub fn overwrite(&self, key: &str, bytes: Vec<u8>) {
        self.objects
            .lock()
            .expect("objects")
            .insert(key.into(), bytes);
    }
    /// Every object, read past any fault.
    pub fn contents(&self) -> BTreeMap<String, Vec<u8>> {
        self.objects.lock().expect("objects").clone()
    }
}

fn unavailable(operation: &str, key: &str) -> StorageError {
    StorageError::Unavailable(format!("simulated fault on {operation} {key}"))
}

impl ObjectStorage for MemoryObjects {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        check_key(key)?;
        if self.fault("get", key, || None)?.is_some() {
            return Err(unavailable("get", key));
        }
        Ok(self
            .objects
            .lock()
            .expect("objects")
            .get(key)
            .map(|b| (b.clone(), content_tag(b))))
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        check_key(key)?;
        let fault = self.fault("replace", key, || {
            Some(Held::Replace {
                key: key.into(),
                bytes: bytes.to_vec(),
                condition: condition.clone(),
            })
        })?;
        if fault == Some(false) {
            return Err(unavailable("replace", key));
        }
        let mut objects = self.objects.lock().expect("objects");
        let current = objects.get(key).map(|b| content_tag(b));
        let holds = match &condition {
            Condition::Absent => current.is_none(),
            Condition::Matches(tag) => current.as_ref() == Some(tag),
        };
        let replaced = if holds {
            objects.insert(key.into(), bytes.to_vec());
            Replaced::Replaced(content_tag(bytes))
        } else {
            Replaced::Mismatch
        };
        match fault {
            Some(_) => Err(unavailable("replace reply", key)),
            None => Ok(replaced),
        }
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        let fault = self.fault("list", prefix, || None)?;
        if fault == Some(false) {
            return Err(unavailable("list", prefix));
        }
        let objects = self.objects.lock().expect("objects");
        let mut keys: Vec<String> = objects
            .keys()
            .filter(|k| k.starts_with(prefix) && k.as_str() > after)
            .take(LIST_PAGE + 1)
            .cloned()
            .collect();
        let truncated = keys.len() > LIST_PAGE;
        keys.truncate(LIST_PAGE);
        if fault.is_some() {
            // An incomplete listing: every other key is missing.
            keys = keys.into_iter().step_by(2).collect();
        }
        Ok(Listing { keys, truncated })
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        check_key(key)?;
        let fault = self.fault("delete", key, || Some(Held::Delete { key: key.into() }))?;
        if fault == Some(false) {
            return Err(unavailable("delete", key));
        }
        self.objects.lock().expect("objects").remove(key);
        match fault {
            Some(_) => Err(unavailable("delete reply", key)),
            None => Ok(()),
        }
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        check_key(key)?;
        let fault = self.fault("put", key, || {
            Some(Held::Put {
                key: key.into(),
                bytes: bytes.to_vec(),
            })
        })?;
        if fault == Some(false) {
            return Err(unavailable("put", key));
        }
        let mut objects = self.objects.lock().expect("objects");
        let put = if objects.contains_key(key) {
            Put::Exists
        } else {
            objects.insert(key.into(), bytes.to_vec());
            Put::Created
        };
        match fault {
            Some(_) => Err(unavailable("put reply", key)),
            None => Ok(put),
        }
    }
}

/// Open an object storage provider entry: `{"provider": "objects.dir@1",
/// "path": "<absolute directory>"}`, `{"provider": "objects.sim@1",
/// "seed": <u64>, "fail_one_in": <u64>}` (both members optional, default
/// 0) or `{"provider": "objects.s3@1", "endpoint": "http[s]://host[:port]",
/// "bucket": .., "region": .., "prefix": .., "credentials": "<absolute JSON
/// file>", "roots": "<absolute PEM file>", "connect_timeout_ms": ..,
/// "request_timeout_ms": ..}` (`prefix`, `roots` and the timeouts optional; see
/// [`S3Objects`]). Unknown members and providers are refused.
pub fn open(entry: &serde_json::Value) -> Result<Arc<dyn ObjectStorage>, String> {
    Ok(match provider(entry)? {
        Provider::Dir(objects) => Arc::new(objects),
        Provider::S3(objects) => Arc::new(objects),
        Provider::Sim(objects) => Arc::new(objects),
    })
}

/// The provider an object storage entry opens, by kind.
pub enum Provider {
    Dir(DirObjects),
    S3(S3Objects),
    /// Simulated objects, which a host may also cut and restart the client of.
    Sim(MemoryObjects),
}

/// Open an object storage entry as [`open`] does, keeping the provider's kind.
pub fn provider(entry: &serde_json::Value) -> Result<Provider, String> {
    let members = entry
        .as_object()
        .ok_or("an object storage entry is a JSON object")?;
    let provider = members
        .get("provider")
        .and_then(|p| p.as_str())
        .ok_or("an object storage entry names its provider")?;
    let allowed: &[&str] = match provider {
        "objects.dir@1" => &["provider", "path"],
        "objects.s3@1" => s3::MEMBERS,
        "objects.sim@1" => &[
            "provider",
            "seed",
            "fail_one_in",
            "crash_at",
            "fail_at",
            "lose_response_at",
            "lose_unapplied_at",
            "defer_apply_at",
            "defer_for",
            "claim_at",
        ],
        other => return Err(format!("unsupported object storage provider {other}")),
    };
    if let Some(unknown) = members.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("{provider} has no member {unknown:?}"));
    }
    if provider == "objects.dir@1" {
        let path = members
            .get("path")
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("objects.dir@1 path is an absolute directory")?;
        return Ok(Provider::Dir(
            DirObjects::open(path).map_err(|e| e.to_string())?,
        ));
    }
    if provider == "objects.s3@1" {
        return Ok(Provider::S3(S3Objects::open(members)?));
    }
    let uint = |name: &str| match members.get(name) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("{provider} {name} is an unsigned integer")),
    };
    let request = |name: &str| match uint(name)? {
        0 if members.contains_key(name) => Err(format!("{provider} {name} counts requests from 1")),
        0 => Ok(None),
        n => Ok(Some(n)),
    };
    let cuts = Cuts {
        fail_at: request("fail_at")?,
        lose_unapplied_at: request("lose_unapplied_at")?,
        lose_response_at: request("lose_response_at")?,
        crash_at: request("crash_at")?,
        defer_apply_at: request("defer_apply_at")?,
        defer_for: request("defer_for")?,
        claim_at: request("claim_at")?,
    };
    Ok(Provider::Sim(
        MemoryObjects::with_faults(uint("seed")?, uint("fail_one_in")?).with_cuts(cuts),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_check_and_publish_exclude_a_competing_writer() {
        let directory = tempfile::tempdir().unwrap();
        let objects = DirObjects::open(directory.path()).unwrap();
        File::create(directory.path().join(".lock")).unwrap();
        objects.replace("head", b"old", Condition::Absent).unwrap();
        let old = content_tag(b"old");
        std::thread::scope(|scope| {
            let (checked, at_check) = std::sync::mpsc::channel();
            let (probed, probe_result) = std::sync::mpsc::channel();
            let (published, after_publish) = std::sync::mpsc::channel();
            let competing = DirObjects::open(directory.path()).unwrap();
            let tag = old.clone();
            let writer = scope.spawn(move || {
                at_check.recv().unwrap();
                let file = File::open(competing.root().join(".lock")).unwrap();
                // SAFETY: the descriptor belongs to this thread. LOCK_NB makes
                // the check/publish overlap observable without a timing race.
                let excluded =
                    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
                if excluded {
                    assert_eq!(
                        std::io::Error::last_os_error().kind(),
                        ErrorKind::WouldBlock
                    );
                }
                drop(file);
                if excluded {
                    probed.send(true).unwrap();
                    after_publish.recv().unwrap();
                }
                let outcome = competing
                    .replace("head", b"competitor", Condition::Matches(tag))
                    .unwrap();
                if !excluded {
                    probed.send(false).unwrap();
                }
                outcome
            });
            let mut excluded = false;
            let first = objects
                .replace_checked("head", b"first", Condition::Matches(old), || {
                    checked.send(()).unwrap();
                    excluded = probe_result.recv().unwrap();
                    assert_eq!(objects.get("head").unwrap().unwrap(), b"old");
                })
                .unwrap();
            if excluded {
                published.send(()).unwrap();
            }
            let second = writer.join().unwrap();
            assert_eq!(first, Replaced::Replaced(content_tag(b"first")));
            assert_eq!(second, Replaced::Mismatch);
            assert_eq!(objects.get("head").unwrap().unwrap(), b"first");
        });
    }
}

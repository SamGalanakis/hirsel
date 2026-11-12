//! Local installation authority. Nix local-store.cc's registerValidPaths keeps
//! trusted associations in local state; git setup.c keeps repository state out
//! of transported objects. Product roots never select this private namespace.
use super::*;
use hirsel_artifact_store::Reference;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::Path,
};

const MAX_ROWS: usize = hirsel_artifact_store::MAX_RECORD / 256;
#[derive(Serialize, Deserialize)]
struct Registration {
    state: PathBuf,
    root: PathBuf,
    filesystem: (u64, u64),
    verifier: ContentHash,
    /// The installed-package header association this run published: its
    /// address, action and header reference.
    header: (ContentHash, ContentHash, Reference),
    generation: String,
    rows: BTreeMap<ContentHash, Reference>,
}
/// An installed-package header association: its address, action and reference.
type Header = (
    hirsel_artifact_store::Address,
    hirsel_artifact_store::Association,
);
/// A root's authenticated registration: V's rows from it over `local`, and
/// the exact header association it binds. An operation opens that header,
/// never a separately reselected one, so its products and V's authority are
/// one generation's.
pub struct Registered {
    pub verifier: Arc<VerifierCache>,
    pub header: AuthenticatedHeader,
}
/// The header association a registration authenticated. Only `registered`
/// constructs one, so a reader that trusts an image lazily on its strength
/// cannot be handed an arbitrary header instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthenticatedHeader(Header);
impl AuthenticatedHeader {
    pub fn header(self) -> Header {
        self.0
    }
}
/// Issuance is private to VerifierCache::publish_bytes. Imported associations
/// are never enumerated. A Precompute adopts only V's own rows from the root's
/// previous generation, each after V's checks at its hit site (adopt.rs).
pub(super) struct Rows {
    store: Store,
    rows: Mutex<BTreeMap<ContentHash, Reference>>,
    overflow: std::sync::atomic::AtomicBool,
    writable: bool,
}
impl hirsel_artifact_store::Contents for Rows {
    fn get_content(
        &self,
        reference: Reference,
    ) -> Result<hirsel_artifact_store::Bytes, hirsel_artifact_store::ContentReadError> {
        hirsel_artifact_store::Contents::get_content(&self.store, reference)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.put_identified_content(domain.identify(bytes)?)
    }
    fn put_identified_content(
        &self,
        content: hirsel_artifact_store::IdentifiedContent<'_>,
    ) -> Option<Reference> {
        self.writable
            .then(|| self.store.put_identified(content).ok())
            .flatten()
    }
}
impl VerifierRows for Rows {
    fn publication_batch(&self) -> Option<hirsel_artifact_store::Batch> {
        self.writable.then(|| self.store.batch())
    }
    fn publication_request(&self) -> Option<hirsel_artifact_store::Request> {
        self.writable.then(|| self.store.request())
    }
    fn row(&self, key: ContentHash) -> Option<Reference> {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .copied()
    }
    fn set_row(&self, key: ContentHash, reference: Reference) {
        if !self.writable {
            return;
        }
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if rows.len() < MAX_ROWS || rows.contains_key(&key) {
            rows.insert(key, reference);
        } else {
            self.overflow
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
#[cfg(test)]
impl Rows {
    /// Drop one association, as a generation that never held it.
    pub(super) fn forget(&self, key: ContentHash) {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
}
pub(super) struct Contents {
    pub(super) local: Arc<dyn hirsel_artifact_store::Contents>,
    pub(super) installed: Arc<Rows>,
}
impl hirsel_artifact_store::Contents for Contents {
    fn get_content(
        &self,
        reference: Reference,
    ) -> Result<hirsel_artifact_store::Bytes, hirsel_artifact_store::ContentReadError> {
        self.local
            .get_content(reference)
            .or_else(|_| self.installed.get_content(reference))
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.local.put_content(domain, bytes)
    }
    fn put_identified_content(
        &self,
        content: hirsel_artifact_store::IdentifiedContent<'_>,
    ) -> Option<Reference> {
        self.local.put_identified_content(content)
    }
}
/// A fresh local verifier run whose rows can be registered only after its
/// caller observes terminal successful Precompute and finishes publication.
///
/// Cooperating precomputes of one root are serialized: `begin` takes the
/// root's exclusive lock in private state and holds it until the installation
/// drops, so root capture, product publication and registration of one run
/// never interleave with another's. `finish` registers the installed-package
/// header association its caller published, never whatever the root holds at
/// finish time. A registration loads only while the root's header association
/// is exactly the registered one, so a writer outside this protocol that
/// changes the header, before or after `finish`, makes it a miss. Every
/// registered row is still checked against current content and reads at use.
///
/// Failure or interruption before `finish` registers nothing and deletes only
/// this run's own generation. The root's previous registration stays, but it
/// loads only while the root still holds the header it names, so a run that
/// failed after publishing its header leaves a safe miss: the next Check runs
/// V and the next Precompute rebuilds. Once the registration is renamed into place its
/// generation is never deleted, even if `finish` then reports an error.
///
/// Trust boundary: the private state directory is trusted to this user
/// account. The registration MAC protects against writers of the output root,
/// not against anyone who can read the generation's key. Missing or unsafe
/// private state is a cold path.
pub struct Installation {
    state: PathBuf,
    root: PathBuf,
    verifier: ContentHash,
    generation: String,
    rows: Arc<Rows>,
    cache: Arc<VerifierCache>,
    committed: bool,
    /// The root's exclusive precompute lock, released when this drops.
    _lock: fs::File,
}
#[cfg(test)]
thread_local! {
    /// Fail `finish` after its rename and before the directory sync.
    pub(super) static FAIL_AFTER_RENAME: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}
impl Installation {
    /// State survives disposable result, native and verifier cache clearing.
    pub fn default_state() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|v| PathBuf::from(v).join(".local/state"))
            })?;
        base.is_absolute().then(|| base.join("hirsel/verifier"))
    }
    pub fn begin(root: &Path, state: &Path, verifier: ContentHash) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        private_directory(state)?;
        let state = state.canonicalize()?;
        separate(&state, &root)?;
        let lock = private_lock(&registration_path(&state, &root).with_extension("lock"))?;
        lock.lock()?;
        let generation = hex(&random()?);
        let directory = state.join(&generation);
        private_directory(&directory)?;
        private_write(&directory.join("key"), &random()?)?;
        let rows = Arc::new(Rows {
            store: Store::new(directory.join("receipts")),
            rows: Mutex::default(),
            overflow: false.into(),
            writable: true,
        });
        rows.store.set_cache_cap(2 << 30);
        // The previous registration, read only under this lock and only while
        // it still authenticates against the root's unchanged header. V may
        // adopt its own rows from it (see adopt.rs); nothing else reads it.
        let previous = Self::rows(&root, &state, verifier)
            .ok()
            .map(|(rows, _)| rows);
        let cache = Arc::new(VerifierCache(
            Backend::Hosted(rows.clone(), rows.clone(), previous),
            Mutex::default(),
            Arc::new(()),
            crate::FactTable::default(),
            Mutex::default(),
        ));
        Ok(Self {
            state,
            root,
            verifier,
            generation,
            rows,
            cache,
            committed: false,
            _lock: lock,
        })
    }
    pub fn verifier(&self) -> Arc<VerifierCache> {
        self.cache.clone()
    }
    /// Register this run's rows for the header association `header` that the
    /// caller published into the root.
    pub fn finish(
        mut self,
        header: (
            hirsel_artifact_store::Address,
            hirsel_artifact_store::Association,
        ),
    ) -> io::Result<()> {
        if self
            .rows
            .overflow
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(invalid());
        }
        let registration = Registration {
            state: self.state.clone(),
            root: self.root.clone(),
            filesystem: filesystem(&self.root)?,
            verifier: self.verifier,
            header: (header.0.0, header.1.action.0, header.1.reference),
            generation: self.generation.clone(),
            rows: self
                .rows
                .rows
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };
        let payload = bincode::serialize(&registration).map_err(|_| invalid())?;
        if payload.len() > hirsel_artifact_store::MAX_RECORD {
            return Err(invalid());
        }
        let directory = self.state.join(&self.generation);
        let key = private_read(&directory.join("key"), 32)?;
        let mut bytes = mac(&key, &payload).to_vec();
        bytes.extend(payload);
        let temporary = directory.join("registration");
        private_write(&temporary, &bytes)?;
        fs::rename(temporary, registration_path(&self.state, &self.root))?;
        // The visible registration names this generation from here on.
        self.committed = true;
        #[cfg(test)]
        if FAIL_AFTER_RENAME.with(std::cell::Cell::take) {
            return Err(io::Error::other("injected failure after rename"));
        }
        fs::File::open(&self.state)?.sync_all()?;
        Ok(())
    }
    /// Pin an authenticated association map. Subsequent product publications
    /// cannot add authority to it; every referenced container still checks its
    /// domain, length and content digest through the ordinary store reader.
    pub fn registered(
        root: &Path,
        state: &Path,
        verifier: ContentHash,
        local: Arc<VerifierCache>,
    ) -> Option<Registered> {
        let (rows, header) = Self::rows(root, state, verifier).ok()?;
        Some(Registered {
            verifier: Arc::new(VerifierCache(
                Backend::Registered(local, rows),
                Mutex::default(),
                Arc::new(()),
                crate::FactTable::default(),
                Mutex::default(),
            )),
            header: AuthenticatedHeader(header),
        })
    }
    /// The authenticated, read-only rows of the root's current registration,
    /// and the header association it authenticated.
    fn rows(root: &Path, state: &Path, verifier: ContentHash) -> io::Result<(Arc<Rows>, Header)> {
        hirsel_intern::work::count(hirsel_intern::work::Counter::VerifierRegistrationReads);
        check_private(state)?;
        let root = root.canonicalize()?;
        let state = state.canonicalize()?;
        separate(&state, &root)?;
        let bytes = private_read(
            &registration_path(&state, &root),
            hirsel_artifact_store::MAX_RECORD + 32,
        )?;
        let (tag, payload) = bytes.split_at_checked(32).ok_or_else(invalid)?;
        let registration: Registration = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(payload.len() as u64)
            .reject_trailing_bytes()
            .deserialize(payload)
            .map_err(|_| invalid())?;
        if registration.state != state
            || registration.root != root
            || registration.verifier != verifier
            || registration.filesystem != filesystem(&root)?
            || !header_current(&root, registration.header)
            || registration.rows.len() > MAX_ROWS
            || registration.generation.len() != 64
            || !registration
                .generation
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        let directory = state.join(&registration.generation);
        check_private(&directory)?;
        let key = private_read(&directory.join("key"), 32)?;
        if key.len() != 32
            || tag
                .iter()
                .zip(mac(&key, payload))
                .fold(0, |v, (a, b)| v | (a ^ b))
                != 0
        {
            return Err(invalid());
        }
        let (address, action, reference) = registration.header;
        Ok((
            Arc::new(Rows {
                store: Store::new(directory.join("receipts")),
                rows: Mutex::new(registration.rows),
                overflow: false.into(),
                writable: false,
            }),
            (
                hirsel_artifact_store::Address(address),
                hirsel_artifact_store::Association {
                    action: hirsel_artifact_store::Action(action),
                    reference,
                },
            ),
        ))
    }
}
impl Drop for Installation {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(self.state.join(&self.generation));
        }
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid local verifier registration",
    )
}
pub(super) fn registration_path(state: &Path, root: &Path) -> PathBuf {
    state.join(format!(
        "{}.registration",
        hex(&ContentHash::of_parts(
            "hirsel.verifier.installation.root.v1",
            [root.as_os_str().as_encoded_bytes()]
        )
        .0)
    ))
}
/// The root's current association at the registered header address is
/// exactly the registered header.
fn header_current(
    root: &Path,
    (address, action, reference): (ContentHash, ContentHash, Reference),
) -> bool {
    Store::new(root.to_path_buf())
        .lookup_owned(hirsel_artifact_store::Address(address))
        .is_some_and(|current| current.action.0 == action && current.reference == reference)
}
#[cfg(unix)]
fn filesystem(root: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(root)?;
    Ok((meta.dev(), meta.ino()))
}
#[cfg(not(unix))]
fn filesystem(_: &Path) -> io::Result<(u64, u64)> {
    Err(invalid())
}
fn separate(state: &Path, root: &Path) -> io::Result<()> {
    if state.starts_with(root) || root.starts_with(state) {
        return Err(invalid());
    }
    for path in [
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache"))),
        std::env::var_os("HIRSEL_RESULTS_DIR").map(PathBuf::from),
        std::env::var_os("HIRSEL_NATIVE_CACHE").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(path) = path.canonicalize()
            && (state.starts_with(&path) || path.starts_with(state))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
#[cfg(unix)]
fn check_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(())
}
#[cfg(not(unix))]
fn check_private(_: &Path) -> io::Result<()> {
    Err(invalid())
}
pub(super) fn private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
    }
    check_private(path)
}
fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
/// Open (creating if absent) a private lock file; a symlink is refused.
fn private_lock(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    check_private(path)?;
    Ok(file)
}
fn private_read(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    check_private(path)?;
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit as u64 {
        return Err(invalid());
    }
    let mut bytes = vec![];
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid());
    }
    Ok(bytes)
}
fn random() -> io::Result<[u8; 32]> {
    let mut bytes = [0; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
// RFC 2104, RustCrypto hmac/src/simple.rs:29-41,60-65. Fixed local keys
// need no arbitrary-length key interface or second cryptographic dependency.
fn mac(key: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut pad = [0x36; 64];
    for (a, b) in pad.iter_mut().zip(key) {
        *a ^= b;
    }
    let mut inner = Sha256::new();
    inner.update(pad);
    inner.update(payload);
    for b in &mut pad {
        *b ^= 0x36 ^ 0x5c;
    }
    let mut outer = Sha256::new();
    outer.update(pad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc4231_hmac_sha256_vectors() {
        assert_eq!(
            super::hex(&super::mac(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            super::hex(&super::mac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}

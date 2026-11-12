//! Local installation authority. Nix local-store.cc's registerValidPaths keeps
//! trusted associations in local state; git setup.c keeps repository state out
//! of transported objects. Product roots never select this private namespace.
use crate::{Reference, Store};
use bincode::Options;
use hirsel_syntax::ContentHash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

#[derive(Serialize, Deserialize)]
struct Registration {
    state: PathBuf,
    root: PathBuf,
    filesystem: (u64, u64),
    compiler: ContentHash,
    /// The installed-package header association this run published: its
    /// address, action and header reference.
    header: (ContentHash, ContentHash, Reference),
    generation: String,
}
/// An installed-package header association: its address, action and reference.
type Header = (crate::Address, crate::Association);
/// One installed header and its local program-record registration.
pub struct Registered {
    pub header: AuthenticatedHeader,
    /// The program-record authority of this same registration.
    pub authority: Authority,
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
/// One root's authenticated registration as an authority for program
/// records (B). Constructed only by `Installation::registration`, after the
/// registration's MAC, root, filesystem, compiler and current header checks
/// hold, so holding one is proof that the registration authenticated.
/// Its epoch is the registration generation and the compiler key's id:
/// replacing or revoking the registration changes it, so every association
/// made under the old epoch misses.
#[derive(Clone)]
pub struct Authority {
    key: [u8; 32],
    pub(super) directory: PathBuf,
    root: PathBuf,
    filesystem: (u64, u64),
    generation: String,
    epoch: ContentHash,
}
impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authority")
            .field("root", &self.root)
            .field("generation", &self.generation)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}
impl Authority {
    pub fn epoch(&self) -> ContentHash {
        self.epoch
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// A MAC under this registration's key, bound to its root, filesystem
    /// and generation as well as `payload`.
    pub(super) fn mac(&self, payload: &[u8]) -> [u8; 32] {
        let mut bound = Vec::with_capacity(payload.len() + 160);
        for part in [
            self.root.as_os_str().as_encoded_bytes(),
            &self.filesystem.0.to_le_bytes(),
            &self.filesystem.1.to_le_bytes(),
            self.generation.as_bytes(),
            payload,
        ] {
            bound.extend_from_slice(&(part.len() as u64).to_le_bytes());
            bound.extend_from_slice(part);
        }
        mac(&self.key, &bound)
    }
}
/// Serializes publication of one installed root and registers its exact header.
/// A failed publication keeps the previous registration. The private state MAC
/// binds disk inputs to this compiler, root and filesystem generation.
pub struct Installation {
    state: PathBuf,
    root: PathBuf,
    compiler: ContentHash,
    generation: String,
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
    /// State survives disposable result and native cache clearing.
    pub fn default_state() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|v| PathBuf::from(v).join(".local/state"))
            })?;
        base.is_absolute()
            .then(|| base.join("hirsel/compiler-records"))
    }
    pub fn begin(root: &Path, state: &Path, compiler: ContentHash) -> io::Result<Self> {
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
        Ok(Self {
            state,
            root,
            compiler,
            generation,
            committed: false,
            _lock: lock,
        })
    }
    /// Register the exact header association published by this installation.
    pub fn finish(mut self, header: (crate::Address, crate::Association)) -> io::Result<()> {
        let registration = Registration {
            state: self.state.clone(),
            root: self.root.clone(),
            filesystem: filesystem(&self.root)?,
            compiler: self.compiler,
            header: (header.0.0, header.1.action.0, header.1.reference),
            generation: self.generation.clone(),
        };
        let payload = bincode::serialize(&registration).map_err(|_| invalid())?;
        if payload.len() > crate::MAX_RECORD {
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
    /// Read the disk registration of this compiler and root.
    pub fn registered(root: &Path, state: &Path, compiler: ContentHash) -> Option<Registered> {
        let (header, authority) = Self::registration(root, state, compiler).ok()?;
        Some(Registered {
            authority,
            header: AuthenticatedHeader(header),
        })
    }
    /// The root's current registered header and program-record domain.
    fn registration(
        root: &Path,
        state: &Path,
        compiler: ContentHash,
    ) -> io::Result<(Header, Authority)> {
        check_private(state)?;
        let root = root.canonicalize()?;
        let state = state.canonicalize()?;
        separate(&state, &root)?;
        let bytes = private_read(&registration_path(&state, &root), crate::MAX_RECORD + 32)?;
        let (tag, payload) = bytes.split_at_checked(32).ok_or_else(invalid)?;
        let registration: Registration = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(payload.len() as u64)
            .reject_trailing_bytes()
            .deserialize(payload)
            .map_err(|_| invalid())?;
        if registration.state != state
            || registration.root != root
            || registration.compiler != compiler
            || registration.filesystem != filesystem(&root)?
            || !header_current(&root, registration.header)
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
        let key: [u8; 32] = key.try_into().map_err(|_| invalid())?;
        let authority = Authority {
            epoch: ContentHash::of_parts(
                "hirsel.program.authority-epoch.v1",
                [registration.generation.as_bytes(), &Sha256::digest(key)[..]],
            ),
            key,
            directory: directory.clone(),
            root: root.clone(),
            filesystem: registration.filesystem,
            generation: registration.generation.clone(),
        };
        Ok((
            (
                crate::Address(address),
                crate::Association {
                    action: crate::Action(action),
                    reference,
                },
            ),
            authority,
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
pub(super) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid compiler registration")
}
pub(super) fn registration_path(state: &Path, root: &Path) -> PathBuf {
    state.join(format!(
        "{}.registration",
        hex(&ContentHash::of_parts(
            "hirsel.compiler.installation.root.v1",
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
        .lookup_owned(crate::Address(address))
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
pub(super) fn check_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(())
}
#[cfg(not(unix))]
pub(super) fn check_private(_: &Path) -> io::Result<()> {
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
pub(super) fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
pub(super) fn private_lock(path: &Path) -> io::Result<fs::File> {
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
pub(super) fn private_read(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
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
pub(super) fn random() -> io::Result<[u8; 32]> {
    let mut bytes = [0; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub(super) fn hex(bytes: &[u8]) -> String {
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

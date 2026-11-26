//! The remote object cache (CAS S5, ruling G1 = B): a plain HTTP store of
//! `o/<sha256>` blobs and `r/<keyid>/<key>` signed records, configured by
//! `[cache.remote]` in `$XDG_CONFIG_HOME/hirsel/config.toml` (docs/spec/cli.md,
//! "Remote cache").
//!
//! A blob is used only when its bytes hash to the name it was fetched under;
//! a record only when a listed builder key signed it (`store::signed`), and
//! only for the kinds that travel: release std sides, object records
//! (release parts and development unit objects) and development instances. A fetched record is never written to `keys/`, so the
//! launcher and runner host, which read only `keys/` and open no session, reuse
//! nothing this host did not produce (launch.md, "a cached mapping"). A link
//! over remote content is keyed apart ([`bind`]).
//!
//! A session is opened by a build or test request (`request::run`) after the
//! launcher, host and operator commands have returned. A dead remote costs one
//! timeout: a failed request stops every request to it for [`DOWN`], and a
//! missing name is not asked for again for [`MISSING`]. Uploads run on one
//! background thread and are drained when the request concludes ([`finish`]).
use super::signed::{self, Builder, Kind, Signed};
use super::{Bare, Store};
use crate::instances::{Location, Records};
use crate::native_object::{self, Instance};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::NativeObject;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a remote that failed a request is not asked again.
const DOWN: Duration = Duration::from_secs(60);
/// How long a name the remote did not have is not asked for again.
const MISSING: Duration = Duration::from_secs(10 * 60);
const CONNECT: Duration = Duration::from_secs(2);
const READ: Duration = Duration::from_secs(20);
/// The largest signed record read.
const RECORD_LIMIT: u64 = 256 << 20;
const STD_MANIFEST_SCHEMA: u32 = 1;

/// `config.toml`: only the `cache.remote` table is read, and every field of
/// it is known.
#[derive(serde::Deserialize)]
struct ConfigFile {
    #[serde(default)]
    cache: CacheTable,
}
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheTable {
    remote: Option<Config>,
}
/// `[cache.remote]`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Config {
    /// The cache's base URL, `http://` or `https://`.
    pub url: String,
    /// The Ed25519 public keys, 64 hex digits each, whose records are used.
    #[serde(default)]
    pub trusted: Vec<String>,
    /// A PKCS#8 Ed25519 private key file; with it, this host uploads.
    pub signing_key: Option<PathBuf>,
}

/// The signing key this host uploads with.
struct Signer {
    pkcs8: Vec<u8>,
    id: ContentHash,
}

/// What a remote failed or lacked, by URL, for every session of the process.
#[derive(Default)]
struct Health {
    down_until: Option<Instant>,
    missing: HashMap<String, Instant>,
}
static HEALTH: Mutex<Option<HashMap<String, Health>>> = Mutex::new(None);
static SESSION: Mutex<Option<Arc<Remote>>> = Mutex::new(None);

/// What an upload carries: a record of `kind` under `key` in `store`, or
/// the std side under `key` whole.
enum Offer {
    Record(Kind, Store, ContentHash),
    Side(Store, ContentHash),
}

/// A remote cache, as one request reads and writes it.
pub(crate) struct Remote {
    url: String,
    trusted: Vec<Builder>,
    signer: Option<Signer>,
    agent: ureq::Agent,
    /// The builders whose records this session used.
    used: Mutex<BTreeSet<ContentHash>>,
    uploads: Mutex<Option<(Sender<Offer>, JoinHandle<()>)>>,
}

/// Open the session of this request from the user's configuration: none
/// when it configures no remote, or one that would read nothing and write
/// nothing.
pub(crate) fn begin() -> Result<(), String> {
    let session = match configured()? {
        Some(config) => Remote::new(config)?.map(Arc::new),
        None => None,
    };
    *SESSION.lock().unwrap() = session;
    Ok(())
}

/// Close the session, waiting for its uploads.
pub(crate) fn finish() {
    let Some(remote) = SESSION.lock().unwrap().take() else {
        return;
    };
    if let Some((offers, worker)) = remote.uploads.lock().unwrap().take() {
        drop(offers);
        let _ = worker.join();
    }
}

/// The session a build reads and writes, when it has one: none for a
/// coverage build or an FFI-8 program, whose records never travel.
pub(crate) fn for_build(options: &crate::native::NativeOptions) -> Option<Arc<Remote>> {
    if options.coverage.is_some() || !options.foreign_grants.is_empty() {
        return None;
    }
    SESSION.lock().unwrap().clone()
}

/// `key`, a link's key, kept apart from every link over no remote content
/// when this session used a listed builder's records: a release link's key
/// names no object, so the host would otherwise run an executable linked
/// over content it did not produce.
pub(crate) fn bind(key: ContentHash) -> ContentHash {
    let used = match SESSION.lock().unwrap().as_ref() {
        Some(remote) => remote.used.lock().unwrap().clone(),
        None => return key,
    };
    if used.is_empty() {
        return key;
    }
    let ids: Vec<&[u8]> = used.iter().map(|id| &id.0[..]).collect();
    ContentHash::of_parts("hirsel.remote-link.v1", [&key.0[..]].into_iter().chain(ids))
}

/// The object under `key` in `store` ([`native_object::load_shared`]), else
/// a listed builder's, which the resident compiler's object cache never
/// holds: a later request with no remote must not reuse it unbound.
pub(crate) fn load_shared(
    store: &Store,
    options: &crate::native::NativeOptions,
    key: ContentHash,
) -> io::Result<Option<native_object::Shared>> {
    if let Some(found) = native_object::load_shared(store, key)? {
        return Ok(Some(found));
    }
    let Some(remote) = for_build(options) else {
        return Ok(None);
    };
    let received = remote.object(store, key)?;
    Ok(received.map(|(bare, object)| (bare, object.debug.map(Arc::new), object.bytes.into())))
}

/// Publish an emitted object ([`native_object::publish`]) and offer it to
/// the build's remote.
pub(crate) fn publish(
    store: &Store,
    options: &crate::native::NativeOptions,
    key: ContentHash,
    object: &NativeObject,
) -> io::Result<Bare> {
    let bare = native_object::publish(store, key, object)?;
    if let Some(remote) = for_build(options) {
        remote.offer(Kind::Object, store, key);
    }
    Ok(bare)
}

fn configured() -> Result<Option<Config>, String> {
    let directory = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let Some(path) = directory.map(|d| d.join("hirsel/config.toml")) else {
        return Ok(None);
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let file: ConfigFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(file.cache.remote)
}

fn public_key(hex: &str) -> Result<[u8; 32], String> {
    let bad = || format!("trusted key `{hex}` is not 64 hex digits");
    if hex.len() != 64 {
        return Err(bad());
    }
    let mut key = [0; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16)
            .map_err(|_| bad())?;
    }
    Ok(key)
}

impl Remote {
    /// The remote `config` describes, or none when it would read nothing
    /// and write nothing.
    pub(crate) fn new(config: Config) -> Result<Option<Self>, String> {
        let trusted = (config.trusted.iter())
            .map(|hex| public_key(hex).map(Builder::new))
            .collect::<Result<Vec<_>, _>>()?;
        let signer = match &config.signing_key {
            Some(path) => {
                let pkcs8 = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let public = hirsel_core_net::key_wrap::ed25519_public(&pkcs8)
                    .map_err(|_| format!("{}: not a PKCS#8 Ed25519 key", path.display()))?;
                let id = Builder::new(public).id;
                Some(Signer { pkcs8, id })
            }
            None => None,
        };
        if trusted.is_empty() && signer.is_none() {
            return Ok(None);
        }
        let url = config.url.trim_end_matches('/').to_owned();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!(
                "cache.remote url `{url}` is not http:// or https://"
            ));
        }
        let agent = crate::package_store::https_config()
            .https_only(false)
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT))
            .timeout_per_read(Some(READ))
            .build()
            .into();
        Ok(Some(Self {
            url,
            trusted,
            signer,
            agent,
            used: Mutex::new(BTreeSet::new()),
            uploads: Mutex::new(None),
        }))
    }

    /// The object record under `key`, read from `store` or received.
    pub(crate) fn object(
        &self,
        store: &Store,
        key: ContentHash,
    ) -> io::Result<Option<(Bare, NativeObject)>> {
        match self.signed(store, Kind::Object, key)? {
            Some(signed) => native_object::load_payload(store, &signed.payload),
            None => Ok(None),
        }
    }

    /// The record of `kind` under `key` signed by a listed builder, kept in
    /// `store`'s `r/` or received, with every blob it names in `store`'s
    /// own `o/`.
    pub(crate) fn signed(
        &self,
        store: &Store,
        kind: Kind,
        key: ContentHash,
    ) -> io::Result<Option<Signed>> {
        for builder in &self.trusted {
            if let Some(signed) = self.from(store, kind, key, builder)? {
                return Ok(Some(signed));
            }
        }
        Ok(None)
    }

    fn from(
        &self,
        store: &Store,
        kind: Kind,
        key: ContentHash,
        builder: &Builder,
    ) -> io::Result<Option<Signed>> {
        let listed = std::slice::from_ref(builder);
        let signed = match store.signed(kind, key, listed)? {
            Some(signed) => signed,
            None => {
                let path = record_path(builder.id, key);
                let Some(bytes) = self.get(&path, RECORD_LIMIT) else {
                    return Ok(None);
                };
                match store.keep_signed(&bytes, kind, key, listed)? {
                    Some(signed) => signed,
                    None => return Ok(None),
                }
            }
        };
        for &(address, len) in &signed.blobs {
            let path = store.blob_path(address);
            if fs::metadata(&path).is_ok_and(|m| m.len() == len) {
                continue;
            }
            let Some(bytes) = self.get(&blob_path(address), len.saturating_add(1)) else {
                return Ok(None);
            };
            let bare = Bare::of(&bytes);
            if bytes.len() as u64 != len || bare.address != address {
                return Ok(None);
            }
            store.write_blob(bare, &bytes)?;
        }
        self.used.lock().unwrap().insert(builder.id);
        Ok(Some(signed))
    }

    /// A listed builder's std side under `key`, unpacked into a side of
    /// `cache` whose records carry that builder's id, when one is received.
    pub(crate) fn std_side(&self, cache: &Store, key: ContentHash) -> io::Result<Option<Store>> {
        for builder in &self.trusted {
            let name =
                ContentHash::of_parts("hirsel.remote-side.v1", [&builder.id.0[..], &key.0[..]]);
            let side = cache.side(name)?.origin(builder.id);
            if side.get(key)?.is_some() {
                return Ok(Some(side));
            }
            let Some(signed) = self.from(&side, Kind::Std, key, builder)? else {
                continue;
            };
            if unpack(&side, &signed)? {
                return Ok(Some(side));
            }
        }
        Ok(None)
    }

    /// Upload the record under `key` in `store`, of `kind`, with its blobs.
    pub(crate) fn offer(&self, kind: Kind, store: &Store, key: ContentHash) {
        self.send(Offer::Record(kind, store.clone(), key));
    }
    /// Upload the std side under `key`, this host's, when the request ends:
    /// its parts are generated as the program's link reads them.
    pub(crate) fn offer_side(&self, side: &Store, key: ContentHash) {
        self.send(Offer::Side(side.clone(), key));
    }
    fn send(&self, offer: Offer) {
        if self.signer.is_none() {
            return;
        }
        let mut uploads = self.uploads.lock().unwrap();
        let (offers, _) = uploads.get_or_insert_with(|| {
            let (offers, received) = channel();
            let remote = Remote {
                url: self.url.clone(),
                trusted: Vec::new(),
                signer: self.signer.as_ref().map(|s| Signer {
                    pkcs8: s.pkcs8.clone(),
                    id: s.id,
                }),
                agent: self.agent.clone(),
                used: Mutex::new(BTreeSet::new()),
                uploads: Mutex::new(None),
            };
            let worker = std::thread::spawn(move || {
                let mut sides = Vec::new();
                for offer in received {
                    match offer {
                        Offer::Record(kind, store, key) => remote.upload(kind, &store, key),
                        Offer::Side(side, key) => sides.push((side, key)),
                    }
                }
                for (side, key) in sides {
                    remote.upload_side(&side, key);
                }
            });
            (offers, worker)
        });
        let _ = offers.send(offer);
    }

    fn upload(&self, kind: Kind, store: &Store, key: ContentHash) {
        let Ok(Some(payload)) = store.get(key) else {
            return;
        };
        let Some(bares) = native_object::blobs(&payload, kind == Kind::Instance) else {
            return;
        };
        let mut blobs = Vec::with_capacity(bares.len());
        for bare in bares {
            let Ok(Some(bytes)) = store.get_blob(bare) else {
                return;
            };
            if !self.put_blob(bare.address, &bytes) {
                return;
            }
            blobs.push((bare.address, bytes.len() as u64));
        }
        self.put_record(kind, key, &blobs, &payload);
    }

    /// Pack the side under `key`: its records and its blobs, the blobs one
    /// pack the manifest indexes.
    fn upload_side(&self, side: &Store, key: ContentHash) {
        let signer = self.signer.as_ref().expect("an uploading session");
        if self.has(&record_path(signer.id, key)) {
            return;
        }
        let mut records = Vec::new();
        for name in names(side.root(), super::collect::RECORDS) {
            if let Ok(Some(payload)) = side.get(name) {
                records.push((name, payload));
            }
        }
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        for name in names(side.root(), super::collect::BLOBS) {
            let Ok(bytes) = fs::read(side.blob_path(name)) else {
                continue;
            };
            if Bare::of(&bytes).address == name {
                index.push((name, bytes.len() as u64));
                pack.extend(bytes);
            }
        }
        let Ok(manifest) = crate::encode(&(STD_MANIFEST_SCHEMA, records, index)) else {
            return;
        };
        let address = Bare::of(&pack).address;
        if self.put_blob(address, &pack) {
            self.put_record(Kind::Std, key, &[(address, pack.len() as u64)], &manifest);
        }
    }

    fn put_record(
        &self,
        kind: Kind,
        key: ContentHash,
        blobs: &[(ContentHash, u64)],
        payload: &[u8],
    ) {
        let signer = self.signer.as_ref().expect("an uploading session");
        let mut bytes = signed::unsigned(kind, key, signer.id, blobs, payload);
        let Ok(signature) = hirsel_core_net::key_wrap::sign(
            hirsel_core_net::key_wrap::SignAlg::Ed25519,
            &signer.pkcs8,
            &signed::message(&bytes),
            hirsel_core_net::key_wrap::SigningRandom::Host,
        ) else {
            return;
        };
        bytes.extend(signature);
        self.put(&record_path(signer.id, key), &bytes);
    }

    fn put_blob(&self, address: ContentHash, bytes: &[u8]) -> bool {
        let path = blob_path(address);
        self.has(&path) || self.put(&path, bytes)
    }

    /// Whether the remote holds `path`.
    fn has(&self, path: &str) -> bool {
        if self.down() {
            return false;
        }
        match self.agent.head(format!("{}/{path}", self.url)).call() {
            Ok(response) => response.status().as_u16() == 200,
            Err(_) => {
                self.trip();
                false
            }
        }
    }

    fn put(&self, path: &str, bytes: &[u8]) -> bool {
        if self.down() {
            return false;
        }
        let put = self.agent.put(format!("{}/{path}", self.url)).send(bytes);
        match put {
            Ok(response) if response.status().is_success() => {
                self.health(|h| h.missing.remove(path));
                true
            }
            _ => {
                self.trip();
                false
            }
        }
    }

    /// The body at `path`, at most `limit` bytes; none when the remote is
    /// down, lacks it, or fails.
    fn get(&self, path: &str, limit: u64) -> Option<Vec<u8>> {
        let missing = self.health(|h| {
            h.down_until.is_some_and(|until| Instant::now() < until)
                || h.missing
                    .get(path)
                    .is_some_and(|until| Instant::now() < *until)
        });
        if missing {
            return None;
        }
        let response = self.agent.get(format!("{}/{path}", self.url)).call();
        match response {
            Ok(mut response) => match response.status().as_u16() {
                200 => {
                    let body = response.body_mut().with_config().limit(limit).read_to_vec();
                    body.inspect_err(|_| self.trip()).ok()
                }
                404 => {
                    let until = Instant::now() + MISSING;
                    self.health(|h| h.missing.insert(path.to_owned(), until));
                    None
                }
                _ => {
                    self.trip();
                    None
                }
            },
            Err(_) => {
                self.trip();
                None
            }
        }
    }

    fn down(&self) -> bool {
        self.health(|h| h.down_until.is_some_and(|until| Instant::now() < until))
    }
    fn trip(&self) {
        self.health(|h| h.down_until = Some(Instant::now() + DOWN));
    }
    fn health<T>(&self, f: impl FnOnce(&mut Health) -> T) -> T {
        let mut health = HEALTH.lock().unwrap();
        f(health
            .get_or_insert_with(HashMap::new)
            .entry(self.url.clone())
            .or_default())
    }
}

/// Write a received std side's blobs from its pack, each checked against
/// its name, then its records under the builder's origin; whether all of it
/// checked.
fn unpack(side: &Store, signed: &Signed) -> io::Result<bool> {
    type Manifest = (u32, Vec<(ContentHash, Vec<u8>)>, Vec<(ContentHash, u64)>);
    let Some((STD_MANIFEST_SCHEMA, records, index)) = crate::decode::<Manifest>(&signed.payload)
    else {
        return Ok(false);
    };
    let [(pack, _)] = signed.blobs[..] else {
        return Ok(false);
    };
    let bytes = fs::read(side.blob_path(pack))?;
    if Bare::of(&bytes).address != pack {
        return Ok(false);
    }
    let mut rest = &bytes[..];
    for (address, len) in index {
        let Some((blob, tail)) = usize::try_from(len)
            .ok()
            .and_then(|n| rest.split_at_checked(n))
        else {
            return Ok(false);
        };
        rest = tail;
        let bare = Bare::of(blob);
        if bare.address != address {
            return Ok(false);
        }
        side.write_blob(bare, blob)?;
    }
    if !rest.is_empty() {
        return Ok(false);
    }
    for (key, payload) in records {
        side.put_keyed(key, &payload)?;
    }
    let _ = fs::remove_file(side.blob_path(pack));
    Ok(true)
}

/// The names of the files under `directory` of the content store at `root`.
fn names(root: &Path, directory: &str) -> Vec<ContentHash> {
    let mut found = Vec::new();
    for shard in fs::read_dir(root.join(directory))
        .into_iter()
        .flatten()
        .flatten()
    {
        for file in fs::read_dir(shard.path()).into_iter().flatten().flatten() {
            let hex = [shard.file_name(), file.file_name()]
                .map(|n| n.to_string_lossy().into_owned())
                .concat();
            if let Ok(name) = public_key(&hex) {
                found.push(ContentHash(name));
            }
        }
    }
    found
}

fn record_path(signer: ContentHash, key: ContentHash) -> String {
    format!("r/{signer}/{key}")
}
fn blob_path(address: ContentHash) -> String {
    format!("o/{address}")
}

impl Records {
    /// The instance record under `key` in the store for `location`, this
    /// host's or a listed builder's.
    pub(crate) fn load(
        &self,
        location: Location,
        key: ContentHash,
    ) -> io::Result<Option<Instance>> {
        let store = self.store(location);
        if let Some(record) = native_object::load_instance(store, key)? {
            return Ok(Some(record));
        }
        let Some(remote) = &self.remote else {
            return Ok(None);
        };
        Ok(remote
            .signed(store, Kind::Instance, key)?
            .and_then(|signed| native_object::instance(&signed.payload)))
    }
    /// Publish an emitted instance ([`native_object::publish_instance`]) and
    /// offer it to the remote.
    pub(crate) fn offer(
        &self,
        store: &Store,
        key: ContentHash,
        object: &NativeObject,
        record: Instance,
    ) -> io::Result<(Bare, Option<Bare>)> {
        let published = native_object::publish_instance(store, key, object, record)?;
        if let Some(remote) = &self.remote {
            remote.offer(Kind::Instance, store, key);
        }
        Ok(published)
    }
}

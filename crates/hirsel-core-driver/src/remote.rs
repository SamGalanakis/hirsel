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
use sha2::Digest;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a remote that failed a request is not asked again.
const DOWN: Duration = Duration::from_secs(15);
/// How long a name the remote did not have is not asked for again.
const MISSING: Duration = Duration::from_secs(30);
const CONNECT: Duration = Duration::from_secs(5);
const READ: Duration = Duration::from_secs(30);
const STD_MANIFEST_SCHEMA: u32 = 1;

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct TransferPolicy {
    record_bytes: u64,
    blob_bytes: u64,
    work_bytes: u64,
}

impl TransferPolicy {
    fn allows_blobs(self, sizes: impl IntoIterator<Item = u64>) -> bool {
        let mut remaining = self.work_bytes;
        for size in sizes {
            if size > self.blob_bytes || size > remaining {
                return false;
            }
            remaining -= size;
        }
        true
    }

    fn allows_record(self, count: usize, payload_bytes: u64) -> bool {
        signed::envelope_len(count, payload_bytes).is_some_and(|len| len <= self.record_bytes)
    }
}

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
    transfer: TransferPolicy,
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
    policy: TransferPolicy,
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
    remote.finish_uploads();
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
        if config.transfer.record_bytes == 0
            || config.transfer.blob_bytes == 0
            || config.transfer.blob_bytes > config.transfer.work_bytes
        {
            return Err("cache.remote.transfer requires positive record-bytes and blob-bytes, and work-bytes at least blob-bytes".into());
        }
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
            policy: config.transfer,
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
        let signed = match store.signed(kind, key, listed, self.policy.record_bytes)? {
            Some(signed) => signed,
            None => {
                let path = record_path(builder.id, key);
                let Some(bytes) = self.get_record(&path) else {
                    return Ok(None);
                };
                match store.keep_signed(&bytes, kind, key, listed)? {
                    Some(signed) => signed,
                    None => return Ok(None),
                }
            }
        };
        if !self
            .policy
            .allows_blobs(signed.blobs.iter().map(|&(_, len)| len))
        {
            return Ok(None);
        }
        for &(address, len) in &signed.blobs {
            let path = store.blob_path(address);
            if fs::metadata(&path).is_ok_and(|m| m.len() == len) {
                continue;
            }
            if !self.get_blob(store, address, len)? {
                return Ok(None);
            }
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

    pub(crate) fn finish_uploads(&self) {
        if let Some((offers, worker)) = self.uploads.lock().unwrap().take() {
            drop(offers);
            let _ = worker.join();
        }
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
                policy: self.policy,
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
        if !self.policy.allows_record(bares.len(), payload.len() as u64) {
            return;
        }
        let mut blobs = Vec::with_capacity(bares.len());
        let mut files = Vec::with_capacity(bares.len());
        let mut remaining = self.policy.work_bytes;
        for bare in bares {
            let Some(file) = store
                .layers(store.blob_path(bare.address))
                .find_map(|(path, own)| {
                    let mut file = File::open(path).ok()?;
                    let len = file.metadata().ok()?.len();
                    if len > self.policy.blob_bytes.min(remaining)
                        || digest_copy(&mut file, &mut io::sink(), len)
                            .ok()
                            .flatten()?
                            != bare.address
                    {
                        return None;
                    }
                    file.rewind().ok()?;
                    if own && store.cas.is_some() {
                        super::collect::touch(&file);
                    }
                    Some((file, len))
                })
            else {
                return;
            };
            blobs.push((bare.address, file.1));
            remaining -= file.1;
            files.push(file.0);
        }
        for ((address, len), mut file) in blobs.iter().copied().zip(files) {
            if !self.put_blob(address, &mut file, len) {
                return;
            }
        }
        self.put_record(kind, key, &blobs, &payload);
    }

    /// Pack the side under `key` on disk, with the per-blob transfer limit
    /// bounding the temporary file and the HTTP body in both directions.
    fn upload_side(&self, side: &Store, key: ContentHash) {
        let signer = self.signer.as_ref().expect("an uploading session");
        if self.has(&record_path(signer.id, key)) {
            return;
        }
        let (Ok(mut pack), mut index, mut records) = (side.temporary(), Vec::new(), Vec::new())
        else {
            return;
        };
        let mut len = 0;
        for address in names(side.root(), super::collect::BLOBS) {
            let Ok(mut source) = File::open(side.blob_path(address)) else {
                return;
            };
            let Ok(bytes) = source.metadata().map(|m| m.len()) else {
                return;
            };
            if bytes > self.policy.blob_bytes - len
                || !matches!(digest_copy(&mut source, &mut pack.file, bytes), Ok(Some(hash)) if hash == address)
            {
                return;
            }
            index.push((address, bytes));
            len += bytes;
        }
        let mut retained_bytes = 0;
        for name in names(side.root(), super::collect::RECORDS) {
            if let Ok(Some(payload)) = side.get(name) {
                let bytes = payload.len() as u64;
                if bytes > self.policy.record_bytes - retained_bytes {
                    return;
                }
                retained_bytes += bytes;
                records.push((name, payload));
            }
        }
        let manifest = (STD_MANIFEST_SCHEMA, records, index);
        let Ok(payload_bytes) = hirsel_core_ids::encoding::fixed_size(&manifest) else {
            return;
        };
        if !self.policy.allows_record(1, payload_bytes) {
            return;
        }
        let Ok(payload) = crate::encode(&manifest) else {
            return;
        };
        if pack.file.rewind().is_err() {
            return;
        }
        let Ok(Some(address)) = digest_copy(&mut pack.file, &mut io::sink(), len) else {
            return;
        };
        if pack.file.rewind().is_ok() && self.put_blob(address, &mut pack.file, len) {
            self.put_record(Kind::Std, key, &[(address, len)], &payload);
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
        if !self.policy.allows_record(blobs.len(), payload.len() as u64) {
            return;
        }
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
        self.put(&record_path(signer.id, key), &bytes[..], bytes.len() as u64);
    }

    fn put_blob(&self, address: ContentHash, file: &mut File, len: u64) -> bool {
        let path = blob_path(address);
        self.has(&path) || self.put(&path, ureq::SendBody::from_reader(&mut file.take(len)), len)
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

    fn put(&self, path: &str, body: impl ureq::AsSendBody, len: u64) -> bool {
        if self.down() {
            return false;
        }
        let put = self
            .agent
            .put(format!("{}/{path}", self.url))
            .header("Content-Length", len.to_string())
            .send(body);
        match put {
            Ok(response) if response.status().is_success() => {
                self.health(|h| h.missing.remove(path));
                true
            }
            Ok(response) if response.status().as_u16() == 413 => false,
            _ => {
                self.trip();
                false
            }
        }
    }

    fn response(&self, path: &str) -> Option<ureq::http::Response<ureq::Body>> {
        let unavailable = self.health(|h| {
            h.down_until.is_some_and(|until| Instant::now() < until)
                || h.missing
                    .get(path)
                    .is_some_and(|until| Instant::now() < *until)
        });
        if unavailable {
            return None;
        }
        match self.agent.get(format!("{}/{path}", self.url)).call() {
            Ok(response) if response.status().as_u16() == 200 => Some(response),
            Ok(response) if response.status().as_u16() == 404 => {
                let until = Instant::now() + MISSING;
                self.health(|h| h.missing.insert(path.to_owned(), until));
                None
            }
            Ok(response) if response.status().as_u16() == 413 => None,
            _ => {
                self.trip();
                None
            }
        }
    }

    fn get_record(&self, path: &str) -> Option<Vec<u8>> {
        let mut response = self.response(path)?;
        match response
            .body_mut()
            .with_config()
            .limit(self.policy.record_bytes)
            .read_to_vec()
        {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                self.body_error(error);
                None
            }
        }
    }

    fn body_error(&self, error: ureq::Error) {
        if !matches!(error, ureq::Error::BodyExceedsLimit(_)) {
            self.trip();
        }
    }

    fn get_blob(&self, store: &Store, address: ContentHash, len: u64) -> io::Result<bool> {
        let Some(mut response) = self.response(&blob_path(address)) else {
            return Ok(false);
        };
        let mut reader = response.body_mut().with_config().limit(len).reader();
        publish_blob(store, address, len, |file| {
            let hash = match digest_copy(&mut reader, file, len) {
                Ok(hash) => hash,
                Err(BlobIo::Read(error)) => {
                    self.body_error(error.into());
                    return Ok(false);
                }
                Err(BlobIo::Write(error)) => return Err(error),
            };
            match reader.read(&mut [0]) {
                Ok(0) => Ok(hash == Some(address)),
                Ok(_) => unreachable!("the body reader limits bytes to the declared length"),
                Err(error) => {
                    self.body_error(error.into());
                    Ok(false)
                }
            }
        })
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
    let [(pack, len)] = signed.blobs[..] else {
        return Ok(false);
    };
    let mut file = File::open(side.blob_path(pack))?;
    if file.metadata()?.len() != len
        || digest_copy(&mut file, &mut io::sink(), len).map_err(BlobIo::into_io)? != Some(pack)
    {
        return Ok(false);
    }
    file.rewind()?;
    let mut remaining = len;
    let pack_is_blob = index.iter().any(|&(address, _)| address == pack);
    for (address, len) in index {
        if len > remaining
            || !publish_blob(side, address, len, |out| {
                Ok(digest_copy(&mut file, out, len).map_err(BlobIo::into_io)? == Some(address))
            })?
        {
            return Ok(false);
        }
        remaining -= len;
    }
    if remaining != 0 {
        return Ok(false);
    }
    for (key, payload) in records {
        side.put_keyed(key, &payload)?;
    }
    if !pack_is_blob {
        let _ = fs::remove_file(side.blob_path(pack));
    }
    Ok(true)
}

enum BlobIo {
    Read(io::Error),
    Write(io::Error),
}

impl BlobIo {
    fn into_io(self) -> io::Error {
        match self {
            Self::Read(error) | Self::Write(error) => error,
        }
    }
}

/// Copy and hash exactly `len` bytes, leaving any following bytes unread.
fn digest_copy(
    input: &mut impl Read,
    output: &mut impl Write,
    len: u64,
) -> Result<Option<ContentHash>, BlobIo> {
    let mut input = input.take(len);
    let mut sha = sha2::Sha256::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let read = match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(BlobIo::Read(error)),
        };
        output.write_all(&chunk[..read]).map_err(BlobIo::Write)?;
        sha.update(&chunk[..read]);
    }
    Ok((input.limit() == 0).then(|| ContentHash(sha.finalize().into())))
}

fn publish_blob(
    store: &Store,
    address: ContentHash,
    len: u64,
    write: impl FnOnce(&mut File) -> io::Result<bool>,
) -> io::Result<bool> {
    let mut rejected = false;
    let published = store.publish_to(|file| {
        if !write(file)? {
            rejected = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "remote blob failed verification",
            ));
        }
        Ok((store.blob_path(address), ()))
    });
    if rejected {
        return Ok(false);
    }
    published?;
    if let Some(cas) = &store.cas {
        cas.wrote(len as usize);
    }
    super::wrote("bare");
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

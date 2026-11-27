//! Pinned registry transport and verified local log state.

use crate::package_content::{Id, Library, Root, check_identity};
use crate::package_manifest::name::Version;
use crate::package_store::Transport;
use crate::registry_route::{Route, Routes};
use crate::registry_verify::notes::{Checkpoint, Log, Trust};
use crate::registry_verify::status::StatusReplay;
use crate::registry_verify::{Error, Hash, Release, ReleaseClaim};
use sha2::{Digest, Sha256};
use std::borrow::Borrow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

type Failure = Box<dyn std::error::Error + Send + Sync>;
type Result<T, E = Failure> = std::result::Result<T, E>;

const NOTE_LIMIT: usize = 128 << 10;
const BUNDLE_LIMIT: usize = (u16::MAX as usize + 2) * 256;
const EVIDENCE_LIMIT: usize = 64 << 20;

pub(crate) struct Client<'a> {
    routes: &'a Routes,
    route: &'a Route,
    transport: &'a Transport,
    cache: &'a Path,
}

impl<'a> Client<'a> {
    pub(crate) fn new(
        routes: &'a Routes,
        route: &'a Route,
        transport: &'a Transport,
        cache: &'a Path,
    ) -> Self {
        Self {
            routes,
            route,
            transport,
            cache,
        }
    }

    pub(crate) fn route(&self) -> &Route {
        self.route
    }

    pub(crate) fn frame(
        &self,
        id: &Id,
        root: Root,
        mut receive: impl FnMut(usize) -> io::Result<()>,
    ) -> Result<Library> {
        let path = format!("c/{}.zst", hex(&root.0));
        let mut body = self
            .get(&path)?
            .ok_or_else(|| format!("{id} {root}: {} has no package frame", self.route.origin()))?;
        let len = body.content_length();
        let mut reader = body.as_reader();
        let mut counted = Counted(|buf: &mut [u8]| {
            let n = reader.read(buf)?;
            receive(n)?;
            Ok(n)
        });
        let (actual, library) = Library::read_frame(&mut counted, len)
            .map_err(|error| format!("{id} {root}: {error}"))?;
        // ureq returns the connection to the pool when its body reader reaches EOF.
        if counted.read(&mut [0])? != 0 {
            return Err(format!("{id} {root}: registry frame has trailing bytes").into());
        }
        if actual != root {
            return Err(format!("{id}: registry frame has root {actual}, expected {root}").into());
        }
        check_identity(id, root, &library.content.id)?;
        Ok(library)
    }

    pub(crate) fn hints(&self, name: &str) -> Result<Vec<Hint>> {
        self.require_name(name)?;
        let path = format!("versions/{name}");
        let log = self.log(Log::Release);
        let missing = || format!("no version hints for {name} at {}", self.route.origin());
        let offline = self.transport.is_offline();
        let bytes = if offline {
            log.cached(&path, EVIDENCE_LIMIT)?
        } else {
            self.read(&path, EVIDENCE_LIMIT)?
        }
        .ok_or_else(missing)?;
        let hints: Vec<(String, u64, u64)> = serde_json::from_slice(&bytes)?;
        let bad_hint = || format!("invalid version hint for {name}");
        let hints = hints
            .into_iter()
            .map(|(version, index, covering_size)| {
                let version = Version::parse(&version).ok_or_else(bad_hint)?;
                if index >= covering_size {
                    return Err(bad_hint());
                }
                Ok(Hint {
                    version,
                    index,
                    covering_size,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !offline {
            log.store_object(&path, &bytes)?;
        }
        Ok(hints)
    }

    pub(crate) fn release_checkpoint(&self, now: u64) -> Result<Checkpoint> {
        let log = self.log(Log::Release);
        if self.transport.is_offline() {
            let (head, _) = log
                .saved()?
                .ok_or("--frozen needs a verified release checkpoint")?;
            head.check_fresh(now)?;
            return Ok(head);
        }
        let _guard = log.lock()?;
        let saved = log.saved()?;
        let prior = saved.as_ref().map(|(head, _)| head);
        let current = log.current(prior, now)?;
        if prior.is_none_or(|prior| prior.note() != current.note()) {
            log.save(&current, &[])?;
        }
        Ok(current)
    }

    pub(crate) fn release(
        &self,
        head: &Checkpoint,
        index: u64,
        claim: ReleaseClaim<'_>,
    ) -> Result<Release> {
        let (name, _) = claim.identity.rsplit_once('@').ok_or(Error::ReleaseClaim)?;
        self.require_name(name)?;
        let log = self.release_log(head)?;
        log.prove(|objects| head.release(index, claim, |path| objects.read(path)))
            .map_err(Into::into)
    }

    pub(crate) fn release_from_hint(
        &self,
        head: &Checkpoint,
        name: &str,
        hint: &Hint,
    ) -> Result<Release> {
        self.require_name(name)?;
        let log = self.release_log(head)?;
        let id = format!("{name}@{}", hint.version.major);
        let version = hint.version.to_string();
        log.prove(|objects| {
            head.locate_release(hint.index, &id, &version, |path| objects.read(path))
        })
        .map_err(Into::into)
    }

    /// Verifies a covering historical head. A later usable head gives a conservative age.
    pub(crate) fn publication_time(
        &self,
        release: &Release,
        covering_size: Option<u64>,
        now: u64,
    ) -> Result<u64> {
        let head = release.checkpoint();
        let log = self.release_log(head)?;
        head.check_fresh(now)?;
        if self.route.preview() {
            return Ok(release.time());
        }
        let historical = if let Some(size) = covering_size {
            if !(release.index() + 1..=head.size()).contains(&size) {
                return Err(Error::ReleaseClaim.into());
            }
            let path = format!("heads/{size}");
            let cached = log.cached(&path, NOTE_LIMIT)?;
            let from_disk = cached.is_some();
            let bytes = if from_disk || self.transport.is_offline() {
                cached
            } else {
                self.read(&format!("log/{path}"), NOTE_LIMIT)?
            };
            if let Some(bytes) = bytes {
                let earlier = log.trust().authenticate(&bytes)?;
                if earlier.size() != size {
                    return Err(Error::ReleaseClaim.into());
                }
                if earlier
                    .witness_times()
                    .is_some_and(|times| times.into_iter().any(|time| time > now))
                {
                    return Err(Error::WitnessTime.into());
                }
                log.record_conflict(
                    log.prove(|objects| head.extends(&earlier, |path| objects.read(path))),
                )?;
                if !from_disk {
                    log.store_object(&path, &bytes)?;
                }
                Some(earlier)
            } else {
                None
            }
        } else {
            None
        };
        let times = historical
            .as_ref()
            .unwrap_or(head)
            .witness_times()
            .ok_or(Error::Trust)?;
        if times.into_iter().any(|time| time > now) {
            return Err(Error::WitnessTime.into());
        }
        Ok(release.time().max(times[0]).max(times[1]))
    }

    pub(crate) fn status_replay<R: Borrow<Release>>(
        &self,
        now: u64,
        mut release: impl FnMut(Root, &[u8]) -> Result<R, Error>,
    ) -> Result<StatusReplay> {
        let log = self.log(Log::Status);
        let _guard = if self.transport.is_offline() {
            None
        } else {
            Some(log.lock()?)
        };
        let mut replay = match log.saved()? {
            None => StatusReplay::default(),
            Some((head, snapshot)) => StatusReplay::restore(head, &snapshot)?,
        };
        if self.transport.is_offline() {
            replay
                .checkpoint()
                .ok_or("--frozen needs a verified status replay")?
                .check_fresh(now)?;
            return Ok(replay);
        }
        let head = log.current(replay.checkpoint(), now)?;
        if replay
            .checkpoint()
            .is_some_and(|prior| prior.note() == head.note())
        {
            return Ok(replay);
        }
        let result = log.prove(|objects| {
            replay.advance(
                &head,
                now,
                |path| objects.read(path),
                |root, digest| {
                    let bytes = self
                        .evidence(digest, 1 << 20)
                        .map_err(|_| Error::MissingEvidence)?;
                    let release = release(root, &bytes)?;
                    let verified = release.borrow();
                    let route = self
                        .routes
                        .for_name(&verified.id().name)
                        .map_err(|_| Error::Trust)?;
                    if route.origin() != self.route.origin()
                        || route.trust_digest() != self.route.trust_digest()
                        || !self.route.trust(Log::Release).owns(verified.checkpoint())
                    {
                        return Err(Error::Trust);
                    }
                    verified.checkpoint().check_fresh(now)?;
                    Ok((release, bytes))
                },
            )
        });
        log.record_conflict(result)?;
        log.save(&head, &replay.snapshot()?)?;
        Ok(replay)
    }

    pub(crate) fn evidence(&self, digest: Hash, limit: usize) -> Result<Vec<u8>> {
        let limit = limit.min(EVIDENCE_LIMIT);
        let path = format!("e/{}", hex(&digest));
        // e/ objects are public and immutable. Their digest is their authentication.
        let cache = self.log(Log::Status).directory.join("objects").join(&path);
        if let Some(bytes) = read_checked(&cache, limit)?
            && Hash::from(Sha256::digest(&bytes)) == digest
        {
            return Ok(bytes);
        }
        let bytes = self.read(&path, limit)?.ok_or(Error::MissingEvidence)?;
        if Hash::from(Sha256::digest(&bytes)) != digest {
            return Err(Error::MissingEvidence.into());
        }
        write_checked(&cache, &bytes)?;
        Ok(bytes)
    }

    fn require_name(&self, name: &str) -> Result<()> {
        let route = self.routes.for_name(name)?;
        if route.origin() != self.route.origin()
            || route.trust_digest() != self.route.trust_digest()
        {
            return Err(Error::Trust.into());
        }
        Ok(())
    }

    fn release_log(&self, head: &Checkpoint) -> Result<LogCache<'_>> {
        let log = self.log(Log::Release);
        if !log.trust().owns(head) {
            return Err(Error::Trust.into());
        }
        Ok(log)
    }

    fn log(&self, log: Log) -> LogCache<'_> {
        let origin = Sha256::digest(self.route.origin().to_string());
        let directory = self
            .cache
            .join("registry")
            .join(hex(&origin))
            .join(log.prefix())
            .join(hex(&self.route.trust_digest()));
        LogCache {
            client: self,
            log,
            directory,
        }
    }

    fn read(&self, path: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let Some(mut body) = self.get(path)? else {
            return Ok(None);
        };
        if body.content_length().is_some_and(|len| len > limit as u64) {
            return Err(format!("registry object {path} exceeds {limit} bytes").into());
        }
        let mut bytes = Vec::new();
        body.as_reader()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(format!("registry object {path} exceeds {limit} bytes").into());
        }
        Ok(Some(bytes))
    }

    fn get(&self, path: &str) -> Result<Option<ureq::Body>> {
        let agent = self.transport.agent().ok_or("--frozen does not fetch")?;
        let url = format!("{}/{path}", self.route.origin());
        let response = agent
            .get(&url)
            .config()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(|error| format!("fetching {url}: {error}"))?;
        match response.status().as_u16() {
            200 => Ok(Some(response.into_body())),
            404 => {
                let mut body = response.into_body();
                if body.content_length() == Some(0) {
                    let _ = body.as_reader().read(&mut [0])?;
                }
                Ok(None)
            }
            410 if path.starts_with("c/") => {
                let mut reason = Vec::new();
                response
                    .into_body()
                    .as_reader()
                    .take(4096)
                    .read_to_end(&mut reason)?;
                Err(format!(
                    "fetching {url}: HTTP 410 (taken down): {}",
                    String::from_utf8_lossy(&reason)
                )
                .into())
            }
            status => Err(format!("fetching {url}: HTTP {status}").into()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Hint {
    pub(crate) version: Version,
    pub(crate) index: u64,
    pub(crate) covering_size: u64,
}

struct Counted<F>(F);

impl<F: FnMut(&mut [u8]) -> io::Result<usize>> Read for Counted<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (self.0)(buf)
    }
}

struct LogCache<'a> {
    client: &'a Client<'a>,
    log: Log,
    directory: PathBuf,
}

impl LogCache<'_> {
    fn trust(&self) -> &Trust {
        self.client.route.trust(self.log)
    }

    fn lock(&self) -> io::Result<File> {
        ensure_directory(&self.directory)?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("lock"))?;
        file.lock()?;
        Ok(file)
    }

    fn saved(&self) -> Result<Option<(Checkpoint, Vec<u8>)>> {
        let path = self.directory.join("verified");
        let Some(payload) = read_optional(&path, u64::MAX)? else {
            return Ok(None);
        };
        let invalid = || format!("{}: invalid verified registry state", path.display());
        let payload = checked_payload(&payload).ok_or_else(invalid)?;
        let (&len, bytes) = payload.split_first_chunk::<4>().ok_or_else(invalid)?;
        let (note, snapshot) = bytes
            .split_at_checked(u32::from_be_bytes(len) as usize)
            .ok_or_else(invalid)?;
        let head = self.trust().authenticate(note)?;
        Ok(Some((head, snapshot.to_vec())))
    }

    fn current(&self, previous: Option<&Checkpoint>, now: u64) -> Result<Checkpoint> {
        let path = format!("{}/checkpoint", self.log.prefix());
        let note = self
            .client
            .read(&path, NOTE_LIMIT)?
            .ok_or(Error::MissingObject)?;
        let checked = self.prove(|objects| {
            self.trust()
                .accept(&note, previous, now, |path| objects.read(path))
        });
        self.record_conflict(checked)
    }

    fn save(&self, head: &Checkpoint, snapshot: &[u8]) -> io::Result<()> {
        let mut payload = (head.note().len() as u32).to_be_bytes().to_vec();
        payload.extend_from_slice(head.note());
        payload.extend_from_slice(snapshot);
        write_checked(&self.directory.join("verified"), &payload)
    }

    fn record_conflict<T>(&self, result: Result<T, Error>) -> Result<T> {
        if let Err(Error::Conflict { prior, candidate }) = &result {
            let mut notes = (prior.len() as u32).to_be_bytes().to_vec();
            notes.extend(prior);
            notes.extend(candidate);
            let path = self
                .directory
                .join("conflicts")
                .join(hex(&Sha256::digest(&notes)));
            write_checked(&path, &notes)?;
        }
        Ok(result?)
    }

    fn prove<T>(
        &self,
        mut verify: impl FnMut(&mut Objects<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut objects = Objects {
            log: self,
            bypass: false,
            used_cache: false,
        };
        let result = verify(&mut objects);
        if objects.used_cache
            && !self.client.transport.is_offline()
            && matches!(
                result,
                Err(Error::Conflict { .. }
                    | Error::Tile
                    | Error::Bundle
                    | Error::MerkleRoot
                    | Error::Entry
                    | Error::StatusTransition)
            )
        {
            objects.bypass = true;
            return verify(&mut objects);
        }
        result
    }

    fn cached(&self, path: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        read_checked(&self.directory.join("objects").join(path), limit)
    }

    fn store_object(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        write_checked(&self.directory.join("objects").join(path), bytes)
    }

    fn download(&self, path: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let url_path = format!("{}/{path}", self.log.prefix());
        let bytes = self.client.read(&url_path, limit)?;
        if let Some(bytes) = &bytes {
            self.store_object(path, bytes)?;
        }
        Ok(bytes)
    }
}

struct Objects<'a> {
    log: &'a LogCache<'a>,
    bypass: bool,
    used_cache: bool,
}

impl Objects<'_> {
    fn read(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        self.tile(path)
            .map_err(|_| Error::MissingObject)?
            .ok_or(Error::MissingObject)
    }

    fn tile(&mut self, path: &str) -> Result<Option<Vec<u8>>> {
        let limit = if path.starts_with("tile/entries/") {
            BUNDLE_LIMIT
        } else {
            256 * 32
        };
        let full = path.split_once(".p/").map(|(full, _)| full);
        if !self.bypass {
            for path in std::iter::once(path).chain(full) {
                if let Some(bytes) = self.log.cached(path, limit)? {
                    self.used_cache = true;
                    return Ok(Some(bytes));
                }
            }
        }
        if self.log.client.transport.is_offline() {
            return Ok(None);
        }
        let bytes = self.log.download(path, limit)?;
        match (bytes, full) {
            (None, Some(full)) => self.log.download(full, limit),
            (bytes, _) => Ok(bytes),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    hirsel_core_ids::hex::encode(bytes)
}

fn checked_payload(bytes: &[u8]) -> Option<&[u8]> {
    let (digest, payload) = bytes.split_first_chunk::<32>()?;
    (*digest == Hash::from(Sha256::digest(payload))).then_some(payload)
}

fn read_optional(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn read_checked(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    Ok(read_optional(path, limit as u64 + 33)?
        .as_deref()
        .and_then(checked_payload)
        .filter(|bytes| bytes.len() <= limit)
        .map(<[u8]>::to_vec))
}

fn write_checked(path: &Path, bytes: &[u8]) -> io::Result<()> {
    ensure_directory(path.parent().unwrap())?;
    let mut payload = Vec::with_capacity(32 + bytes.len());
    payload.extend_from_slice(&Sha256::digest(bytes));
    payload.extend_from_slice(bytes);
    crate::fs::atomic_write(path, &payload)
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all(),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_directory(path.parent().unwrap())?;
            ensure_directory(path)
        }
        Err(error) => Err(error),
    }
}

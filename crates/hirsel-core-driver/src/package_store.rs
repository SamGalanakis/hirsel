use crate::package_content::{
    ARCHIVE_LIMIT, CONTENT_LIMIT, Content, Id, Library, PackageArchive, Root,
};
use crate::package_files::{FileId, open_directory, walk};
use crate::package_manifest::SourceLocation;
use crate::registry_route::{Origin, Routes};
use bytes::BytesMut;
use rustix::fs::{Mode, OFlags, mkdirat, openat};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub(crate) struct PackageStore {
    path: PathBuf,
    pub(crate) transport: Transport,
    routes: Arc<OnceLock<Result<Routes, String>>>,
    /// The commits this command has fetched (GIT-6).
    pub(crate) session: crate::package_git::Session,
}

pub(crate) enum Entry {
    Present(Library),
    Missing,
    Damaged,
}

impl PackageStore {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        Self::with_transport(path, Transport::online())
    }

    pub(crate) fn with_transport(path: Option<PathBuf>, transport: Transport) -> Self {
        Self {
            transport,
            routes: Arc::new(OnceLock::new()),
            path: path.unwrap_or_else(|| {
                crate::PackageProduct::default_cache().with_file_name("packages")
            }),
            session: crate::package_git::Session::default(),
        }
    }

    pub(crate) fn read(&self, root: Root) -> io::Result<Entry> {
        self.read_batch(&[root])
            .remove(&root)
            .expect("requested entry")
    }

    /// Independent captures may finish out of order. The resolver consumes their results
    /// in graph order, including I/O and validation errors.
    pub(crate) fn read_batch(&self, roots: &[Root]) -> BTreeMap<Root, io::Result<Entry>> {
        let workers = if roots.len() < 4 {
            1
        } else {
            hirsel_guard::jobs().min(4)
        };
        let chunk = roots.len().div_ceil(workers).max(4);
        if roots.len() <= chunk {
            return read_entries(&self.path, roots);
        }
        std::thread::scope(|scope| {
            let workers: Vec<_> = roots
                .chunks(chunk)
                .map(|roots| scope.spawn(|| read_entries(&self.path, roots)))
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().expect("package store reader panicked"))
                .collect()
        })
    }

    pub(crate) fn install(&self, archive: &PackageArchive) -> io::Result<()> {
        for (root, library) in &archive.entries {
            self.install_entry(*root, &library.encoded)?;
        }
        Ok(())
    }

    pub(crate) fn install_entry(&self, root: Root, encoded: &[u8]) -> io::Result<bool> {
        fs::create_dir_all(&self.path)?;
        let directory = open_directory(&self.path)?;
        let path = entry_path(root);
        let shard = path.parent().unwrap();
        match mkdirat(&directory, shard, Mode::from_raw_mode(0o777)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let shard = open_shard(&directory, shard)?;
        let name = path.file_name().unwrap();
        if let Ok(mut file) = open_entry(&shard, name)
            && file.metadata()?.is_file()
        {
            let mut buffer = [0; 16384];
            if file.metadata()?.len() == encoded.len() as u64
                && encoded.chunks(buffer.len()).all(|chunk| {
                    let bytes = &mut buffer[..chunk.len()];
                    file.read_exact(bytes).is_ok() && bytes == chunk
                })
            {
                return Ok(false);
            }
        }
        let path = crate::host::fd_path(&shard)?.join(name);
        crate::fs::atomic_write(&path, encoded)?;
        directory.sync_all()?;
        Ok(true)
    }

    pub(crate) fn output_alias(
        &self,
        parent: FileId,
        name: &std::ffi::OsStr,
        archive: &PackageArchive,
    ) -> bool {
        archive.entries.keys().any(|root| {
            let relative = entry_path(*root);
            relative.file_name() == Some(name)
                && self
                    .path
                    .join(relative.parent().unwrap())
                    .metadata()
                    .is_ok_and(|dir| FileId::of(&dir) == parent)
        })
    }

    pub(crate) fn identities_include(&self, id: FileId) -> io::Result<bool> {
        let files = match fs::read_dir(&self.path) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        for shard in files {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || !hex_name(&shard.file_name(), 2) {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                if hex_name(&entry.file_name(), 62)
                    && fs::metadata(entry.path()).is_ok_and(|meta| FileId::of(&meta) == id)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn registry(
        &self,
        name: &str,
        pin: Option<&Origin>,
    ) -> Result<crate::registry_fetch::Client<'_>, String> {
        let routes = self
            .routes
            .get_or_init(Routes::load)
            .as_ref()
            .map_err(Clone::clone)?;
        let route = match pin {
            Some(origin) => routes.for_pin(name, origin)?,
            None => routes.for_name(name)?,
        };
        Ok(crate::registry_fetch::Client::new(
            routes,
            route,
            &self.transport,
            &self.path,
        ))
    }

    pub(crate) fn registry_frame(
        &self,
        frame: &crate::package_resolve::Frame<'_>,
        receive: impl FnMut(usize) -> io::Result<()>,
    ) -> Result<(Library, String), Box<dyn std::error::Error + Send + Sync>> {
        if self.transport.is_offline() {
            return Err("--frozen does not fetch".into());
        }
        let carrier = self.registry(&frame.carrier.name, Some(frame.origin))?;
        let routed;
        let client = if let Some(id) = frame.floor {
            routed = self.registry(&id.name, None)?;
            &routed
        } else {
            &carrier
        };
        let library = client.frame(frame.id, frame.root, receive)?;
        Ok((library, format!("registry:{}", client.route().origin())))
    }

    pub(crate) fn fetch_frames(
        &self,
        frames: &[crate::package_resolve::Frame<'_>],
        fetched: &mut BTreeSet<String>,
    ) -> Result<crate::package_resolve::Libraries, Box<dyn std::error::Error>> {
        let mut libraries = crate::package_resolve::Libraries::new();
        for batch in frames.chunks(8) {
            let results = std::thread::scope(|scope| {
                let jobs: Vec<_> = batch
                    .iter()
                    .map(|frame| scope.spawn(|| self.registry_frame(frame, |_| Ok(()))))
                    .collect();
                jobs.into_iter()
                    .map(|job| job.join().expect("registry frame worker"))
                    .collect::<Vec<_>>()
            });
            for (frame, result) in batch.iter().zip(results) {
                let (library, source) =
                    result.map_err(|error| -> Box<dyn std::error::Error> { error })?;
                self.install_entry(frame.root, &library.encoded)
                    .map_err(|error| format!("{}: admitting {source}: {error}", frame.id))?;
                libraries.insert(frame.root, library);
                fetched.insert(source);
            }
        }
        Ok(libraries)
    }

    /// Fetches `source`'s bundle for `id` pinned to `pin`, verifies it and admits every entry.
    /// Each source contacted, nested ones a git snapshot declares included, joins `fetched`.
    pub(crate) fn fetch(
        &self,
        directory: &File,
        id: &Id,
        pin: Root,
        source: &SourceLocation,
        fetched: &mut BTreeSet<String>,
    ) -> Result<(PackageArchive, Option<FileId>), Box<dyn std::error::Error>> {
        if self.transport.is_offline() {
            return Err("--frozen does not fetch".into());
        }
        let spelling = source.to_string();
        let (archive, identity) = match source {
            SourceLocation::Archive(path) => {
                let file = walk(directory, Path::new(path))
                    .and_then(|file| file.read_vec(ARCHIVE_LIMIT))
                    .map_err(|error| {
                        format!(
                            "{id}: {source} {}",
                            error.explain("workspace", "a regular file")
                        )
                    })?;
                (decode(id, pin, &spelling, file.0)?, Some(file.1))
            }
            SourceLocation::Url(url) => {
                let (archive, _) =
                    fetch_https(&self.transport, url, &mut |_| Ok(()), &format!("{id}: "))?;
                archive.verify(id, pin, &spelling)?;
                (archive, None)
            }
            SourceLocation::Registry(origin) => {
                let client = self.registry(&id.name, Some(origin))?;
                let library = client
                    .frame(id, pin, |_| Ok(()))
                    .map_err(|error| -> Box<dyn std::error::Error> { error })?;
                (
                    PackageArchive {
                        head: pin,
                        entries: [(pin, library)].into(),
                    },
                    None,
                )
            }
            SourceLocation::Git(git) => {
                let mut budget = crate::package_git::Budget::new(format!("{id}: {spelling}"));
                let acquired = crate::package_git::pack_commit(
                    self,
                    &self.session,
                    &mut budget,
                    id,
                    pin,
                    git,
                )?;
                fetched.extend(acquired.fetched);
                (acquired.archive, None)
            }
        };
        self.install(&archive)
            .map_err(|error| format!("{id}: admitting {source}: {error}\n  cache: partial"))?;
        fetched.insert(spelling);
        Ok((archive, identity))
    }
}

enum StoreFile {
    Readable(File, usize),
    Missing,
    Damaged,
}

fn open_store_file(path: &Path, directory: &mut Option<File>, root: Root) -> io::Result<StoreFile> {
    let directory = match directory {
        Some(file) => file,
        None => match open_directory(path) {
            Ok(file) => directory.insert(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StoreFile::Missing),
            Err(error) => return Err(error),
        },
    };
    let path = entry_path(root);
    let file = match open_shard(directory, path.parent().unwrap())
        .and_then(|shard| open_entry(&shard, path.file_name().unwrap()))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StoreFile::Missing),
        Err(error) if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) => {
            return Err(error);
        }
        Err(_) => return Ok(StoreFile::Damaged),
    };
    let metadata = file.metadata()?;
    if metadata.is_file() && metadata.len() <= CONTENT_LIMIT as u64 {
        Ok(StoreFile::Readable(file, metadata.len() as usize))
    } else {
        Ok(StoreFile::Damaged)
    }
}

fn read_entries(path: &Path, roots: &[Root]) -> BTreeMap<Root, io::Result<Entry>> {
    let mut results = BTreeMap::new();
    let mut directory = None;
    let mut files = Vec::new();
    let mut total = 0;
    for &root in roots {
        let mut opened = open_store_file(path, &mut directory, root);
        if opened
            .as_ref()
            .is_err_and(|error| matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)))
            && !files.is_empty()
        {
            capture_entries(&mut files, total, &mut results);
            total = 0;
            opened = open_store_file(path, &mut directory, root);
        }
        let result = match opened {
            Ok(StoreFile::Readable(file, length)) => {
                if total + length + 1 > 8 << 20 {
                    capture_entries(&mut files, total, &mut results);
                    total = 0;
                }
                files.push((root, file, length));
                total += length + 1;
                continue;
            }
            Ok(StoreFile::Missing) => Ok(Entry::Missing),
            Ok(StoreFile::Damaged) => Ok(Entry::Damaged),
            Err(error) => Err(error),
        };
        results.insert(root, result);
    }
    capture_entries(&mut files, total, &mut results);
    results
}

fn capture_entries(
    files: &mut Vec<(Root, File, usize)>,
    total: usize,
    results: &mut BTreeMap<Root, io::Result<Entry>>,
) {
    if files.is_empty() {
        return;
    }
    let mut buffer = BytesMut::with_capacity(total);
    crate::host::prefault_capture(&mut buffer.spare_capacity_mut()[..total]);
    for (root, file, length) in files.drain(..) {
        let tail = buffer.split_off(length + 1);
        results.insert(root, Ok(capture(file, length, root, buffer)));
        buffer = tail;
    }
}

fn capture(file: File, length: usize, root: Root, mut buffer: BytesMut) -> Entry {
    let needed = length + 1;
    let spare = &mut buffer.spare_capacity_mut()[..needed];
    let mut read = 0;
    while read < needed {
        match rustix::io::read(&file, &mut spare[read..]) {
            Ok(([], _)) => break,
            Ok((filled, _)) => read += filled.len(),
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Entry::Damaged,
        }
    }
    if read != length {
        return Entry::Damaged;
    }
    // SAFETY: read initialized exactly the first length bytes, leaving the EOF probe.
    unsafe { buffer.set_len(length) };
    let encoded = buffer.freeze();
    if Root::of(&encoded) != root {
        return Entry::Damaged;
    }
    match Content::decode_as(&encoded, false) {
        Ok(content) => Entry::Present(Library { content, encoded }),
        Err(_) => Entry::Damaged,
    }
}

/// Decodes and verifies a fetched archive's bytes (section 7 steps 1 to 3).
pub(crate) fn decode(
    id: &Id,
    pin: Root,
    source: &str,
    bytes: Vec<u8>,
) -> Result<PackageArchive, String> {
    let archive = PackageArchive::decode_as(bytes.into(), false)
        .map_err(|error| format!("{id}: {source} is not a package archive: {error}"))?;
    archive.verify(id, pin, source)?;
    Ok(archive)
}

fn open_shard(directory: &File, name: &Path) -> io::Result<File> {
    Ok(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}

fn open_entry(shard: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    Ok(openat(
        shard,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}

fn entry_path(root: Root) -> PathBuf {
    let hex = hirsel_core_ids::hex::encode(&root.0);
    Path::new(&hex[..2]).join(&hex[2..])
}

fn hex_name(name: &std::ffi::OsStr, len: usize) -> bool {
    name.as_encoded_bytes().len() == len
        && name
            .as_encoded_bytes()
            .iter()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A request's authority to use HTTP and its lazy connection pool.
#[derive(Clone)]
pub(crate) struct Transport(Option<Arc<OnceLock<ureq::Agent>>>);

impl Transport {
    pub(crate) fn offline() -> Self {
        Self(None)
    }

    pub(crate) fn online() -> Self {
        Self(Some(Arc::new(OnceLock::new())))
    }

    #[cfg(test)]
    pub(crate) fn with_agent(agent: ureq::Agent) -> Self {
        Self(Some(Arc::new(OnceLock::from(agent))))
    }

    pub(crate) fn is_offline(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn agent(&self) -> Option<&ureq::Agent> {
        Some(self.0.as_ref()?.get_or_init(http_agent))
    }
}

/// The host's roots, the proxy from the environment, and the package source stall bound.
fn http_agent() -> ureq::Agent {
    use ureq::tls::{Certificate, RootCerts, TlsConfig};
    let system_roots = rustls_native_certs::load_native_certs();
    let roots = RootCerts::from(
        system_roots
            .certs
            .iter()
            .map(|der| Certificate::from_der(der.as_ref()).to_owned()),
    );
    let tls = TlsConfig::builder()
        .root_certs(roots)
        .unversioned_rustls_crypto_provider(rustls::crypto::ring::default_provider().into())
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_per_read(Some(std::time::Duration::from_secs(60)))
        .build()
        .new_agent()
}

/// One GET of a URL source, following only HTTPS redirects. The archive reader
/// inflates and hashes each entry directly from the bounded response body.
pub(crate) fn fetch_https(
    transport: &Transport,
    url: &str,
    receive: &mut dyn FnMut(usize) -> Result<(), String>,
    who: &str,
) -> Result<(PackageArchive, usize), String> {
    let agent = transport.agent().ok_or("--frozen does not fetch")?;
    let mut response = agent
        .get(url)
        .config()
        .https_only(true)
        .build()
        .call()
        .map_err(|error| {
            let reason = if matches!(error, ureq::Error::Timeout(_)) {
                "the server sent nothing for 60 s".to_owned()
            } else {
                error.to_string()
            };
            format!("{who}fetching {url} failed: {reason}")
        })?;
    if response
        .body()
        .content_length()
        .is_some_and(|len| len > ARCHIVE_LIMIT as u64)
    {
        return Err(format!(
            "{who}fetching {url} failed: over the limit of {ARCHIVE_LIMIT} bytes"
        ));
    }

    let mut reader = response.body_mut().as_reader();
    let mut failure = None;
    let body = CountedBody {
        reader: &mut reader,
        receive,
        failure: &mut failure,
    };
    let archive = PackageArchive::read(body, false);
    match failure {
        Some(error) => Err(format!("{who}fetching {url} failed: {error}")),
        None => {
            archive.map_err(|error| format!("{who}url:{url} is not a package archive: {error}"))
        }
    }
}

struct CountedBody<'a, R> {
    reader: R,
    receive: &'a mut dyn FnMut(usize) -> Result<(), String>,
    failure: &'a mut Option<String>,
}

impl<R: Read> Read for CountedBody<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.reader.read(bytes).and_then(|n| {
            (self.receive)(n).map_err(io::Error::other)?;
            Ok(n)
        });
        if let Err(error) = &result
            && error.kind() != io::ErrorKind::Interrupted
        {
            let message = error.to_string();
            *self.failure = Some(
                if error.kind() == io::ErrorKind::TimedOut
                    || message.contains("timeout")
                    || message.contains("timed out")
                {
                    "the server sent nothing for 60 s".into()
                } else {
                    message
                },
            );
        }
        result
    }
}

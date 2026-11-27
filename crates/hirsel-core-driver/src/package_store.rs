use crate::package_content::{
    ARCHIVE_LIMIT, CONTENT_LIMIT, Content, Id, Library, PackageArchive, Root,
};
use crate::package_files::{FileId, open_directory, walk};
use crate::package_manifest::SourceLocation;
use rustix::fs::{Mode, OFlags, mkdirat, openat};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub(crate) struct PackageStore {
    path: PathBuf,
    pub(crate) transport: Transport,
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
            path: path.unwrap_or_else(|| {
                crate::PackageProduct::default_cache().with_file_name("packages")
            }),
            session: crate::package_git::Session::default(),
        }
    }

    pub(crate) fn read(&self, root: Root) -> io::Result<Entry> {
        let directory = match open_directory(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
            Err(error) => return Err(error),
        };
        let path = entry_path(root);
        let shard = path.parent().unwrap();
        let name = path.file_name().unwrap();
        let file = match open_shard(&directory, shard).and_then(|shard| open_entry(&shard, name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
            Err(_) => return Ok(Entry::Damaged),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > CONTENT_LIMIT as u64 {
            return Ok(Entry::Damaged);
        }
        let length = metadata.len() as usize;
        // Reserve the EOF probe too, so a complete read cannot trigger another allocation.
        let mut bytes = Vec::with_capacity(length + 1);
        if file
            .take(length as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() != length
        {
            return Ok(Entry::Damaged);
        }
        if Root::of(&bytes) != root {
            return Ok(Entry::Damaged);
        }
        let encoded = bytes.into();
        match Content::decode_as(&encoded, false) {
            Ok(content) => Ok(Entry::Present(Library { content, encoded })),
            Err(_) => Ok(Entry::Damaged),
        }
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

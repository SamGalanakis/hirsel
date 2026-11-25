use crate::package_content::{
    ARCHIVE_LIMIT, CONTENT_LIMIT, Content, Id, Library, PackageArchive, Root,
};
use crate::package_files::{FileId, WalkError, open_directory, walk};
use crate::package_manifest::SourceLocation;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct PackageStore {
    path: PathBuf,
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
        Self {
            path: path.unwrap_or_else(|| {
                crate::PackageProduct::default_cache().with_file_name("packages")
            }),
            session: crate::package_git::Session::default(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn read(&self, root: Root) -> io::Result<Entry> {
        let directory = match open_directory(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
            Err(error) => return Err(error),
        };
        let (bytes, _) = match walk(&directory, Path::new(&root.hex()))
            .and_then(|file| file.read_vec(CONTENT_LIMIT))
        {
            Ok(captured) => captured,
            Err(WalkError::Missing) => return Ok(Entry::Missing),
            Err(WalkError::Unsupported) => return Err(io::ErrorKind::Unsupported.into()),
            Err(_) => return Ok(Entry::Damaged),
        };
        if Root::of(&bytes) != root {
            return Ok(Entry::Damaged);
        }
        match Content::decode(&bytes) {
            Ok(content) => Ok(Entry::Present(Library {
                content,
                encoded: bytes.into(),
            })),
            Err(_) => Ok(Entry::Damaged),
        }
    }

    pub(crate) fn install(&self, archive: &PackageArchive) -> io::Result<()> {
        fs::create_dir_all(&self.path)?;
        for (root, library) in &archive.entries {
            atomic_write(&self.path.join(root.hex()), &library.encoded)?;
        }
        Ok(())
    }

    pub(crate) fn install_entry(&self, root: Root, encoded: &[u8]) -> io::Result<bool> {
        fs::create_dir_all(&self.path)?;
        let directory = open_directory(&self.path)?;
        let name = root.hex();
        if let Ok((mut file, _)) =
            walk(&directory, Path::new(&name)).and_then(|entry| entry.reader())
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
        use std::os::fd::AsRawFd;
        let path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
        atomic_write(&path, encoded)?;
        Ok(true)
    }

    pub(crate) fn identities_include(&self, id: FileId) -> io::Result<bool> {
        let files = match fs::read_dir(&self.path) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        for entry in files {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .and_then(crate::package_content::parse_digest)
                .is_some()
                && fs::metadata(entry.path()).is_ok_and(|meta| FileId::of(&meta) == id)
            {
                return Ok(true);
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
                let bytes = fetch_https(url, &mut |_| Ok(()))
                    .map_err(|error| format!("{id}: fetching {url} failed: {error}"))?;
                (decode(id, pin, &spelling, bytes)?, None)
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
    let archive = PackageArchive::decode(bytes.into())
        .map_err(|error| format!("{id}: {source} is not a package archive: {error}"))?;
    archive.verify(id, pin, source)?;
    Ok(archive)
}

/// The HTTPS client every package fetch uses: the host's roots, the proxy from the
/// environment, `https` only, and no byte for 60 s refused (GIT-7's limits).
pub(crate) fn https_config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
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
        .https_only(true)
        .tls_config(tls)
        .timeout_per_read(Some(std::time::Duration::from_secs(60)))
}

/// One GET of a `url` source, following `https` redirects, capped at the archive limit.
/// `receive` counts each chunk as it arrives and may refuse it.
pub(crate) fn fetch_https(
    url: &str,
    receive: &mut dyn FnMut(usize) -> Result<(), String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let stalled = |error: &dyn std::error::Error| {
        let text = error.to_string();
        text.contains("timeout") || text.contains("timed out")
    };
    let mut response = https_config().build().new_agent().get(url).call().map_err(
        |error| -> Box<dyn std::error::Error> {
            if matches!(error, ureq::Error::Timeout(_)) {
                "the server sent nothing for 60 s".into()
            } else {
                error.into()
            }
        },
    )?;
    let mut reader = response.body_mut().as_reader();
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::TimedOut || stalled(&error) => {
                return Err("the server sent nothing for 60 s".into());
            }
            Err(error) => return Err(error.into()),
        };
        receive(n)?;
        if bytes.len() + n > ARCHIVE_LIMIT {
            return Err(format!("over the limit of {ARCHIVE_LIMIT} bytes").into());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(bytes)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

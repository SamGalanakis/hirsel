use crate::package_content::{
    ARCHIVE_LIMIT, CONTENT_LIMIT, Content, Id, Library, PackageArchive, Root,
};
use crate::package_files::{FileId, WalkError, open_directory, walk};
use crate::package_manifest::SourceLocation;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) struct PackageStore {
    path: PathBuf,
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
        let captured = match walk(&directory, Path::new(&root.hex()))
            .and_then(|file| file.read(CONTENT_LIMIT))
        {
            Ok(captured) => captured,
            Err(WalkError::Missing) => return Ok(Entry::Missing),
            Err(WalkError::Unsupported) => return Err(io::ErrorKind::Unsupported.into()),
            Err(_) => return Ok(Entry::Damaged),
        };
        if Root::of(&captured.bytes) != root {
            return Ok(Entry::Damaged);
        }
        match Content::decode(&captured.bytes) {
            Ok(content) => Ok(Entry::Present(Library {
                content,
                encoded: captured.bytes,
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

    pub(crate) fn fetch(
        &self,
        directory: &File,
        id: &Id,
        pin: Root,
        source: &SourceLocation,
    ) -> Result<PackageArchive, Box<dyn std::error::Error>> {
        let bytes = match source {
            SourceLocation::Archive(path) => {
                let file = walk(directory, Path::new(path))
                    .and_then(|file| file.read(ARCHIVE_LIMIT))
                    .map_err(|error| {
                        format!(
                            "{id}: {source} {}",
                            error.explain("workspace", "a regular file")
                        )
                    })?;
                file.bytes
            }
            SourceLocation::Url(url) => fetch_https(url)
                .map_err(|error| format!("{id}: fetching {url} failed: {error}"))?
                .into(),
        };
        let archive = PackageArchive::decode(&bytes)
            .map_err(|error| format!("{id}: {source} is not a package archive: {error}"))?;
        archive.verify(id, pin, &source.to_string())?;
        self.install(&archive)?;
        Ok(archive)
    }
}

fn fetch_https(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
    let agent = ureq::Agent::config_builder()
        .https_only(true)
        .tls_config(tls)
        .build()
        .new_agent();
    let mut response = agent.get(url).call()?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(ARCHIVE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > ARCHIVE_LIMIT {
        return Err(format!("over the limit of {ARCHIVE_LIMIT} bytes").into());
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

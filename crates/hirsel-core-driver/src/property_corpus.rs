//! Owner-only storage for the test runner's primary minimized property tapes.
use hirsel_core_ids::{ContentHash, DefPath};
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Output;

pub(crate) const ENV: &str = "HIRSEL_TEST_PROPERTY_CORPUS";
const MAX_INPUT: usize = 96 * 1024;
const MAX_FILE: usize = MAX_INPUT + 4096;
const VERSION: u32 = 2;
const MARKER: &str = "Fail(\"prop-corpus-v2 ";

pub(crate) struct Corpus {
    root: PathBuf,
    _lock: File,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    version: u32,
    test: String,
    pub launch_seed: Option<u64>,
    pub input: String,
    site: Option<String>,
}

pub(crate) enum SavedEntry {
    Missing,
    Stale,
    Current(Entry),
}

pub(crate) enum Report<'a> {
    Stale,
    Failure(Failure<'a>),
}

pub(crate) struct Failure<'a> {
    input: &'a str,
    site: Option<String>,
}

impl Entry {
    pub(crate) fn confirms(&self, failure: &Failure<'_>) -> bool {
        self.input == failure.input && self.site == failure.site
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("property corpus: {message}"),
    )
}

fn owned(metadata: &fs::Metadata) -> bool {
    // SAFETY: geteuid takes no arguments and has no preconditions.
    metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0
}

impl Corpus {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let lock = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?;
        if !owned(&lock.metadata()?) {
            return Err(invalid("directory must be accessible only to its owner"));
        }
        lock.lock()?;
        let root = path.canonicalize()?;
        let canonical = root.metadata()?;
        let opened = lock.metadata()?;
        if (canonical.dev(), canonical.ino()) != (opened.dev(), opened.ino()) {
            return Err(invalid("directory changed while opening it"));
        }
        Ok(Self { root, _lock: lock })
    }

    pub(crate) fn check_grants(&self, directories: &[String]) -> io::Result<()> {
        for directory in directories {
            if crate::package_dir::private_directory(directory) {
                continue;
            }
            let path = Path::new(directory);
            if !path.is_absolute() {
                return Err(invalid(
                    "directory grant can escape the private test directory",
                ));
            }
            match path.canonicalize() {
                Ok(grant) if self.root.starts_with(&grant) || grant.starts_with(&self.root) => {
                    return Err(invalid("directory grant overlaps the corpus"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn path(&self, test: &DefPath) -> PathBuf {
        let digest = ContentHash::of_bytes(test.to_string().as_bytes());
        self.root.join(format!("{digest}.json"))
    }

    pub(crate) fn read(&self, test: &DefPath) -> io::Result<SavedEntry> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.path(test))
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SavedEntry::Missing);
            }
            Err(error) => return Err(error),
        };
        let meta = file.metadata()?;
        if !meta.is_file() || !owned(&meta) || meta.nlink() != 1 {
            return Err(invalid("entry must be a private regular file"));
        }
        let mut bytes = Vec::new();
        file.take((MAX_FILE + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE {
            return Err(invalid("entry exceeds the size limit"));
        }
        let entry: Entry = serde_json::from_slice(&bytes)?;
        if entry.test != test.to_string() {
            return Err(invalid("invalid entry"));
        }
        if entry.version != VERSION {
            return Ok(SavedEntry::Stale);
        }
        if !valid_input(&entry.input, entry.site.is_some()) {
            return Err(invalid("invalid entry"));
        }
        Ok(SavedEntry::Current(entry))
    }

    pub(crate) fn write(
        &self,
        test: &DefPath,
        launch_seed: Option<u64>,
        failure: Failure<'_>,
    ) -> io::Result<()> {
        if !valid_input(failure.input, failure.site.is_some()) {
            return Err(invalid("invalid or oversized failure report"));
        }
        let entry = Entry {
            version: VERSION,
            test: test.to_string(),
            launch_seed,
            input: failure.input.to_owned(),
            site: failure.site,
        };
        let bytes = serde_json::to_vec(&entry)?;
        if bytes.len() > MAX_FILE {
            return Err(invalid("entry exceeds the size limit"));
        }
        let mut staged = tempfile::NamedTempFile::new_in(&self.root)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        staged.persist(self.path(test))?;
        self._lock.sync_all()
    }

    pub(crate) fn retire(&self, test: &DefPath) -> io::Result<()> {
        fs::remove_file(self.path(test))?;
        self._lock.sync_all()
    }
}

fn valid_input(input: &str, has_site: bool) -> bool {
    if input.len() > MAX_INPUT {
        return false;
    }
    let natural =
        |word: &str| word.parse::<u64>().is_ok() && word.bytes().all(|b| b.is_ascii_digit());
    let base64 = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    };
    let mut fields = input.split(' ');
    for index in 0..10 {
        if !fields
            .next()
            .is_some_and(|word| natural(word) || index == 2 && word == "-")
        {
            return false;
        }
    }
    fields.next().is_some_and(base64)
        && fields.next().is_some_and(base64)
        && fields.next().is_some_and(|word| {
            if has_site {
                word.strip_prefix("raised-").is_some_and(natural)
            } else {
                matches!(word, "returned" | "steps" | "bytes")
                    || word.strip_prefix("returned-").is_some_and(natural)
            }
        })
        && fields.next().is_none()
}

pub(crate) fn report<'a>(
    output: &'a Output,
    timed_out: bool,
    executable: &Path,
) -> Option<Report<'a>> {
    if timed_out || output.status.code() != Some(101) {
        return None;
    }
    // The native diagnostic escapes newlines within a failure's message.
    let line = std::str::from_utf8(&output.stderr)
        .ok()?
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(MARKER))?;
    if line.starts_with("stale\") at ") {
        return Some(Report::Stale);
    }
    let (input, _) = line.split_once(" | prop: falsified: ")?;
    let (input, word) = input
        .rsplit_once(" @")
        .map_or((input, None), |(input, word)| (input, Some(word)));
    let site = match word {
        Some(word) => Some(crate::native_entry::source_site(
            executable,
            word.parse().ok()?,
        )?),
        None => None,
    };
    Some(Report::Failure(Failure { input, site }))
}

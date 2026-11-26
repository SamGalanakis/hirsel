use super::{Error, ImageRef, Pending};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

const MARKER: &str = ".hirsel-dev-owned";

pub(super) struct Directory {
    pub root: PathBuf,
    pub store: PathBuf,
    store_dir: File,
    _dev_lock: File,
    _store_lock: File,
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Locator {
    pub active: Option<ImageRef>,
    pub pending: Option<Pending>,
}

fn open_dir(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

fn open_regular(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok(file)
}

fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: flock borrows a live descriptor and does not wait.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

fn lock(path: &Path, conflict: impl FnOnce(&str) -> String) -> Result<File, Error> {
    let mut file = open_regular(path, true)?;
    if !try_lock(&file)? {
        let mut pid = String::new();
        let _ = (&file).take(32).read_to_string(&mut pid);
        return Err(conflict(pid.trim()).into());
    }
    file.set_len(0)?;
    writeln!(file, "{}", std::process::id())?;
    Ok(file)
}

fn id(file: &File) -> io::Result<(u64, u64)> {
    let info = file.metadata()?;
    Ok((info.dev(), info.ino()))
}

fn decimal(text: &str) -> Option<u64> {
    let number: u64 = text.parse().ok()?;
    (number.to_string() == text && number != 0).then_some(number)
}

fn next_directory(parent: &Path) -> io::Result<(u64, PathBuf)> {
    let mut highest = 0;
    for entry in fs::read_dir(parent)? {
        if let Some(number) = entry?.file_name().to_str().and_then(decimal) {
            highest = highest.max(number);
        }
    }
    let number = highest
        .checked_add(1)
        .ok_or_else(|| io::Error::other("generation exhausted"))?;
    let path = parent.join(number.to_string());
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok((number, path))
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

pub(super) fn check_overlap(path: &Path, other: &Path) -> Result<(), Error> {
    if overlap(path, other) {
        return Err(format!("{} overlaps {}", path.display(), other.display()).into());
    }
    Ok(())
}

pub(super) fn resolve(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or(error)?;
            let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
            Ok(resolve(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn marker(dir: &File) -> io::Result<Vec<u8>> {
    let (device, inode) = id(dir)?;
    Ok(format!("hirsel dev store {device}:{inode}\n").into_bytes())
}

fn owned(dir: &File, path: &Path) -> io::Result<bool> {
    let mut bytes = Vec::new();
    open_regular(&path.join(MARKER), false)?
        .take(128)
        .read_to_end(&mut bytes)?;
    Ok(bytes == marker(dir)?)
}

fn current(stores: &Path) -> Result<Option<u64>, Error> {
    let file = match open_regular(&stores.join("current"), false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    file.take(32).read_to_string(&mut text)?;
    decimal(text.strip_suffix('\n').unwrap_or(&text))
        .map(Some)
        .ok_or_else(|| "invalid .hirsel-dev/stores/current".into())
}

impl Directory {
    pub(super) fn open(
        server: &Path,
        supplied: Option<&Path>,
        fresh: bool,
        sources: Option<&[PathBuf]>,
    ) -> Result<Self, Error> {
        let root_path = server.join(".hirsel-dev");
        let root = crate::launcher::private_dir(root_path)?;
        let dev_lock = lock(&root.join("dev.lock"), |pid| {
            format!(
                "hirsel dev is already running for {} (pid {pid})",
                server.display()
            )
        })?;
        if let Some(sources) = sources {
            for source in sources {
                for area in ["compiler", "gen", "scratch"] {
                    check_overlap(&root.join(area), source)?;
                }
            }
        }
        let compiler = crate::launcher::private_dir(root.join("compiler"))?;
        crate::launcher::private_dir(root.join("gen"))?;
        crate::launcher::private_dir(root.join("scratch"))?;
        let stores = crate::launcher::private_dir(root.join("stores"))?;
        let selected = current(&stores)?;
        let (store, store_dir) = if let Some(supplied) = supplied {
            let supplied = resolve(&std::path::absolute(supplied)?)?;
            check_overlap(&supplied, &compiler)?;
            check_overlap(&supplied, &root)?;
            for ancestor in server.ancestors() {
                check_overlap(&supplied, &ancestor.join(crate::package_dir::MANIFEST))?;
            }
            if let Some(sources) = sources {
                for source in sources {
                    check_overlap(&supplied, source)?;
                }
            }
            fs::create_dir_all(&supplied)?;
            let dir = open_dir(&supplied)?;
            if owned(&dir, &supplied).unwrap_or(false) {
                return Err(
                    format!("{} is a dev-owned store generation", supplied.display()).into(),
                );
            }
            (supplied, dir)
        } else if let Some(number) = selected.filter(|_| !fresh) {
            let path = stores.join(number.to_string());
            let dir = open_dir(&path)?;
            if !owned(&dir, &path)? {
                return Err(format!("{} is not a dev-owned store", path.display()).into());
            }
            (path, dir)
        } else {
            let (number, path) = next_directory(&stores)?;
            let dir = open_dir(&path)?;
            crate::fs::atomic_write(&path.join(MARKER), &marker(&dir)?)?;
            crate::fs::atomic_write(&stores.join("current"), format!("{number}\n").as_bytes())?;
            (path, dir)
        };
        if let Some(sources) = sources {
            for source in sources {
                check_overlap(&store, source)?;
            }
        }
        // SAFETY: geteuid reads the current process's effective uid.
        let uid = unsafe { libc::geteuid() };
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map_or_else(
                || Path::new("/tmp").join(format!("hirsel-dev-{uid}")),
                |path| path.join("hirsel-dev"),
            );
        let runtime = crate::launcher::private_dir(runtime)?;
        if open_dir(&runtime)?.metadata()?.uid() != uid {
            return Err("dev supervisor lock directory has another owner".into());
        }
        let (device, inode) = id(&store_dir)?;
        let store_lock = lock(&runtime.join(format!("{device}-{inode}.lock")), |pid| {
            format!(
                "hirsel dev is already supervising store {} (pid {pid})",
                store.display()
            )
        })?;
        let directory = Self {
            root,
            store,
            store_dir,
            _dev_lock: dev_lock,
            _store_lock: store_lock,
        };
        if !fresh && let (Some(sources), Some(selected)) = (sources, selected) {
            for entry in fs::read_dir(&stores)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_str()
                    .and_then(decimal)
                    .is_some_and(|n| n < selected)
                {
                    let path = entry.path();
                    if !sources
                        .iter()
                        .chain([&directory.store, &compiler])
                        .any(|source| overlap(&path, source))
                    {
                        // Ineligible generations are intentionally left in place.
                        let _ = retire(&path, &runtime);
                    }
                }
            }
        }
        Ok(directory)
    }

    pub(super) fn verify_path(&self) -> Result<(), Error> {
        if id(&open_dir(&self.store)?)? != id(&self.store_dir)? {
            return Err(
                format!("the store directory {} was replaced", self.store.display()).into(),
            );
        }
        Ok(())
    }

    pub(super) fn store_fd(&self) -> RawFd {
        self.store_dir.as_raw_fd()
    }

    pub(super) fn verify(&self, sources: &[PathBuf]) -> Result<(), Error> {
        self.verify_path()?;
        for source in sources {
            check_overlap(&self.store, source)?;
        }
        Ok(())
    }

    pub(super) fn allocate(&self) -> io::Result<(u64, PathBuf)> {
        next_directory(&self.root.join("gen")).map(|(n, path)| (n, path.join("place")))
    }

    pub(super) fn image(&self, number: u64) -> PathBuf {
        self.root.join("gen").join(number.to_string()).join("place")
    }

    pub(super) fn sync_image(&self, number: u64) -> io::Result<()> {
        let path = self.image(number);
        open_regular(&path, false)?.sync_all()?;
        open_dir(path.parent().expect("generation directory"))?.sync_all()?;
        open_dir(&self.root.join("gen"))?.sync_all()
    }

    pub(super) fn discard(&self, number: u64) -> io::Result<()> {
        let path = self.image(number);
        fs::remove_dir_all(path.parent().expect("generation directory"))
    }

    pub(super) fn load(&self) -> Result<Locator, Error> {
        let file = match open_regular(&self.root.join("state.json"), false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Locator::default()),
            Err(error) => return Err(error.into()),
        };
        let expected = id(&self.store_dir)?;
        let (store, inode, locator): (PathBuf, (u64, u64), Locator) =
            serde_json::from_reader(file.take(16 * 1024))?;
        if store == self.store && inode == expected {
            Ok(locator)
        } else {
            Ok(Locator::default())
        }
    }

    pub(super) fn save(&self, locator: &Locator) -> Result<(), Error> {
        crate::fs::atomic_write(
            &self.root.join("state.json"),
            &serde_json::to_vec(&(&self.store, id(&self.store_dir)?, locator))?,
        )?;
        Ok(())
    }
}

fn retire(path: &Path, runtime: &Path) -> io::Result<()> {
    let dir = open_dir(path)?;
    if !owned(&dir, path)? {
        return Ok(());
    }
    let (device, inode) = id(&dir)?;
    let supervisor = open_regular(&runtime.join(format!("{device}-{inode}.lock")), true)?;
    if !try_lock(&supervisor)? {
        return Ok(());
    }
    let lock = open_regular(&path.join("LOCK"), true)?;
    if !try_lock(&lock)? {
        return Ok(());
    }
    if id(&open_dir(path)?)? != (device, inode) || !owned(&dir, path)? {
        return Ok(());
    }
    // Detach the pathname before removing contents, so new writers cannot open
    // it. A file already opened under that path still sees the same LOCK.
    let retired = tempfile::Builder::new()
        .prefix(".retired-")
        .tempdir_in(path.parent().expect("store has a parent"))?;
    let detached = retired.path().join("store");
    fs::rename(path, &detached)?;
    if !open_dir(&detached)
        .and_then(|dir| id(&dir))
        .is_ok_and(|found| found == (device, inode))
    {
        // A replacement raced the rename. Keep it even if the old name has
        // since been recreated.
        let _ = retired.keep();
        return Err(io::Error::other("store replaced during retirement"));
    }
    fs::remove_dir_all(detached)
}

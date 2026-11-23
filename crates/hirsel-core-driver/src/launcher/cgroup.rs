use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use super::{hex, wait_pid};
use hirsel_contain::ChildId;

const MOUNT: &str = "/sys/fs/cgroup";

fn membership(path: &Path) -> io::Result<PathBuf> {
    let text = fs::read_to_string(path)?;
    let relative = text
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no cgroup v2 membership"))?;
    Ok(Path::new(MOUNT).join(relative))
}

/// The default uses a launcher-owned directory in the current delegated cgroup.
/// It has a stable name so a restart in the same delegation can recover it.
pub(super) fn delegated(state: &Path) -> io::Result<Option<PathBuf>> {
    let parent = match membership(Path::new("/proc/self/cgroup")) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => return Ok(None),
        Err(error) => return Err(error),
    };
    let state = fs::canonicalize(state)?;
    let key = Sha256::digest(state.as_os_str().as_encoded_bytes());
    let root = parent.join(format!("hirsel-{}", hex(&key)));
    match fs::create_dir(&root) {
        Ok(()) => Ok(Some(root)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && root.is_dir() => {
            Ok(Some(root))
        }
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EACCES | libc::EPERM | libc::EROFS | libc::ENOENT)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// A launcher with cgroups owns the orphans it kills, so it can reap them before
/// reporting Ended. The launcher runs in its own process.
pub(super) fn own_orphans() -> io::Result<()> {
    // SAFETY: this prctl only changes who adopts this process's descendants.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn recover(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .and_then(ChildId::parse)
                .is_some()
        {
            let group = Group::open(entry.path())?;
            group.kill()?;
            group.wait_empty()?;
            group.remove()?;
        }
    }
    Ok(())
}

pub(super) struct Group {
    path: PathBuf,
    events: File,
    identity: (u64, u64),
}

impl Group {
    pub(super) fn create(root: &Path, id: &ChildId) -> io::Result<Self> {
        let path = root.join(id.name());
        fs::create_dir(&path)?;
        Self::open(path.clone()).inspect_err(|_| {
            let _ = fs::remove_dir(path);
        })
    }

    fn open(path: PathBuf) -> io::Result<Self> {
        let events = File::open(path.join("cgroup.events"))?;
        let info = path.metadata()?;
        Ok(Self {
            path,
            events,
            identity: (info.dev(), info.ino()),
        })
    }

    pub(super) fn directory(&self) -> io::Result<File> {
        File::open(&self.path)
    }

    pub(super) fn event_fd(&self) -> RawFd {
        self.events.as_raw_fd()
    }

    pub(super) fn kill(&self) -> io::Result<()> {
        fs::write(self.path.join("cgroup.kill"), b"1")
    }

    pub(super) fn empty(&self) -> io::Result<bool> {
        let mut buffer = [0; 256];
        let count = self.events.read_at(&mut buffer, 0)?;
        for line in buffer[..count].split(|byte| *byte == b'\n') {
            match line {
                b"populated 0" => return Ok(true),
                b"populated 1" => return Ok(false),
                _ => {}
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing cgroup population",
        ))
    }

    pub(super) fn wait_empty(&self) -> io::Result<()> {
        while !self.empty()? {
            let mut ready = libc::pollfd {
                fd: self.event_fd(),
                events: libc::POLLPRI,
                revents: 0,
            };
            // SAFETY: poll borrows one live descriptor. The preceding read clears
            // stale readiness; a change after it wakes the wait.
            if unsafe { libc::poll(&mut ready, 1, -1) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// cgroup.events counts live tasks. Once it is empty, every remaining
    /// descendant is exiting or a zombie. Wait only for our adopted children
    /// of this cgroup, including children reparented by each exit.
    pub(super) fn reap_orphans(&self) -> io::Result<()> {
        let launcher = std::process::id();
        let children = format!("/proc/self/task/{launcher}/children");
        loop {
            let mut reaped = false;
            for pid in fs::read_to_string(&children)?.split_whitespace() {
                let path = format!("/proc/{pid}/cgroup");
                let membership = match membership(Path::new(&path)) {
                    Ok(path) => path,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                let mut within = false;
                for path in membership.ancestors() {
                    if path.file_name() != self.path.file_name() {
                        continue;
                    }
                    match path.metadata() {
                        Ok(info) if self.identity == (info.dev(), info.ino()) => {
                            within = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                if within {
                    let pid = pid.parse().map_err(io::Error::other)?;
                    wait_pid(pid)?;
                    reaped = true;
                }
            }
            if !reaped {
                return Ok(());
            }
        }
    }

    pub(super) fn remove(&self) -> io::Result<()> {
        remove_tree(&self.path)
    }
}

fn remove_tree(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_tree(&entry.path())?;
        }
    }
    fs::remove_dir(path)
}

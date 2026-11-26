use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use super::session::{Child, Role};
use super::{hex, wait_pid};

const MOUNT: &str = "/sys/fs/cgroup";

/// Process `pid`'s cgroup v2 directory; this process's when `pid` is `None`.
fn membership(pid: Option<libc::pid_t>) -> io::Result<PathBuf> {
    let text = crate::host::cgroup_membership(pid)?;
    let relative = text
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no cgroup v2 membership"))?;
    Ok(Path::new(MOUNT).join(relative))
}

/// The default uses a launcher-owned directory in the current delegated cgroup.
/// It has a stable name so a restart in the same delegation can recover it.
pub(super) fn delegated(state: &Path) -> io::Result<Option<PathBuf>> {
    let parent = match membership(None) {
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

pub(super) fn recover(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name().to_str().is_some_and(is_child_name) {
            let group = Group::open(entry.path())?;
            group.kill()?;
            group.wait_empty()?;
            group.remove()?;
        }
    }
    Ok(())
}

fn child_name(child: &Child) -> String {
    let (role, key) = match child.role {
        Role::Install(generation) => ("install", u128::from(generation)),
        Role::Serving(generation) => ("serving", u128::from(generation)),
        Role::Describe(attempt) => ("describe", u128::from_le_bytes(attempt.bytes())),
        Role::Preview(attempt) => ("preview", u128::from_le_bytes(attempt.bytes())),
        Role::Prepare(attempt) => ("prepare", u128::from_le_bytes(attempt.bytes())),
    };
    let group = hex(child.group.name.as_bytes());
    format!("c-{group}-{}-{key}-{role}", child.group.incarnation)
}

fn is_child_name(name: &str) -> bool {
    let mut parts = name.split('-');
    let (Some("c"), Some(group), Some(incarnation), Some(key), Some(role), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    if !matches!(
        role,
        "install" | "serving" | "describe" | "preview" | "prepare"
    ) {
        return false;
    }
    let (Some(bytes), Ok(incarnation), Ok(key)) = (
        crate::call::from_hex(group),
        incarnation.parse::<u64>(),
        key.parse::<u128>(),
    ) else {
        return false;
    };
    std::str::from_utf8(&bytes).is_ok()
        && format!("c-{}-{incarnation}-{key}-{role}", hex(&bytes)) == name
}

pub(super) struct Group {
    path: PathBuf,
    events: File,
    identity: (u64, u64),
}

impl Group {
    pub(super) fn create(root: &Path, child: &Child) -> io::Result<Self> {
        let path = root.join(child_name(child));
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
        let launcher = std::process::id() as libc::pid_t;
        loop {
            let mut reaped = false;
            for pid in crate::host::child_pids(launcher)? {
                let membership = match membership(Some(pid)) {
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

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use super::session::{Child, Role};
use super::wait_pid;
use hirsel_core_ids::{cgroup::resolve, hex};

/// Process `pid`'s cgroup v2 directory; this process's when `pid` is `None`.
fn membership(pid: Option<libc::pid_t>) -> io::Result<PathBuf> {
    let cgroup = crate::host::cgroup_membership(pid)?;
    resolve(&crate::host::mountinfo()?, &cgroup)
}

/// How the launcher tracks and reaps child process trees (launch.md "Child lifecycle").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Lifecycle {
    /// Require a delegated cgroup v2 subtree; fail startup if one cannot be opened.
    Managed,
    /// Track and reap only the direct child leader (pidfd on Linux, kqueue on macOS).
    Leader,
}

/// Open or create the launcher-owned directory in the current delegated cgroup v2.
/// It has a stable name so a restart in the same delegation can recover it.
/// Fails when the host has no delegatable cgroup v2 subtree (never falls back silently).
pub(super) fn delegated(state: &Path) -> io::Result<PathBuf> {
    let cgroup = crate::host::cgroup_membership(None).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("--lifecycle managed requires a delegated cgroup v2 subtree: {error}"),
        )
    })?;
    let mountinfo = crate::host::mountinfo().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("--lifecycle managed requires a delegated cgroup v2 subtree: {error}"),
        )
    })?;
    delegated_from(&mountinfo, &cgroup, state)
}

fn delegated_from(mountinfo: &str, cgroup: &str, state: &Path) -> io::Result<PathBuf> {
    let parent = resolve(mountinfo, cgroup).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("--lifecycle managed requires a delegated cgroup v2 subtree: {error}"),
        )
    })?;
    let state = fs::canonicalize(state)?;
    let key = Sha256::digest(state.as_os_str().as_encoded_bytes());
    let root = parent.join(format!("hirsel-{}", hex::encode(&key)));
    match fs::create_dir(&root) {
        Ok(()) => Ok(root),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && root.is_dir() => Ok(root),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "--lifecycle managed requires a delegated cgroup v2 subtree at {}: {error}",
                parent.display()
            ),
        )),
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
    let group = hex::encode(child.group.name.as_bytes());
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
        hex::decode(group),
        incarnation.parse::<u64>(),
        key.parse::<u128>(),
    ) else {
        return false;
    };
    std::str::from_utf8(&bytes).is_ok()
        && format!("c-{}-{incarnation}-{key}-{role}", hex::encode(&bytes)) == name
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

#[cfg(test)]
mod laws {
    use super::resolve;
    use std::io;
    use std::path::Path;

    /// The launcher's cgroup is found through the mount table (`/proc/self/mountinfo`),
    /// not at a fixed `/sys/fs/cgroup`: a cgroup2 hierarchy mounted at `/run/cg` with mount
    /// root `/sub` holds cgroup `/sub/a/b` at `/run/cg/a/b`. The longest root holding the
    /// membership wins, a root matches whole components only, an escaped space in a mount
    /// point is a space, a cgroup1 mount is not a candidate, and a membership outside every
    /// cgroup2 root is `NotFound` while no cgroup2 mount at all is `Unsupported`.
    #[test]
    fn law_cgroup_resolves_through_mountinfo() {
        let cgroup1 = "30 25 0:26 / /sys/fs/cgroup/cpu rw,nosuid shared:9 - cgroup cgroup rw,cpu";
        let relocated = "41 25 0:30 /sub /run/cg rw,nosuid shared:12 master:3 - cgroup2 cgroup2 rw";
        let resolved = |mounts: &[&str], cgroup: &str| resolve(&mounts.join("\n"), cgroup);
        assert_eq!(
            resolved(&[cgroup1, relocated], "0::/sub/a/b\n").unwrap(),
            Path::new("/run/cg/a/b")
        );
        assert_eq!(
            resolved(&[relocated], "0::/sub").unwrap(),
            Path::new("/run/cg")
        );
        let whole = "29 25 0:27 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw";
        let spaced = "42 25 0:30 /sub/a /mnt/c\\040g rw - cgroup2 cgroup2 rw";
        assert_eq!(
            resolved(&[whole, spaced], "0::/sub/a/b").unwrap(),
            Path::new("/mnt/c g/b")
        );
        assert_eq!(
            resolved(&[whole, relocated], "0::/subx/a").unwrap(),
            Path::new("/sys/fs/cgroup/subx/a")
        );
        assert_eq!(
            resolved(&[relocated], "0::/subx/a").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            resolved(&[cgroup1], "0::/a").unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            resolved(&[whole], "1:cpu:/a").unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    /// `--lifecycle managed` fails startup with the stated error when a delegatable
    /// cgroup v2 subtree cannot be opened (no cgroup2 mount, missing or read-only
    /// directory), instead of falling back silently to leader-only tracking.
    /// `--lifecycle leader` succeeds without probing cgroups.
    #[test]
    fn law_managed_lifecycle_fails_without_delegation() {
        use super::super::{Image, Images, Launcher, Lifecycle};
        use super::delegated_from;
        use std::collections::BTreeSet;
        use std::os::unix::fs::PermissionsExt;

        struct NoImages;
        impl Images for NoImages {
            fn resolve(&self, _: [u8; 32]) -> Result<Image, String> {
                Err("unused".into())
            }

            fn collect(
                &self,
                _: &BTreeSet<[u8; 32]>,
                _: &dyn Fn(crate::code_admission::ObjectKey) -> bool,
            ) -> io::Result<u64> {
                Ok(0)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();

        let assert_managed_fails = |mountinfo: &str, cgroup: &str| {
            let mut opened = false;
            let error = Launcher::new_with(
                &state,
                Lifecycle::Managed,
                || {
                    opened = true;
                    Ok(NoImages)
                },
                |state| delegated_from(mountinfo, cgroup, state),
            )
            .err()
            .expect("managed lifecycle must fail startup without a delegatable cgroup");
            assert!(
                !opened,
                "managed failure must stop startup before opening stores"
            );
            let message = error.to_string();
            assert!(
                message.contains("--lifecycle managed") && message.contains("delegated cgroup v2"),
                "unexpected error: {message}"
            );
        };

        // 1. No cgroup2 mount (Unsupported): parent commit fell back silently to Ok(None).
        let cgroup1 = "30 25 0:26 / /sys/fs/cgroup/cpu rw,nosuid shared:9 - cgroup cgroup rw,cpu";
        assert_managed_fails(cgroup1, "0::/a");

        // 2. Nonexistent cgroup directory (ENOENT): parent commit fell back silently to Ok(None).
        let missing = dir.path().join("absent-cgroup");
        let missing_mount = format!("41 25 0:30 / {} rw - cgroup2 cgroup2 rw", missing.display());
        assert_managed_fails(&missing_mount, "0::/sub");

        // 3. Unwritable cgroup directory (EACCES for non-root): parent commit fell back silently.
        // SAFETY: geteuid has no failure.
        if unsafe { libc::geteuid() } != 0 {
            let readonly = dir.path().join("readonly-cgroup");
            std::fs::create_dir(&readonly).unwrap();
            std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o500)).unwrap();
            let ro_mount = format!(
                "41 25 0:30 / {} rw - cgroup2 cgroup2 rw",
                readonly.display()
            );
            assert_managed_fails(&ro_mount, "0::/");
            std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        // 4. Explicit leader mode succeeds without probing cgroups.
        let leader = Launcher::new_with(
            &state,
            Lifecycle::Leader,
            || Ok(NoImages),
            |_| panic!("leader lifecycle must not probe cgroups"),
        )
        .unwrap();
        assert!(!leader.has_cgroup_root());
    }
}

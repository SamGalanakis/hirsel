//! R27 containment: the primitive the launcher calls to run one child of a
//! `contained` authority group (docs/spec/launch.md "Containment (R27)",
//! "Child modes"; docs/spec/reserved.md R27).
//!
//! A [`Host`] owns one delegated cgroup v2 subtree. [`Host::open`] refuses a host
//! that cannot contain: one without a writable cgroup v2 subtree with the `cpu`,
//! `memory` and `pids` controllers, or one where an unprivileged process cannot
//! create a user, PID, mount, network and cgroup namespace, mount its own root
//! and pivot into it. It tries each mechanism rather than reading a setting.
//! No `Host` exists on such a host, so no contained child can be launched.
//!
//! Each launch gets its own container, named by the child's identity:
//! `<root>/<name>` carries the quota's limits and `<root>/<name>/run` holds every
//! process. The child starts as PID 1 of fresh namespaces, a small init that
//! forwards stop signals to the program and reports its wait status. The
//! program sees a read-only root built only from its mode's authority
//! ([`Authority::of`]): the directories it may write, the read-only paths the
//! launcher names (its image and loader), `/proc` of its own PID namespace and
//! the four character devices `null`, `zero`, `random` and `urandom`. Its
//! network namespace is empty, so it reaches no address; an inherited listener
//! still accepts. Its descendants, `Exec` tools included, inherit all of it and
//! cannot leave: the cgroup file system is not in its root, and a cgroup
//! namespace mounted later sees only `run`.
//!
//! A container ends when its init ends, which ends every process in its PID
//! namespace; [`Contained::wait`] then also kills the cgroup and returns only
//! once `cgroup.events` reports it unpopulated and the cgroup is removed.
//! [`Host::recover`], run by a new launcher before it answers `Hello`, does
//! the same for every container a lost launcher left.
//!
//! Not built: a `Net` binding cannot yet be enforced inside a container, so a
//! launch whose authority holds one is refused ([`Refused::Net`]).

use std::ffi::OsString;
use std::io;
use std::os::fd::{OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

#[cfg(test)]
#[path = "../../../tools/test_support/loopback.rs"]
#[allow(dead_code)]
mod loopback;
#[cfg(test)]
hirsel_guard::install!();

/// A child's mode: a row of launch.md "Child modes".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Mode {
    Install,
    Describe,
    Preview,
    Prepare,
    Serving,
}

impl Mode {
    pub const ALL: [Mode; 5] = [
        Mode::Install,
        Mode::Describe,
        Mode::Preview,
        Mode::Prepare,
        Mode::Serving,
    ];

    fn tag(self) -> &'static str {
        match self {
            Mode::Install => "install",
            Mode::Describe => "describe",
            Mode::Preview => "preview",
            Mode::Prepare => "prepare",
            Mode::Serving => "serving",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Mode::ALL.into_iter().find(|mode| mode.tag() == tag)
    }

    /// The descriptors above 2 the mode's row lets the child inherit: the
    /// serving child's listener (3), private endpoint (4) and wake report (5).
    /// Describe and preview answer on stdout. Prepare syncs PREPARED in its export.
    pub fn descriptors(self) -> &'static [RawFd] {
        match self {
            Mode::Install | Mode::Describe | Mode::Preview | Mode::Prepare => &[],
            Mode::Serving => &[3, 4, 5],
        }
    }
}

/// A child's identity (RUNNER-SPEC §14.1): `key` is the serving or install
/// child's `gen`, or the deploy attempt of a child that opens no store.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ChildId {
    pub group: String,
    pub incarnation: u64,
    pub key: u128,
    pub role: Mode,
}

impl ChildId {
    /// The container's directory name: `c-<hex group>-<incarnation>-<key>-<role>`.
    pub fn name(&self) -> String {
        let mut name = String::from("c-");
        for byte in self.group.as_bytes() {
            name.push_str(&format!("{byte:02x}"));
        }
        format!(
            "{name}-{}-{}-{}",
            self.incarnation,
            self.key,
            self.role.tag()
        )
    }

    pub fn parse(name: &str) -> Option<Self> {
        let mut parts = name.strip_prefix("c-")?.split('-');
        let hex = parts.next()?;
        let incarnation = parts.next()?.parse().ok()?;
        let key = parts.next()?.parse().ok()?;
        let role = Mode::from_tag(parts.next()?)?;
        if parts.next().is_some() || hex.len() % 2 != 0 {
            return None;
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        let id = Self {
            group: String::from_utf8(bytes).ok()?,
            incarnation,
            key,
            role,
        };
        (id.name() == name).then_some(id)
    }
}

/// A group's approved grant bindings, as containment needs them: the `Dir`
/// binding that names the store, the group's other `Dir` paths, and its `Net`
/// rule texts. Relative paths resolve against the launch's working directory.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    pub store: PathBuf,
    pub dirs: Vec<PathBuf>,
    pub net: Vec<String>,
}

/// A child's authority: the bindings intersected with its mode's row.
#[derive(Clone, Debug)]
pub struct Authority {
    mode: Mode,
    dirs: Vec<PathBuf>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    net: Vec<String>,
}

impl Authority {
    /// The row of launch.md "Child modes": install and serving have the
    /// store and the group's `Dir` bindings. Only serving has `Net` bindings.
    /// Describe and preview have no directories, and prepare has its export.
    pub fn of(mode: Mode, bindings: &Bindings, export: Option<&Path>) -> Result<Self, Refused> {
        if (mode == Mode::Prepare) != export.is_some() {
            return Err(Refused::Export);
        }
        let dirs = match mode {
            Mode::Describe | Mode::Preview => Vec::new(),
            Mode::Prepare => vec![export.unwrap().to_path_buf()],
            Mode::Install | Mode::Serving => {
                let mut dirs = vec![bindings.store.clone()];
                dirs.extend(bindings.dirs.iter().cloned());
                dirs
            }
        };
        let net = if mode == Mode::Serving {
            bindings.net.clone()
        } else {
            Vec::new()
        };
        Ok(Self { mode, dirs, net })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// The launcher owns this per-group directory. Serving exchanges exports there.
    pub fn with_serving_scratch(mut self, scratch: &Path) -> Self {
        if self.mode == Mode::Serving {
            self.dirs.push(scratch.to_path_buf());
        }
        self
    }
}

/// OS limits from the group's quota; `None` is the cgroup's `max`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Limits {
    pub memory: Option<u64>,
    /// CPU bandwidth as (quota, period) in microseconds.
    pub cpu: Option<(u64, u64)>,
    pub pids: Option<u64>,
}

/// One contained launch.
pub struct Launch<'a> {
    pub id: &'a ChildId,
    pub authority: &'a Authority,
    pub program: &'a Path,
    pub args: &'a [OsString],
    pub env: &'a [(OsString, OsString)],
    pub workdir: &'a Path,
    /// Paths the child may read and execute: whatever its image and its
    /// `Exec` tools need besides the image itself (a dynamic loader and its
    /// libraries). Each is visible at its own path.
    pub read_only: &'a [PathBuf],
    /// Descriptors the child inherits, by target number. 0, 1 and 2 are
    /// always allowed; others must be in the mode's [`Mode::descriptors`].
    /// A standard descriptor not given is closed.
    pub fds: Vec<(RawFd, OwnedFd)>,
    pub limits: Limits,
}

/// A launch refused before anything ran.
#[derive(Debug, Eq, PartialEq)]
pub enum Refused {
    /// A prepare child without its export directory, or another mode with one.
    Export,
    /// The identity's role is not the authority's mode.
    Role,
    /// The authority holds `Net` bindings, which this build cannot enforce
    /// inside a container (reserved.md R27: refused where the host cannot).
    Net,
    /// A descriptor the mode's row does not give, or one given twice.
    Descriptor(RawFd),
    /// A container of this identity already exists.
    Exists,
}

#[derive(Debug)]
pub enum LaunchError {
    Refused(Refused),
    Io(io::Error),
}

impl From<io::Error> for LaunchError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Refused> for LaunchError {
    fn from(refused: Refused) -> Self {
        Self::Refused(refused)
    }
}

/// Why a host cannot contain.
#[derive(Debug)]
pub enum Uncontainable {
    /// The root is not a cgroup v2 directory.
    NotCgroup2,
    /// The root's `cpu`, `memory` and `pids` controllers cannot be enabled for
    /// its children (not delegated, missing, or the root holds processes).
    Controllers(io::Error),
    /// A namespace or mount step of a trial container failed.
    Mechanism {
        step: &'static str,
        errno: i32,
    },
    Io(io::Error),
}

/// How a contained child ended.
#[derive(Debug)]
pub enum End {
    /// The program ended with this wait status.
    Exited(ExitStatus),
    /// Setup inside the container failed before the program ran.
    Setup { step: &'static str, errno: i32 },
    /// The container's init was killed before it reported.
    Killed,
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Contained, Host};
#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(not(target_os = "linux"))]
pub use unsupported::{Contained, Host};

#[cfg(all(test, feature = "contain", target_os = "linux"))]
mod contain;

#[cfg(all(test, target_os = "linux"))]
mod laws;

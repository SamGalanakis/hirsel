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

use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;

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
    /// serving child's listener (3), private endpoint (4) and wake report (5),
    /// and the prepare child's private endpoint (4). Describe and preview
    /// answer on standard output; install has none.
    pub fn descriptors(self) -> &'static [RawFd] {
        match self {
            Mode::Install | Mode::Describe | Mode::Preview => &[],
            Mode::Prepare => &[4],
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
    pub key: u64,
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
    net: Vec<String>,
}

impl Authority {
    /// The row of launch.md "Child modes": install has the store; describe and
    /// preview nothing; prepare the export directory alone, which only it
    /// takes; serving the store and the group's `Dir` and `Net` bindings.
    pub fn of(mode: Mode, bindings: &Bindings, export: Option<&Path>) -> Result<Self, Refused> {
        if (mode == Mode::Prepare) != export.is_some() {
            return Err(Refused::Export);
        }
        let (dirs, net) = match mode {
            Mode::Install => (vec![bindings.store.clone()], Vec::new()),
            Mode::Describe | Mode::Preview => (Vec::new(), Vec::new()),
            Mode::Prepare => (vec![export.unwrap().to_path_buf()], Vec::new()),
            Mode::Serving => {
                let mut dirs = vec![bindings.store.clone()];
                dirs.extend(bindings.dirs.iter().cloned());
                (dirs, bindings.net.clone())
            }
        };
        Ok(Self { mode, dirs, net })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
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

pub struct Host {
    root: PathBuf,
    mounts: PathBuf,
}

const CGROUP2_SUPER_MAGIC: i64 = 0x6367_7270;
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;
const P_PIDFD: libc::idtype_t = 3;
const PROBE: &str = "probe";
const DEVICES: [&str; 4] = ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"];

impl Host {
    /// Opens the delegated cgroup `root`, which must hold no process, with
    /// `state` as the launcher's own directory for mount points. Refuses a
    /// host that cannot contain.
    pub fn open(root: &Path, state: &Path) -> Result<Self, Uncontainable> {
        let cpath = cstring(root.as_os_str()).map_err(Uncontainable::Io)?;
        // SAFETY: statfs writes into a zeroed struct we own.
        let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(cpath.as_ptr(), &mut fs) } != 0 {
            return Err(Uncontainable::Io(io::Error::last_os_error()));
        }
        if fs.f_type as i64 != CGROUP2_SUPER_MAGIC {
            return Err(Uncontainable::NotCgroup2);
        }
        fs::write(root.join("cgroup.subtree_control"), "+cpu +memory +pids")
            .map_err(Uncontainable::Controllers)?;
        let mounts = state.join("roots");
        fs::create_dir_all(&mounts).map_err(Uncontainable::Io)?;
        let host = Self {
            root: root.to_path_buf(),
            mounts,
        };
        host.empty(PROBE).map_err(Uncontainable::Io)?;
        let plan = host
            .plan(PROBE, None, &[], &[], &[], Path::new("/"), &[], &[])
            .map_err(Uncontainable::Io)?;
        let probe = host
            .start(PROBE, &Limits::default(), plan)
            .map_err(Uncontainable::Io)?;
        match probe.wait().map_err(Uncontainable::Io)? {
            End::Exited(status) if status.success() => Ok(host),
            End::Setup { step, errno } => Err(Uncontainable::Mechanism { step, errno }),
            _ => Err(Uncontainable::Mechanism {
                step: "probe",
                errno: 0,
            }),
        }
    }

    /// Empties and removes every container a previous launcher left: kills
    /// every process in each, waits until each is unpopulated, and removes it.
    /// Returns the identities it ended. Entries that are not containers'
    /// names are left alone.
    pub fn recover(&self) -> io::Result<Vec<ChildId>> {
        let mut ended = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(id) = ChildId::parse(&name) else {
                continue;
            };
            if entry.file_type()?.is_dir() {
                self.empty(&name)?;
                ended.push(id);
            }
        }
        ended.sort();
        Ok(ended)
    }

    /// Starts one contained child.
    pub fn launch(&self, launch: Launch<'_>) -> Result<Contained, LaunchError> {
        let authority = launch.authority;
        if launch.id.role != authority.mode {
            return Err(Refused::Role.into());
        }
        if !authority.net.is_empty() {
            return Err(Refused::Net.into());
        }
        let mut seen = Vec::new();
        for (target, _) in &launch.fds {
            if seen.contains(target)
                || !(0..=2).contains(target) && !authority.mode.descriptors().contains(target)
            {
                return Err(Refused::Descriptor(*target).into());
            }
            seen.push(*target);
        }
        let name = launch.id.name();
        if self.root.join(&name).exists() {
            return Err(Refused::Exists.into());
        }
        let workdir = absolute(launch.workdir, Path::new("/"));
        let dirs: Vec<PathBuf> = authority
            .dirs
            .iter()
            .map(|dir| absolute(dir, &workdir))
            .collect();
        let mut read_only = vec![absolute(launch.program, &workdir)];
        read_only.extend(launch.read_only.iter().map(|path| absolute(path, &workdir)));
        let fds: Vec<(RawFd, RawFd)> = launch
            .fds
            .iter()
            .map(|(target, fd)| (fd.as_raw_fd(), *target))
            .collect();
        let plan = self.plan(
            &name,
            Some(launch.program),
            launch.args,
            launch.env,
            &dirs,
            &workdir,
            &read_only,
            &fds,
        )?;
        let contained = self.start(&name, &launch.limits, plan)?;
        drop(launch.fds);
        Ok(contained)
    }

    #[allow(clippy::too_many_arguments)]
    fn plan(
        &self,
        name: &str,
        program: Option<&Path>,
        args: &[OsString],
        env: &[(OsString, OsString)],
        writable: &[PathBuf],
        workdir: &Path,
        read_only: &[PathBuf],
        fds: &[(RawFd, RawFd)],
    ) -> io::Result<Plan> {
        let root = self.mounts.join(name);
        let mut layout = Layout::default();
        for dir in writable {
            layout.bind(dir, false)?;
        }
        for path in read_only {
            layout.bind(path, true)?;
        }
        for device in DEVICES {
            layout.bind(Path::new(device), false)?;
        }
        layout.dir(Path::new("/proc"));
        let workdir = layout.resolve(workdir)?;
        layout.dir(&workdir);
        // Parents before children, so an inner bind lands inside an outer one.
        layout
            .binds
            .sort_by_key(|bind| bind.target.components().count());
        let under = |path: &Path| -> io::Result<CString> {
            let mut full = root.clone().into_os_string();
            full.push(path.as_os_str());
            cstring(&full)
        };
        let mut plan = Plan {
            root: cstring(root.as_os_str())?,
            tmpfs_data: c"mode=0755".into(),
            maps: [
                format!("{0} {0} 1\n", unsafe { libc::getuid() }).into_bytes(),
                format!("{0} {0} 1\n", unsafe { libc::getgid() }).into_bytes(),
            ],
            dirs: Vec::new(),
            files: Vec::new(),
            binds: Vec::new(),
            links: Vec::new(),
            proc: under(Path::new("/proc"))?,
            workdir: cstring(workdir.as_os_str())?,
            program: None,
            fds: fds.to_vec(),
        };
        for dir in &layout.dirs {
            plan.dirs.push(under(dir)?);
        }
        for bind in &layout.binds {
            let target = under(&bind.target)?;
            if !bind.directory {
                plan.files.push(target.clone());
            }
            plan.binds.push(BindPlan {
                source: cstring(bind.target.as_os_str())?,
                target,
                remount: bind.read_only.then_some(bind.flags | libc::MS_RDONLY),
            });
        }
        for (link, target) in &layout.links {
            plan.links
                .push((cstring(target.as_os_str())?, under(link)?));
        }
        if let Some(program) = program {
            let mut argv = vec![cstring(program.as_os_str())?];
            for arg in args {
                argv.push(cstring(arg)?);
            }
            let mut envp = Vec::new();
            for (key, value) in env {
                let mut pair = key.clone();
                pair.push("=");
                pair.push(value);
                envp.push(cstring(&pair)?);
            }
            plan.program = Some(Exec::new(
                cstring(absolute(program, &workdir).as_os_str())?,
                argv,
                envp,
            ));
        }
        Ok(plan)
    }

    fn start(&self, name: &str, limits: &Limits, plan: Plan) -> io::Result<Contained> {
        let outer = self.root.join(name);
        fs::create_dir(&outer)?;
        let started = (|| {
            write_limit(&outer, "memory.max", limits.memory)?;
            if limits.memory.is_some() {
                fs::write(outer.join("memory.swap.max"), "0")?;
            }
            write_limit(&outer, "pids.max", limits.pids)?;
            if let Some((quota, period)) = limits.cpu {
                fs::write(outer.join("cpu.max"), format!("{quota} {period}"))?;
            }
            let run = outer.join("run");
            fs::create_dir(&run)?;
            let mount = self.mounts.join(name);
            fs::create_dir_all(&mount)?;
            let cgroup = File::open(&run)?;
            let mut pipe = [0; 2];
            // SAFETY: pipe2 fills two descriptors that we then own.
            if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both descriptors were just created and are owned here.
            let (read, write) =
                unsafe { (File::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
            let mut pidfd: RawFd = -1;
            let mut args = CloneArgs {
                flags: (libc::CLONE_NEWUSER
                    | libc::CLONE_NEWPID
                    | libc::CLONE_NEWNS
                    | libc::CLONE_NEWNET
                    | libc::CLONE_NEWCGROUP
                    | libc::CLONE_NEWIPC
                    | libc::CLONE_NEWUTS
                    | libc::CLONE_PIDFD) as u64
                    | CLONE_INTO_CGROUP,
                pidfd: &mut pidfd as *mut RawFd as u64,
                exit_signal: libc::SIGCHLD as u64,
                cgroup: cgroup.as_raw_fd() as u64,
                ..CloneArgs::default()
            };
            // SAFETY: clone3 without CLONE_VM forks this process. The child runs
            // only `init`, which makes raw system calls on memory prepared here.
            let pid = unsafe {
                libc::syscall(
                    libc::SYS_clone3,
                    &mut args as *mut CloneArgs,
                    std::mem::size_of::<CloneArgs>(),
                )
            };
            if pid < 0 {
                return Err(io::Error::last_os_error());
            }
            if pid == 0 {
                init(&plan, write.as_raw_fd());
            }
            drop(write);
            // SAFETY: CLONE_PIDFD stored a new descriptor for the child.
            let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
            Ok(Contained {
                outer: outer.clone(),
                mount,
                pidfd,
                status: read,
                pid: pid as i32,
            })
        })();
        if started.is_err() {
            let _ = self.empty(name);
        }
        started
    }

    /// Kills, waits out and removes the container `name`, if it exists.
    fn empty(&self, name: &str) -> io::Result<()> {
        empty(&self.root.join(name))?;
        match fs::remove_dir(self.mounts.join(name)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// A running contained child.
pub struct Contained {
    outer: PathBuf,
    mount: PathBuf,
    pidfd: OwnedFd,
    status: File,
    pid: i32,
}

impl Contained {
    /// The host's PID of the container's init.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// The container's cgroup directory.
    pub fn cgroup(&self) -> &Path {
        &self.outer
    }

    /// Sends `signal` to the program; the container's init forwards `SIGTERM`,
    /// `SIGINT`, `SIGHUP`, `SIGQUIT`, `SIGUSR1` and `SIGUSR2`.
    pub fn signal(&self, signal: i32) -> io::Result<()> {
        // SAFETY: pidfd_send_signal on a pidfd we own, with no siginfo.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if rc != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Kills every process in the container at once.
    pub fn kill(&self) -> io::Result<()> {
        kill(&self.outer)
    }

    /// Waits until the child ends, then ends every process left in its
    /// container, and returns only once the container is empty and removed.
    pub fn wait(mut self) -> io::Result<End> {
        loop {
            // SAFETY: waitid on our own child's pidfd into a zeroed siginfo.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    P_PIDFD,
                    self.pidfd.as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED,
                )
            };
            if rc == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        let mut report = Vec::new();
        self.status.read_to_end(&mut report)?;
        empty(&self.outer)?;
        match fs::remove_dir(&self.mount) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        Ok(match report.as_slice() {
            [b'S', a, b, c, d] => {
                End::Exited(ExitStatus::from_raw(i32::from_ne_bytes([*a, *b, *c, *d])))
            }
            [b'E', step, a, b, c, d] => End::Setup {
                step: STEPS.get(*step as usize).copied().unwrap_or("setup"),
                errno: i32::from_ne_bytes([*a, *b, *c, *d]),
            },
            _ => End::Killed,
        })
    }
}

fn kill(outer: &Path) -> io::Result<()> {
    match fs::write(outer.join("cgroup.kill"), "1") {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Kills every process under `outer`, waits for `populated 0`, and removes
/// `outer` and every cgroup beneath it.
fn empty(outer: &Path) -> io::Result<()> {
    if !outer.exists() {
        return Ok(());
    }
    kill(outer)?;
    let events = File::open(outer.join("cgroup.events"))?;
    loop {
        let text = fs::read_to_string(outer.join("cgroup.events"))?;
        if text.lines().any(|line| line == "populated 0") {
            break;
        }
        let mut poll = libc::pollfd {
            fd: events.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        };
        // SAFETY: poll on one descriptor we own; a change of cgroup.events
        // wakes it, and the timeout re-reads in case a change was missed.
        unsafe { libc::poll(&mut poll, 1, 100) };
    }
    remove_cgroups(outer)
}

fn remove_cgroups(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_cgroups(&entry.path())?;
        }
    }
    fs::remove_dir(dir)
}

fn write_limit(outer: &Path, file: &str, limit: Option<u64>) -> io::Result<()> {
    match limit {
        Some(value) => fs::write(outer.join(file), value.to_string()),
        None => Ok(()),
    }
}

fn cstring(text: &OsStr) -> io::Result<CString> {
    CString::new(text.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn absolute(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The new root's shape: directories to make, binds and symbolic links, each
/// at the path the host gives it.
#[derive(Default)]
struct Layout {
    dirs: Vec<PathBuf>,
    binds: Vec<Bind>,
    links: Vec<(PathBuf, PathBuf)>,
}

struct Bind {
    target: PathBuf,
    directory: bool,
    read_only: bool,
    flags: libc::c_ulong,
}

impl Layout {
    fn dir(&mut self, path: &Path) {
        let mut prefix = PathBuf::from("/");
        for component in path.components().skip(1) {
            prefix.push(component);
            if !self.dirs.contains(&prefix) {
                self.dirs.push(prefix.clone());
            }
        }
    }

    /// Resolves `path` on the host, recording each symbolic link on the way so
    /// that the path also works inside, and returns the resolved path.
    fn resolve(&mut self, path: &Path) -> io::Result<PathBuf> {
        let mut resolved = PathBuf::from("/");
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Prefix(_) => return Err(io::ErrorKind::InvalidInput.into()),
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(part) => {
                    let next = resolved.join(part);
                    match fs::symlink_metadata(&next) {
                        Ok(meta) if meta.file_type().is_symlink() => {
                            let target = fs::read_link(&next)?;
                            let real = fs::canonicalize(&next)?;
                            self.dir(&resolved);
                            if !self.links.iter().any(|(link, _)| *link == next) {
                                self.links.push((next, target));
                            }
                            // Resolve the target too, so its own links appear.
                            resolved = self.resolve(&real)?;
                        }
                        _ => resolved = next,
                    }
                }
            }
        }
        Ok(resolved)
    }

    fn bind(&mut self, path: &Path, read_only: bool) -> io::Result<()> {
        let target = self.resolve(path)?;
        let meta = fs::metadata(&target)?;
        let source = cstring(target.as_os_str())?;
        // SAFETY: statvfs writes into a zeroed struct we own.
        let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(source.as_ptr(), &mut vfs) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // A remount in a user namespace must keep the flags the mount locked.
        let mut flags = libc::MS_BIND | libc::MS_REMOUNT;
        for (st, ms) in [
            (libc::ST_NOSUID, libc::MS_NOSUID),
            (libc::ST_NODEV, libc::MS_NODEV),
            (libc::ST_NOEXEC, libc::MS_NOEXEC),
            (libc::ST_NOATIME, libc::MS_NOATIME),
            (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
            (libc::ST_RELATIME, libc::MS_RELATIME),
        ] {
            if vfs.f_flag & st != 0 {
                flags |= ms;
            }
        }
        if meta.is_dir() {
            self.dir(&target);
        } else if let Some(parent) = target.parent() {
            self.dir(parent);
        }
        if let Some(bind) = self.binds.iter_mut().find(|bind| bind.target == target) {
            bind.read_only &= read_only;
            return Ok(());
        }
        self.binds.push(Bind {
            target,
            directory: meta.is_dir(),
            read_only,
            flags,
        });
        Ok(())
    }
}

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

struct BindPlan {
    source: CString,
    target: CString,
    remount: Option<libc::c_ulong>,
}

struct Exec {
    path: CString,
    _strings: (Vec<CString>, Vec<CString>),
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
}

impl Exec {
    fn new(path: CString, argv: Vec<CString>, envp: Vec<CString>) -> Self {
        let pointers = |strings: &[CString]| {
            strings
                .iter()
                .map(|s| s.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect()
        };
        Self {
            path,
            argv: pointers(&argv),
            envp: pointers(&envp),
            _strings: (argv, envp),
        }
    }
}

/// Everything the container's init needs, prepared before the clone so that
/// init makes system calls only.
struct Plan {
    root: CString,
    tmpfs_data: CString,
    maps: [Vec<u8>; 2],
    dirs: Vec<CString>,
    files: Vec<CString>,
    binds: Vec<BindPlan>,
    links: Vec<(CString, CString)>,
    proc: CString,
    workdir: CString,
    program: Option<Exec>,
    fds: Vec<(RawFd, RawFd)>,
}

const STEPS: [&str; 16] = [
    "setgroups",
    "uid_map",
    "gid_map",
    "private mounts",
    "root tmpfs",
    "mkdir",
    "bind target",
    "bind",
    "read-only remount",
    "symlink",
    "proc",
    "pivot_root",
    "read-only root",
    "workdir",
    "no_new_privs",
    "exec",
];

static PROGRAM: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
const FORWARDED: [i32; 6] = [
    libc::SIGTERM,
    libc::SIGINT,
    libc::SIGHUP,
    libc::SIGQUIT,
    libc::SIGUSR1,
    libc::SIGUSR2,
];

extern "C" fn forward(signal: libc::c_int) {
    let pid = PROGRAM.load(std::sync::atomic::Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, signal) };
    }
}

fn report(fd: RawFd, bytes: &[u8]) {
    // SAFETY: write is async-signal-safe; a short or failed write leaves the
    // report incomplete, which the launcher reads as `Killed`.
    unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
}

fn fail(fd: RawFd, step: usize) -> ! {
    // SAFETY: __errno_location is the calling thread's errno.
    let errno = unsafe { *libc::__errno_location() };
    let e = errno.to_ne_bytes();
    report(fd, &[b'E', step as u8, e[0], e[1], e[2], e[3]]);
    // SAFETY: _exit ends the process without running destructors.
    unsafe { libc::_exit(127) }
}

fn write_file(path: &std::ffi::CStr, bytes: &[u8]) -> bool {
    // SAFETY: plain open/write/close on a path and a buffer we own.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return false;
        }
        let ok = libc::write(fd, bytes.as_ptr().cast(), bytes.len()) == bytes.len() as isize;
        libc::close(fd);
        ok
    }
}

/// The container's PID 1. Runs in the forked child, so it makes only raw
/// system calls on memory `plan` prepared, and never returns.
fn init(plan: &Plan, status: RawFd) -> ! {
    let null = std::ptr::null::<libc::c_char>();
    // SAFETY: each call below is a system call on prepared NUL-terminated
    // strings and buffers; nothing allocates or takes a lock.
    unsafe {
        if !write_file(c"/proc/self/setgroups", b"deny") {
            fail(status, 0);
        }
        if !write_file(c"/proc/self/uid_map", &plan.maps[0]) {
            fail(status, 1);
        }
        if !write_file(c"/proc/self/gid_map", &plan.maps[1]) {
            fail(status, 2);
        }
        if libc::mount(
            null,
            c"/".as_ptr(),
            null,
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        ) != 0
        {
            fail(status, 3);
        }
        if libc::mount(
            c"tmpfs".as_ptr(),
            plan.root.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            plan.tmpfs_data.as_ptr().cast(),
        ) != 0
        {
            fail(status, 4);
        }
        for dir in &plan.dirs {
            if libc::mkdir(dir.as_ptr(), 0o755) != 0 && *libc::__errno_location() != libc::EEXIST {
                fail(status, 5);
            }
        }
        for file in &plan.files {
            let fd = libc::open(
                file.as_ptr(),
                libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                0o644,
            );
            if fd < 0 {
                fail(status, 6);
            }
            libc::close(fd);
        }
        for bind in &plan.binds {
            if libc::mount(
                bind.source.as_ptr(),
                bind.target.as_ptr(),
                null,
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            ) != 0
            {
                fail(status, 7);
            }
            if let Some(flags) = bind.remount
                && libc::mount(null, bind.target.as_ptr(), null, flags, std::ptr::null()) != 0
            {
                fail(status, 8);
            }
        }
        for (target, link) in &plan.links {
            if libc::symlink(target.as_ptr(), link.as_ptr()) != 0
                && *libc::__errno_location() != libc::EEXIST
            {
                fail(status, 9);
            }
        }
        if libc::mount(
            c"proc".as_ptr(),
            plan.proc.as_ptr(),
            c"proc".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        ) != 0
        {
            fail(status, 10);
        }
        if libc::chdir(plan.root.as_ptr()) != 0
            || libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".".as_ptr()) != 0
            || libc::umount2(c".".as_ptr(), libc::MNT_DETACH) != 0
            || libc::chdir(c"/".as_ptr()) != 0
        {
            fail(status, 11);
        }
        if libc::mount(
            null,
            c"/".as_ptr(),
            null,
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        ) != 0
        {
            fail(status, 12);
        }
        if libc::chdir(plan.workdir.as_ptr()) != 0 {
            fail(status, 13);
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            fail(status, 14);
        }
        let Some(exec) = &plan.program else {
            report(status, &[b'S', 0, 0, 0, 0]);
            libc::_exit(0);
        };
        // Move every source above the targets, so no dup2 clobbers a source a
        // later one needs, and the status pipe to 6. Targets are distinct and
        // at most 0 to 5, so six slots suffice.
        let mut moved = [(0, 0); 6];
        for (i, (source, target)) in plan.fds.iter().enumerate() {
            let high = libc::fcntl(*source, libc::F_DUPFD_CLOEXEC, 64);
            if high < 0 {
                fail(status, 15);
            }
            moved[i] = (high, *target);
        }
        if libc::dup3(status, 6, libc::O_CLOEXEC) < 0 {
            fail(status, 15);
        }
        let status = 6;
        // Init then keeps only the moved sources and its status pipe: every
        // other descriptor the clone copied from the launcher, its control
        // socket among them, is closed, and init is not dumpable, so the
        // program cannot take one through /proc/1/fd.
        let mut fd = 0;
        while fd < 64 {
            if fd != status {
                libc::close(fd);
            }
            fd += 1;
        }
        let top = moved[..plan.fds.len()]
            .iter()
            .map(|(high, _)| *high)
            .max()
            .unwrap_or(63);
        libc::syscall(libc::SYS_close_range, (top + 1) as u32, u32::MAX, 0u32);
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        let mut args = CloneArgs {
            exit_signal: libc::SIGCHLD as u64,
            ..CloneArgs::default()
        };
        let pid = libc::syscall(
            libc::SYS_clone3,
            &mut args as *mut CloneArgs,
            std::mem::size_of::<CloneArgs>(),
        );
        if pid < 0 {
            fail(status, 15);
        }
        if pid == 0 {
            for (high, target) in &moved[..plan.fds.len()] {
                if libc::dup2(*high, *target) < 0 {
                    fail(status, 15);
                }
            }
            libc::syscall(libc::SYS_close_range, 64u32, u32::MAX, 0u32);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            for signal in 1..32 {
                libc::signal(signal, libc::SIG_DFL);
            }
            libc::execve(exec.path.as_ptr(), exec.argv.as_ptr(), exec.envp.as_ptr());
            fail(status, 15);
        }
        PROGRAM.store(pid as i32, std::sync::atomic::Ordering::Relaxed);
        libc::syscall(libc::SYS_close_range, 64u32, u32::MAX, 0u32);
        for signal in FORWARDED {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = forward as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
        loop {
            let mut wait_status = 0;
            let reaped = libc::waitpid(-1, &mut wait_status, 0);
            if reaped == pid as i32 {
                let s = wait_status.to_ne_bytes();
                report(status, &[b'S', s[0], s[1], s[2], s[3]]);
                libc::_exit(0);
            }
            if reaped < 0 && *libc::__errno_location() == libc::ECHILD {
                libc::_exit(1);
            }
        }
    }
}

#[cfg(test)]
mod laws;
